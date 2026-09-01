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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// A compact block header (112 bytes on the wire, similar to Bitcoin plus UTXO commitment).
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
    /// Nonce (PoW) or stake modifier (PoS).
    pub nonce: u32,
    /// Block height (not part of the 80-byte header but stored for convenience).
    pub height: u32,
}

impl SpvHeader {
    /// Compute the double-SHA256 hash of this header.
    pub fn hash(&self) -> [u8; 32] {
        let mut buf = Vec::with_capacity(112);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.prev_hash);
        buf.extend_from_slice(&self.merkle_root);
        buf.extend_from_slice(&self.utxo_root);
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.bits.to_le_bytes());
        buf.extend_from_slice(&self.nonce.to_le_bytes());
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
            height: 1,
        };
        let mut h2 = h1.clone();
        h2.utxo_root = [4u8; 32];
        assert_ne!(h1.hash(), h2.hash());
    }
}
