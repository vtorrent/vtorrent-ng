//! Lightweight SPV chain that stores only block headers (80 bytes each).
//!
//! An SPV client downloads only block headers and validates the chain of
//! hashes without executing transactions or maintaining a UTXO set. This
//! reduces storage from ~GB to ~MB even for a long chain.
//!
//! # Validation Rules
//! - Each header's `prev_hash` must match the hash of the previous header
//! - The chain must be monotonically increasing in height
//! - The Merkle root in the header is used to verify transaction inclusion proofs

use crate::error::{Result, SpvError};
use crate::merkle::MerkleProof;
use crate::stake::{
    check_stake_kernel, compute_pos_reward, compute_stake_modifier, hash_utxo, StakeProof,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// A compact block header (120 bytes on the wire, similar to Bitcoin plus UTXO commitment + stake modifier).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpvHeader {
    /// Block version.
    pub version: u32,
    /// Hash of the previous block header.
    pub prev_hash: [u8; 32],
    /// Merkle root of all transactions in the block.
    pub merkle_root: [u8; 32],
    /// UTXO set commitment (Merkle root over sorted UTXO leaves after this block).
    pub utxo_root: [u8; 32],
    /// Block timestamp (Unix seconds).
    pub timestamp: u32,
    /// Compact target / difficulty bits.
    pub bits: u32,
    /// Nonce (PoW) — 0 for PoS blocks.
    pub nonce: u32,
    /// For PoS blocks: the stake modifier.
    pub stake_modifier: u64,
    /// Block height (not part of the wire header but stored for convenience).
    pub height: u32,
}

impl SpvHeader {
    /// Compute the double-SHA256 hash of this header (mirrors BlockHeader::hash via bincode, but explicit).
    pub fn hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(120);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.prev_hash);
        buf.extend_from_slice(&self.merkle_root);
        buf.extend_from_slice(&self.utxo_root);
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.bits.to_le_bytes());
        buf.extend_from_slice(&self.nonce.to_le_bytes());
        buf.extend_from_slice(&self.stake_modifier.to_le_bytes());
        let first = Sha256::digest(&buf);
        Sha256::digest(first).into()
    }

    /// Returns true if this is a PoS block, matching `vtorrent-node`.
    pub fn is_pos(&self) -> bool {
        self.nonce == 0
    }
}

/// Compute the work contributed by a single header from its compact `bits`.
///
/// Work is `2^256 / target`, the standard Bitcoin-style measure. A header with
/// a smaller target (higher difficulty) contributes more work. The result is
/// saturated to `u128::MAX` for absurdly small targets that would otherwise
/// overflow; such headers can only be produced by genuine extreme PoW because
/// [`hash_meets_target`] gates admission.
fn header_work(bits: u32) -> u128 {
    let exponent = bits >> 24;
    let mantissa = (bits & 0x00ff_ffff) as u128;
    if mantissa == 0 {
        return 0;
    }
    // target = mantissa * 2^(8*(exponent-3)), so
    // work = 2^256 / target = 2^(280 - 8*exponent) / mantissa.
    let shift = 280u32.saturating_sub(8 * exponent);
    if shift >= 128 {
        return u128::MAX;
    }
    (1u128 << shift) / mantissa
}

/// Check that a header hash meets the compact-target difficulty in `bits`.
///
/// Both the hash and the target are compared as little-endian 256-bit numbers
/// (Bitcoin convention: byte 0 is the least significant).
pub fn hash_meets_target(hash: &[u8; 32], bits: u32) -> bool {
    let exponent = (bits >> 24) as usize;
    let mantissa = bits & 0x00ff_ffff;
    if exponent == 0 || mantissa == 0 {
        return false;
    }
    // Build the target as a little-endian byte array.
    let mut target = [0u8; 32];
    if exponent <= 3 {
        let val = mantissa >> (8 * (3 - exponent));
        if val == 0 {
            return false;
        }
        target[0] = val as u8;
        target[1] = (val >> 8) as u8;
        target[2] = (val >> 16) as u8;
    } else {
        let low_zeros = exponent - 3;
        if low_zeros + 3 > 32 {
            // Target ≥ 2^256: every hash trivially meets it.
            return true;
        }
        let mb = mantissa.to_le_bytes();
        target[low_zeros] = mb[0];
        target[low_zeros + 1] = mb[1];
        target[low_zeros + 2] = mb[2];
    }
    // Compare hash ≤ target as little-endian numbers: scan from the most
    // significant byte down.
    for i in (0..32).rev() {
        match hash[i].cmp(&target[i]) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => continue,
        }
    }
    true
}

fn verify_p2pkh_signature(
    coinstake: &crate::stake::Transaction,
    utxo: &crate::stake::SpvUtxo,
) -> Result<()> {
    use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1};
    let script_sig = &coinstake.inputs[0].script_sig;
    if script_sig.len() < 2 {
        return Err(SpvError::HeaderValidation("script_sig too short".into()));
    }
    let len_sig = script_sig[0] as usize;
    if script_sig.len() < 1 + len_sig + 1 {
        return Err(SpvError::HeaderValidation(
            "script_sig truncated sig".into(),
        ));
    }
    let sig_bytes = &script_sig[1..1 + len_sig];
    if sig_bytes.is_empty() || sig_bytes[sig_bytes.len() - 1] != 0x01 {
        return Err(SpvError::HeaderValidation("missing SIGHASH_ALL".into()));
    }
    let der = &sig_bytes[..sig_bytes.len() - 1];
    let len_pk = script_sig[1 + len_sig] as usize;
    if script_sig.len() != 1 + len_sig + 1 + len_pk {
        return Err(SpvError::HeaderValidation("script_sig extra bytes".into()));
    }
    let pk_bytes = &script_sig[1 + len_sig + 1..];
    if pk_bytes.len() != len_pk {
        return Err(SpvError::HeaderValidation("pubkey length mismatch".into()));
    }
    let sighash = coinstake.sighash(0, &utxo.script_pubkey);
    let msg = Message::from_digest(sighash);
    let sig = Signature::from_der(der)
        .map_err(|e| SpvError::HeaderValidation(format!("bad DER sig: {}", e)))?;
    let pk = PublicKey::from_slice(pk_bytes)
        .map_err(|e| SpvError::HeaderValidation(format!("bad pubkey: {}", e)))?;
    let secp = Secp256k1::verification_only();
    secp.verify_ecdsa(&msg, &sig, &pk)
        .map_err(|e| SpvError::HeaderValidation(format!("sig verify failed: {}", e)))?;
    // Verify P2PKH scriptPubKey matches pubkey hash160
    if utxo.script_pubkey.len() != 25
        || utxo.script_pubkey[0] != 0x76
        || utxo.script_pubkey[1] != 0xa9
        || utxo.script_pubkey[2] != 0x14
        || utxo.script_pubkey[23] != 0x88
        || utxo.script_pubkey[24] != 0xac
    {
        return Err(SpvError::HeaderValidation("utxo script not P2PKH".into()));
    }
    let expected_hash = vtorrent_core::crypto::hash160(pk_bytes);
    if expected_hash != utxo.script_pubkey[3..23] {
        return Err(SpvError::HeaderValidation("pubkey hash160 mismatch".into()));
    }
    Ok(())
}

/// A lightweight chain of block headers for SPV verification.
#[derive(Debug, Default)]
pub struct SpvChain {
    /// Headers indexed by their hash.
    headers: HashMap<[u8; 32], SpvHeader>,
    /// Cumulative chain work (sum of per-header work) indexed by header hash.
    ///
    /// The best tip is selected by accumulated work rather than raw height so
    /// a peer cannot steer the client onto a high-height but low-work fork.
    work: HashMap<[u8; 32], u128>,
    /// Best (highest-work) chain tip hash.
    best_hash: Option<[u8; 32]>,
    /// Best chain height.
    best_height: u32,
}

impl SpvChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a block header to the chain.
    ///
    /// Validates that the header connects to the existing chain.
    pub fn add_header(&mut self, header: SpvHeader) -> Result<()> {
        if header.is_pos() {
            return Err(SpvError::HeaderValidation(
                "PoS headers require full-block stake validation".into(),
            ));
        }
        self.add_header_inner(header, false)
    }

    /// Add a header already validated by the local full node.
    pub fn add_trusted_header(&mut self, header: SpvHeader) -> Result<()> {
        self.add_header_inner(header, true)
    }

    /// Validate the legacy stake-proof fields, then fail closed because this
    /// proof format cannot authenticate the header's post-state commitment.
    pub fn add_pos_header(&mut self, header: SpvHeader, proof: StakeProof) -> Result<()> {
        if !header.is_pos() {
            return Err(SpvError::HeaderValidation(
                "expected PoS header (nonce 0)".into(),
            ));
        }
        if header.height == 0 {
            return Err(SpvError::HeaderValidation("PoS genesis not allowed".into()));
        }
        let parent = self
            .headers
            .get(&header.prev_hash)
            .ok_or_else(|| SpvError::UnknownParent(hex::encode(header.prev_hash)))?;

        if header.height != parent.height + 1 {
            return Err(SpvError::HeightMismatch {
                expected: parent.height + 1,
                got: header.height,
            });
        }
        if header.bits != parent.bits {
            return Err(SpvError::HeaderValidation(format!(
                "header difficulty {} does not match parent difficulty {}",
                header.bits, parent.bits
            )));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if header.timestamp > now + 7200 {
            return Err(SpvError::HeaderValidation(format!(
                "header timestamp {} too far in the future",
                header.timestamp
            )));
        }
        if header.timestamp <= parent.timestamp {
            return Err(SpvError::HeaderValidation(
                "header timestamp must exceed parent timestamp".into(),
            ));
        }

        let expected_modifier =
            compute_stake_modifier(proof.prev_stake_modifier, &header.prev_hash);
        if header.stake_modifier != expected_modifier {
            return Err(SpvError::HeaderValidation(format!(
                "stake modifier {} != expected {}",
                header.stake_modifier, expected_modifier
            )));
        }
        if proof.prev_stake_modifier != parent.stake_modifier {
            return Err(SpvError::HeaderValidation(format!(
                "prev_stake_modifier {} != parent {}",
                proof.prev_stake_modifier, parent.stake_modifier
            )));
        }

        if !proof.coinstake.is_coinstake() {
            return Err(SpvError::HeaderValidation(
                "coinstake must be Coinstake".into(),
            ));
        }
        if proof.coinstake.inputs.len() != 1 {
            return Err(SpvError::HeaderValidation(
                "coinstake must have 1 input".into(),
            ));
        }
        if proof.coinstake.outputs.is_empty()
            || proof.coinstake.outputs[0].value != 0
            || !proof.coinstake.outputs[0].script_pubkey.is_empty()
        {
            return Err(SpvError::HeaderValidation(
                "coinstake first output must be empty marker".into(),
            ));
        }
        if proof.coinstake.lock_time != header.height {
            return Err(SpvError::HeaderValidation(format!(
                "coinstake lock_time {} != height {}",
                proof.coinstake.lock_time, header.height
            )));
        }

        proof
            .tx_merkle_proof
            .verify(&header.merkle_root)
            .map_err(|e| SpvError::HeaderValidation(format!("tx merkle proof: {}", e)))?;
        if proof.tx_merkle_proof.txid != proof.coinstake.txid() {
            return Err(SpvError::HeaderValidation(
                "tx merkle proof txid mismatch".into(),
            ));
        }
        if proof.tx_merkle_proof.index != 0 {
            return Err(SpvError::HeaderValidation(
                "coinstake must be index 0".into(),
            ));
        }

        let leaf = hash_utxo(&proof.utxo);
        proof
            .utxo_proof
            .verify(&parent.utxo_root, &leaf)
            .map_err(|e| SpvError::HeaderValidation(format!("utxo proof: {}", e)))?;
        let inp = &proof.coinstake.inputs[0];
        if inp.prev_txid != proof.utxo.txid || inp.prev_vout != proof.utxo.vout {
            return Err(SpvError::HeaderValidation(
                "coinstake outpoint != utxo".into(),
            ));
        }

        const COIN: u64 = 100_000_000;
        const MIN_STAKE_AMOUNT: u64 = COIN;
        const MIN_STAKE_AGE: u64 = 6 * 60 * 60;
        const MAX_STAKE_AGE: u64 = 6 * 24 * 60 * 60;
        if proof.utxo.value < MIN_STAKE_AMOUNT {
            return Err(SpvError::HeaderValidation(format!(
                "stake value {} below minimum {}",
                proof.utxo.value, MIN_STAKE_AMOUNT
            )));
        }
        let age = header.timestamp.saturating_sub(proof.utxo.timestamp);
        if (age as u64) < MIN_STAKE_AGE || (age as u64) > MAX_STAKE_AGE {
            return Err(SpvError::HeaderValidation(format!(
                "stake age {} outside {}..={}",
                age, MIN_STAKE_AGE, MAX_STAKE_AGE
            )));
        }

        if !check_stake_kernel(
            proof.prev_stake_modifier,
            proof.utxo.value,
            &proof.utxo.txid,
            proof.utxo.vout,
            header.timestamp,
        ) {
            return Err(SpvError::HeaderValidation(
                "kernel hash does not meet stake target".into(),
            ));
        }

        verify_p2pkh_signature(&proof.coinstake, &proof.utxo)?;

        let minted = proof
            .coinstake
            .total_output()
            .saturating_sub(proof.utxo.value);
        let max_reward = compute_pos_reward(proof.utxo.value, age as u64);
        if minted > max_reward {
            return Err(SpvError::HeaderValidation(format!(
                "minted {} exceeds max reward {}",
                minted, max_reward
            )));
        }

        Err(SpvError::HeaderValidation(
            "stake proof authenticates the parent UTXO but not the committed post-state; use add_trusted_header until state-transition proofs are implemented".into(),
        ))
    }

    fn add_header_inner(&mut self, header: SpvHeader, trusted: bool) -> Result<()> {
        let hash = header.hash();

        // Check for duplicate
        if self.headers.contains_key(&hash) {
            return Ok(());
        }

        // Validate chain linkage (skip for genesis block at height 0)
        if header.height > 0
            && !self.headers.contains_key(&header.prev_hash)
            && !(trusted && self.headers.is_empty())
        {
            return Err(SpvError::UnknownParent(hex::encode(header.prev_hash)));
        }

        // Proof-of-work validation for non-PoS headers: the hash must meet
        // the target encoded in `bits`. Without this a malicious peer could
        // fabricate a high-work fork out of thin air (each header claiming
        // an easy target yet contributing huge claimed work).
        if !trusted && !hash_meets_target(&hash, header.bits) {
            return Err(SpvError::HeaderValidation(format!(
                "hash {} does not meet target {:08x}",
                hex::encode(hash),
                header.bits
            )));
        }

        // Timestamp sanity: reject headers far in the future and enforce
        // monotonicity against the parent. Without this, timestamp games can
        // influence difficulty-independent tip selection.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if header.timestamp > now + 7200 {
            return Err(SpvError::HeaderValidation(format!(
                "header timestamp {} too far in the future",
                header.timestamp
            )));
        }
        if header.height > 0 && self.headers.contains_key(&header.prev_hash) {
            let parent = &self.headers[&header.prev_hash];
            if header.timestamp <= parent.timestamp {
                return Err(SpvError::HeaderValidation(
                    "header timestamp must exceed parent timestamp".into(),
                ));
            }
        }

        // The height must be exactly one more than the parent's height.
        if header.height > 0 && self.headers.contains_key(&header.prev_hash) {
            let parent = &self.headers[&header.prev_hash];
            if header.height != parent.height + 1 {
                return Err(SpvError::HeightMismatch {
                    expected: parent.height + 1,
                    got: header.height,
                });
            }
        }

        let height = header.height;
        // Cumulative work = parent's cumulative work + this header's work.
        let parent_work = if height == 0 || !self.headers.contains_key(&header.prev_hash) {
            0
        } else {
            self.work[&header.prev_hash]
        };
        let cumulative = parent_work.saturating_add(header_work(header.bits));
        self.headers.insert(hash, header);
        self.work.insert(hash, cumulative);

        // Update best tip if this chain has the most accumulated work.
        if self.best_hash.is_none() || cumulative > self.work[&self.best_hash.unwrap()] {
            self.best_height = height;
            self.best_hash = Some(hash);
        }

        Ok(())
    }

    /// Add multiple headers in sequence (e.g., from a `headers` P2P message).
    /// Add multiple headers in sequence (e.g., from a `headers` P2P message).
    ///
    /// Stops at the first invalid header and returns how many were accepted —
    /// the prefix that validated stays committed, so a peer sending a valid
    /// prefix followed by garbage still contributes the valid part (and the
    /// caller can re-request from the new tip instead of restarting sync).
    pub fn add_headers(&mut self, headers: Vec<SpvHeader>) -> Result<usize> {
        let mut added = 0;
        for h in headers {
            match self.add_header(h) {
                Ok(()) => added += 1,
                Err(e) => {
                    if added == 0 {
                        return Err(e);
                    }
                    tracing::debug!("SPV: stopped after {} valid headers: {}", added, e);
                    break;
                }
            }
        }
        Ok(added)
    }

    /// Returns the best chain height.
    pub fn best_height(&self) -> u32 {
        self.best_height
    }

    /// Returns the best chain tip hash.
    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.best_hash
    }

    /// Look up a header by its hash.
    pub fn get_header(&self, hash: &[u8; 32]) -> Option<&SpvHeader> {
        self.headers.get(hash)
    }

    /// Returns the number of headers stored.
    pub fn len(&self) -> usize {
        self.headers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Verify a Merkle inclusion proof against the header at the given block hash.
    ///
    /// Returns `Ok(())` if the transaction is proven to be in the block.
    pub fn verify_tx_inclusion(&self, block_hash: &[u8; 32], proof: &MerkleProof) -> Result<()> {
        let header = self
            .headers
            .get(block_hash)
            .ok_or_else(|| SpvError::UnknownParent(hex::encode(block_hash)))?;

        proof.verify(&header.merkle_root)
    }

    /// Build a `getheaders` locator from the current chain tip.
    ///
    /// Returns a list of block hashes from the tip backwards (exponentially
    /// spaced) for use in a P2P `getheaders` message.
    pub fn get_locator(&self) -> Vec<[u8; 32]> {
        let mut locator = Vec::new();
        let mut current = self.best_hash;
        let mut step = 1usize;
        let mut count = 0usize;

        while let Some(hash) = current {
            locator.push(hash);
            count += 1;

            if count > 10 {
                step *= 2;
            }

            // Walk back `step` headers
            let header = self.headers.get(&hash);
            current = header.map(|h| h.prev_hash).filter(|h| h != &[0u8; 32]);

            // Skip `step - 1` headers
            for _ in 1..step {
                current = current
                    .and_then(|h| self.headers.get(&h).map(|hdr| hdr.prev_hash))
                    .filter(|h| h != &[0u8; 32]);
            }
        }

        locator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header(height: u32, prev: [u8; 32], merkle: [u8; 32]) -> SpvHeader {
        // Maximum (easiest) target; mine a nonce that satisfies PoW (~2 tries
        // on average). Production sync validates real difficulty.
        let bits = 0x207f_ffff;
        let mut nonce: u32 = 1;
        loop {
            let h = SpvHeader {
                version: 1,
                prev_hash: prev,
                merkle_root: merkle,
                utxo_root: [0u8; 32],
                timestamp: 1_700_000_000 + height,
                bits,
                nonce,
                stake_modifier: 0,
                height,
            };
            if hash_meets_target(&h.hash(), bits) {
                return h;
            }
            nonce += 1;
        }
    }

    #[test]
    fn test_pow_invalid_header_rejected() {
        let mut chain = SpvChain::new();
        // A non-PoS header whose hash cannot meet a hard target is rejected.
        let mut h = make_header(0, [0u8; 32], [1u8; 32]);
        h.bits = 0x1b_00_00_01;
        assert!(chain.add_header(h).is_err());
    }

    #[test]
    fn test_add_genesis() {
        let mut chain = SpvChain::new();
        let genesis = make_header(0, [0u8; 32], [1u8; 32]);
        chain.add_header(genesis).unwrap();
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }

    #[test]
    fn test_chain_of_headers() {
        let mut chain = SpvChain::new();
        let h0 = make_header(0, [0u8; 32], [1u8; 32]);
        let h0_hash = h0.hash();
        chain.add_header(h0).unwrap();

        let h1 = make_header(1, h0_hash, [2u8; 32]);
        let h1_hash = h1.hash();
        chain.add_header(h1).unwrap();

        let h2 = make_header(2, h1_hash, [3u8; 32]);
        chain.add_header(h2).unwrap();

        assert_eq!(chain.best_height(), 2);
        assert_eq!(chain.len(), 3);
    }

    #[test]
    fn test_unknown_parent_rejected() {
        let mut chain = SpvChain::new();
        let orphan = make_header(1, [0xffu8; 32], [0u8; 32]);
        assert!(chain.add_header(orphan).is_err());
    }

    #[test]
    fn test_duplicate_header_ignored() {
        let mut chain = SpvChain::new();
        let h = make_header(0, [0u8; 32], [1u8; 32]);
        chain.add_header(h.clone()).unwrap();
        chain.add_header(h).unwrap(); // should not error
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_verify_tx_inclusion() {
        use crate::merkle::MerkleTree;

        let txids: Vec<[u8; 32]> = (0..4u8)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i;
                h
            })
            .collect();
        let tree = MerkleTree::build(&txids);
        let merkle_root = tree.root();

        let mut chain = SpvChain::new();
        let header = make_header(0, [0u8; 32], merkle_root);
        let block_hash = header.hash();
        chain.add_header(header).unwrap();

        let proof = tree.proof(2).unwrap();
        chain.verify_tx_inclusion(&block_hash, &proof).unwrap();
    }

    #[test]
    fn test_locator_single_header() {
        let mut chain = SpvChain::new();
        let h = make_header(0, [0u8; 32], [0u8; 32]);
        chain.add_header(h).unwrap();
        let locator = chain.get_locator();
        assert_eq!(locator.len(), 1);
    }

    #[test]
    fn test_header_hash_deterministic() {
        let h = make_header(5, [0xabu8; 32], [0xcdu8; 32]);
        assert_eq!(h.hash(), h.hash());
    }

    #[test]
    fn test_spv_header_hash_includes_utxo_root() {
        let h1 = SpvHeader {
            version: 2,
            prev_hash: [1u8; 32],
            merkle_root: [2u8; 32],
            utxo_root: [3u8; 32],
            timestamp: 1_700_000_001,
            bits: 0x1e0fffff,
            nonce: 0,
            stake_modifier: 0,
            height: 1,
        };
        let mut h2 = h1.clone();
        h2.utxo_root = [4u8; 32];
        assert_ne!(h1.hash(), h2.hash());
    }
}

#[cfg(test)]
mod pos_tests {
    use super::*;
    use crate::merkle::MerkleTree;
    use crate::stake::{
        check_stake_kernel, compute_stake_modifier, hash_utxo, SpvUtxo, StakeProof, Transaction,
        TxInput, TxOutput, TxType, UtxoInclusionProof,
    };
    use secp256k1::{Message, Secp256k1};
    use vtorrent_core::crypto::hash160;

    const COIN: u64 = 100_000_000;

    struct TestKey {
        secret: secp256k1::SecretKey,
        pubkey: secp256k1::PublicKey,
        script_pubkey: Vec<u8>,
    }

    fn make_key(seed: u8) -> TestKey {
        let mut sk = [0u8; 32];
        sk[30] = 0xab;
        sk[31] = seed;
        let secret = secp256k1::SecretKey::from_slice(&sk).unwrap();
        let secp = Secp256k1::new();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret);
        let h = hash160(&pubkey.serialize());
        let mut script_pubkey = vec![0x76, 0xa9, 0x14];
        script_pubkey.extend_from_slice(&h);
        script_pubkey.extend_from_slice(&[0x88, 0xac]);
        TestKey {
            secret,
            pubkey,
            script_pubkey,
        }
    }

    /// Sign the coinstake input exactly like `StakingEngine::sign_coinstake_input`:
    /// `script_sig = <len> <der_sig + 0x01> <len> <compressed_pubkey>`.
    fn sign_coinstake(tx: &Transaction, utxo: &SpvUtxo, key: &TestKey) -> Vec<u8> {
        let sighash = tx.sighash(0, &utxo.script_pubkey);
        let secp = Secp256k1::new();
        let sig = secp.sign_ecdsa(&Message::from_digest(sighash), &key.secret);
        let mut der = sig.serialize_der().to_vec();
        der.push(0x01);
        let pk = key.pubkey.serialize();
        let mut script = Vec::with_capacity(1 + der.len() + 1 + pk.len());
        script.push(der.len() as u8);
        script.extend_from_slice(&der);
        script.push(pk.len() as u8);
        script.extend_from_slice(&pk);
        script
    }

    fn now_ts() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as u32
    }

    struct PosFixture {
        genesis: SpvHeader,
        header: SpvHeader,
        proof: StakeProof,
    }

    /// Build a genesis header committing to a single-UTXO set plus a PoS child
    /// header with a fully valid `StakeProof` (real secp256k1 signature,
    /// kernel-searched timestamp). `young` makes the staked UTXO younger than
    /// MIN_STAKE_AGE for the age-violation test.
    fn make_fixture(staked_value: u64, reward: u64, young: bool) -> PosFixture {
        let key = make_key(7);
        let now = now_ts();
        let utxo_timestamp = if young { now - 100_000 } else { now - 200_000 };
        let utxo = SpvUtxo {
            txid: [0x42u8; 32],
            vout: 0,
            value: staked_value,
            script_pubkey: key.script_pubkey.clone(),
            height: 0,
            timestamp: utxo_timestamp,
        };

        // Genesis header commits to the pre-apply UTXO set (single leaf:
        // tree root == hash_utxo).
        let utxo_root = hash_utxo(&utxo);
        let genesis = SpvHeader {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: [1u8; 32],
            utxo_root,
            timestamp: utxo_timestamp,
            bits: 0x207f_ffff,
            nonce: 1,
            stake_modifier: 0,
            height: 0,
        };
        let genesis_hash = genesis.hash();

        // Search for a kernel-passing timestamp (target = value / 1000).
        // Kernel uses prev_stake_modifier (0 for genesis child), not the new header's modifier.
        let prev_modifier = 0u64;
        let modifier = compute_stake_modifier(prev_modifier, &genesis_hash);
        let search_start = if young {
            utxo_timestamp + 3600
        } else {
            now - 100_000
        };
        let mut header_timestamp = search_start;
        while !check_stake_kernel(
            prev_modifier,
            staked_value,
            &utxo.txid,
            utxo.vout,
            header_timestamp,
        ) {
            header_timestamp += 1;
            // safety bound: if we scan far, eventually kernel hits (value large)
            if header_timestamp > search_start + 1_000_000 {
                panic!("kernel search exhausted");
            }
        }

        // Coinstake spending the UTXO, minting `reward` on top.
        let mut coinstake = Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: Vec::new(),
                sequence: 0xffff_ffff,
            }],
            outputs: vec![
                TxOutput {
                    value: 0,
                    script_pubkey: Vec::new(),
                },
                TxOutput {
                    value: staked_value + reward,
                    script_pubkey: key.script_pubkey.clone(),
                },
            ],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        coinstake.inputs[0].script_sig = sign_coinstake(&coinstake, &utxo, &key);

        // Tx merkle proof: coinstake at index 0 plus a dummy second tx.
        let txids = vec![coinstake.txid(), [0x99u8; 32]];
        let tx_tree = MerkleTree::build(&txids);
        let tx_merkle_proof = tx_tree.proof(0).unwrap();

        let utxo_proof = UtxoInclusionProof {
            leaf_index: 0,
            siblings: Vec::new(),
            root: utxo_root,
        };

        let header = SpvHeader {
            version: 2,
            prev_hash: genesis_hash,
            merkle_root: tx_tree.root(),
            utxo_root: [7u8; 32],
            timestamp: header_timestamp,
            bits: genesis.bits,
            nonce: 0,
            stake_modifier: modifier,
            height: 1,
        };

        let proof = StakeProof {
            coinstake,
            tx_merkle_proof,
            utxo,
            utxo_proof,
            prev_stake_modifier: 0,
        };

        PosFixture {
            genesis,
            header,
            proof,
        }
    }

    fn seeded_chain(genesis: &SpvHeader) -> SpvChain {
        let mut chain = SpvChain::new();
        chain.add_trusted_header(genesis.clone()).unwrap();
        chain
    }

    #[test]
    fn test_legacy_stake_proof_fails_closed_without_transition_proof() {
        let f = make_fixture(500 * COIN, 1_000, false);
        let mut chain = seeded_chain(&f.genesis);
        let error = chain.add_pos_header(f.header, f.proof).unwrap_err();
        assert!(error.to_string().contains("post-state"));
        assert_eq!(chain.best_height(), 0);
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn test_add_pos_header_bad_kernel_rejected() {
        let mut f = make_fixture(500 * COIN, 1_000, false);
        f.proof.prev_stake_modifier ^= 1;
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_add_pos_header_bad_utxo_proof_rejected() {
        let mut f = make_fixture(500 * COIN, 1_000, false);
        f.proof.utxo_proof.root = [0xffu8; 32];
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_add_pos_header_bad_signature_rejected() {
        let mut f = make_fixture(500 * COIN, 1_000, false);
        // Corrupt a byte inside the DER signature (after the 0x30 tag).
        f.proof.coinstake.inputs[0].script_sig[5] ^= 0xff;
        // Rebuild the tx merkle proof so inclusion passes and the failure
        // isolates the signature check.
        let txids = vec![f.proof.coinstake.txid(), [0x99u8; 32]];
        let tree = MerkleTree::build(&txids);
        f.proof.tx_merkle_proof = tree.proof(0).unwrap();
        f.header.merkle_root = tree.root();
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_add_pos_header_reward_excess_rejected() {
        let f = make_fixture(500 * COIN, 50 * COIN, false);
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_add_pos_header_age_violation_rejected() {
        let f = make_fixture(1000 * COIN, 1_000, true);
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_forged_utxo_value_tampered_rejected() {
        let mut f = make_fixture(500 * COIN, 1_000, false);
        // inflate UTXO value: leaf hash will no longer match proof
        f.proof.utxo.value += 100 * COIN;
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
    }

    #[test]
    fn test_utxo_root_forgery_fails_closed_immediately() {
        let mut f = make_fixture(500 * COIN, 1_000, false);
        f.header.utxo_root = [0xaa; 32];
        let mut chain = seeded_chain(&f.genesis);
        assert!(chain.add_pos_header(f.header, f.proof).is_err());
        assert_eq!(chain.best_height(), 0);
        assert_eq!(chain.len(), 1);
    }
}
