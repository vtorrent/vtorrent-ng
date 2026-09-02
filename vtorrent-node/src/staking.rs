/// PoS Staking Engine for vTorrent 2.0
///
/// Implements Proof-of-Stake block production:
/// - Selects eligible UTXOs (age >= MIN_STAKE_AGE, amount >= MIN_STAKE_AMOUNT)
/// - Computes the stake kernel hash (simplified: SHA256d of utxo_txid + timestamp)
/// - Builds a coinstake transaction with the stake reward
/// - Assembles a complete PoS block ready to be added to the chain
///
/// The stake kernel check is a simplified version of the PPCoin protocol.
/// A full implementation would use the stake modifier from the chain.
use crate::{
    block::{
        compute_merkle_root_from_txids, compute_utxo_root_sorted, Block, BlockHeader, Transaction,
        TxInput, TxOutput, TxType,
    },
    chain::Utxo,
    consensus::{
        check_stake_kernel, compute_pos_reward, compute_stake_modifier, stake_kernel_hash,
        MAX_STAKE_AGE, MIN_STAKE_AGE, MIN_STAKE_AMOUNT,
    },
};
use secp256k1::{All, Secp256k1};
use std::sync::LazyLock;
use vtorrent_script::{classify_script, Script, ScriptType};
use vtorrent_spv::merkle::MerkleTree as ProofMerkleTree;
use vtorrent_spv::stake::{
    hash_utxo, SpvUtxo, StakeProof, Transaction as SpvTransaction, TxInput as SpvTxInput,
    TxOutput as SpvTxOutput, TxType as SpvTxType, UtxoInclusionProof,
};

/// Runtime staking control command sent from RPC/tauri to the node.
#[derive(Debug, Clone)]
pub enum StakingCommand {
    /// Start staking with the given address and optional signing WIF.
    Start {
        address: String,
        wif: Option<String>,
    },
    /// Stop staking.
    Stop,
}

/// The staking engine.
pub struct StakingEngine {
    /// The address whose UTXOs are used for staking.
    pub address: String,
    /// The WIF-encoded private key used to sign the coinstake input.
    pub wif: Option<String>,
    /// Minimum coin age in seconds for UTXO eligibility.
    pub min_stake_age: u64,
    /// Maximum coin age in seconds for UTXO eligibility.
    pub max_stake_age: u64,
}

impl StakingEngine {
    /// Create a new staking engine for the given address.
    pub fn new(address: String) -> Self {
        Self {
            address,
            wif: None,
            min_stake_age: MIN_STAKE_AGE,
            max_stake_age: MAX_STAKE_AGE,
        }
    }

    /// Create a new staking engine with a signing key.
    pub fn with_wif(address: String, wif: String) -> Self {
        Self {
            address,
            wif: Some(wif),
            min_stake_age: MIN_STAKE_AGE,
            max_stake_age: MAX_STAKE_AGE,
        }
    }

    /// Create a fast staking engine for regtest soak testing.
    ///
    /// Lowers min stake age to 60s and max stake age to 3600s (1 hour) so
    /// blocks are produced rapidly with few UTXOs.
    pub fn new_fast(address: String) -> Self {
        Self {
            address,
            wif: None,
            min_stake_age: 60,
            max_stake_age: u64::MAX,
        }
    }

    /// Create a fast staking engine with a signing key for regtest.
    pub fn with_wif_fast(address: String, wif: String) -> Self {
        Self {
            address,
            wif: Some(wif),
            min_stake_age: 60,
            max_stake_age: u64::MAX,
        }
    }

    /// Try to build a valid PoS block from available UTXOs.
    ///
    /// Returns `Some(block)` if a valid stake kernel was found, `None` otherwise.
    pub fn build_stake_block(
        &self,
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        height: u32,
        timestamp: u32,
        utxos: Vec<Utxo>,
        pending_txs: Vec<Transaction>,
    ) -> Option<Block> {
        self.build_stake_block_with_proof(
            prev_hash,
            prev_stake_modifier,
            height,
            timestamp,
            utxos,
            pending_txs,
        )
        .map(|(block, _)| block)
    }

    /// Try to build a valid PoS block and emit a self-contained [`StakeProof`].
    ///
    /// The proof commits to the staked UTXO's inclusion in the pre-block UTXO
    /// set (`prev_header.utxo_root`) and the coinstake's inclusion in the block
    /// (`header.merkle_root`), letting SPV light clients verify PoS blocks
    /// without a full UTXO set.
    ///
    /// Returns `Some((block, proof))` if a valid stake kernel was found.
    pub fn build_stake_block_with_proof(
        &self,
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        height: u32,
        timestamp: u32,
        utxos: Vec<Utxo>,
        pending_txs: Vec<Transaction>,
    ) -> Option<(Block, StakeProof)> {
        // Sort UTXOs by (txid, vout) — canonical leaf order for the UTXO tree.
        let mut sorted_utxos = utxos;
        sorted_utxos.sort_by(|a, b| a.txid.cmp(&b.txid).then(a.vout.cmp(&b.vout)));

        // Filter eligible UTXOs: staking candidates must be spendable — an
        // unspendable output (e.g. a genesis OP_RETURN legacy-distribution
        // leaf) would win the kernel race on raw value yet produce a
        // coinstake the chain rejects.
        let eligible: Vec<&Utxo> = sorted_utxos
            .iter()
            .filter(|u| self.is_eligible(u, timestamp) && is_spendable(u))
            .collect();
        for u in &sorted_utxos {
            if !self.is_eligible(u, timestamp) {
                let age = timestamp.saturating_sub(u.timestamp);
                tracing::debug!(
                    "UTXO {}:{} value={} REJECTED: age={}s (min {} max {})",
                    hex::encode(u.txid),
                    u.vout,
                    u.value,
                    age,
                    self.min_stake_age,
                    self.max_stake_age
                );
            }
        }

        if eligible.is_empty() {
            tracing::debug!("No eligible UTXOs for staking at height {}", height);
            return None;
        }

        // Build the pre-block UTXO commitment tree (leaves in canonical order).
        let spv_utxos: Vec<SpvUtxo> = sorted_utxos
            .iter()
            .map(|u| SpvUtxo {
                txid: u.txid,
                vout: u.vout,
                value: u.value,
                script_pubkey: u.script_pubkey.clone(),
                height: u.height,
                timestamp: u.timestamp,
            })
            .collect();
        let utxo_leaves: Vec<[u8; 32]> = spv_utxos.iter().map(hash_utxo).collect();
        let utxo_tree = ProofMerkleTree::build(&utxo_leaves);

        // Try each eligible UTXO as a stake kernel.  Move pending_txs on
        // the first (and only) success — the function returns immediately.
        for utxo in &eligible {
            if let Some(coinstake) =
                self.try_stake_kernel(prev_stake_modifier, utxo, timestamp, height)
            {
                // Drop pending txs whose inputs collide with the coinstake's
                // stake input (or with each other). The mempool admits txs
                // against the current UTXO set, but the coinstake consumes a
                // UTXO inside this same block — a stale mempool tx spending
                // the staked outpoint would otherwise make the block invalid
                // and permanently wedge staking.
                let stake_outpoint = (coinstake.inputs[0].prev_txid, coinstake.inputs[0].prev_vout);
                let mut seen: std::collections::HashSet<([u8; 32], u32)> =
                    std::collections::HashSet::new();
                seen.insert(stake_outpoint);
                let non_conflicting: Vec<Transaction> = pending_txs
                    .into_iter()
                    .filter(|tx| {
                        let conflict = tx.inputs.iter().any(|i| !seen.insert((i.prev_txid, i.prev_vout)));
                        if conflict {
                            tracing::debug!(
                                "Excluding mempool tx {} from block template: input conflicts with coinstake or earlier tx",
                                hex::encode(tx.txid())
                            );
                        }
                        !conflict
                    })
                    .collect();
                let mut block = self.assemble_block(
                    prev_hash,
                    prev_stake_modifier,
                    timestamp,
                    coinstake.clone(),
                    non_conflicting,
                );

                // Commit to the post-apply UTXO set. The chain journals every
                // tx in the block — the staked UTXO removed, every output
                // (including the zero-value coinstake marker) added, and each
                // pending tx's inputs removed and outputs added in block
                // order. The producer must reproduce that exactly or the
                // commitment root (and therefore the block hash) diverges
                // from the stored canonical hash.
                let staked_key = (utxo.txid, utxo.vout);
                let mut post_apply: Vec<Utxo> = sorted_utxos
                    .iter()
                    .filter(|u| (u.txid, u.vout) != staked_key)
                    .cloned()
                    .collect();
                let block_height = block.height();
                let block_ts = timestamp;
                // Chain order: coinstake first, then pending txs. Mirror the
                // journal: remove each tx's inputs that exist in the working
                // set, then add every output as a UTXO.
                for tx in std::iter::once(&coinstake).chain(block.transactions[1..].iter()) {
                    let txid = tx.txid();
                    for input in &tx.inputs {
                        post_apply
                            .retain(|u| (u.txid, u.vout) != (input.prev_txid, input.prev_vout));
                    }
                    for (vout, out) in tx.outputs.iter().enumerate() {
                        post_apply.push(Utxo {
                            txid,
                            vout: vout as u32,
                            value: out.value,
                            script_pubkey: out.script_pubkey.clone(),
                            height: block_height,
                            timestamp: block_ts,
                        });
                    }
                }
                block.header.utxo_root = compute_utxo_root_sorted(&post_apply);

                // UTXO inclusion proof for the staked UTXO against the
                // pre-block root (canonical leaf position).
                let leaf_index = sorted_utxos
                    .iter()
                    .position(|u| u.txid == utxo.txid && u.vout == utxo.vout)
                    .expect("winning utxo came from the sorted list");
                let utxo_proof_mp = utxo_tree
                    .proof(leaf_index)
                    .expect("winning utxo leaf index is in-bounds");
                let utxo_proof = UtxoInclusionProof {
                    leaf_index,
                    siblings: utxo_proof_mp.siblings,
                    root: utxo_tree.root(),
                };

                // Transaction Merkle proof: coinstake is always index 0.
                let txids: Vec<[u8; 32]> = block.transactions.iter().map(|tx| tx.txid()).collect();
                let tx_tree = ProofMerkleTree::build(&txids);
                let tx_merkle_proof = tx_tree
                    .proof(0)
                    .expect("block has at least the coinstake tx");

                let proof = StakeProof {
                    coinstake: spv_transaction_mirror(&coinstake),
                    tx_merkle_proof,
                    utxo: SpvUtxo {
                        txid: utxo.txid,
                        vout: utxo.vout,
                        value: utxo.value,
                        script_pubkey: utxo.script_pubkey.clone(),
                        height: utxo.height,
                        timestamp: utxo.timestamp,
                    },
                    utxo_proof,
                    prev_stake_modifier,
                };

                let block_hash = block.hash();
                tracing::info!(
                    height = %height,
                    stake_utxo = %format!("{}:{}", hex::encode(utxo.txid), utxo.vout),
                    stake_value = %utxo.value,
                    timestamp = %timestamp,
                    block_hash = %hex::encode(block_hash),
                    tx_count = %block.transactions.len(),
                    "Successfully staked block"
                );
                return Some((block, proof));
            }
        }

        None
    }

    /// Check if a UTXO is eligible for staking.
    fn is_eligible(&self, utxo: &Utxo, current_timestamp: u32) -> bool {
        // Must meet minimum amount
        if utxo.value < MIN_STAKE_AMOUNT {
            return false;
        }

        // Must have minimum coin age
        let coin_age_seconds = current_timestamp.saturating_sub(utxo.timestamp);
        if (coin_age_seconds as u64) < self.min_stake_age {
            return false;
        }

        // Must not exceed maximum coin age (prevents very old coins from
        // dominating the staking weight indefinitely).
        if (coin_age_seconds as u64) > self.max_stake_age {
            return false;
        }

        true
    }

    /// Try to find a valid stake kernel for a UTXO.
    ///
    /// The stake kernel hash must be below the target (proportional to stake
    /// amount), using the tip's stake modifier.
    fn try_stake_kernel(
        &self,
        stake_modifier: u64,
        utxo: &Utxo,
        timestamp: u32,
        height: u32,
    ) -> Option<Transaction> {
        // The kernel check is shared with the chain's block validation so a
        // block that passes validation provably met the difficulty requirement.
        if !check_stake_kernel(stake_modifier, utxo, timestamp) {
            let kernel_hash = stake_kernel_hash(stake_modifier, utxo, timestamp);
            let kv = u32::from_le_bytes([
                kernel_hash[0],
                kernel_hash[1],
                kernel_hash[2],
                kernel_hash[3],
            ]);
            tracing::debug!(
                "Kernel miss: value={} target={} kernel_val={} modifier={}",
                utxo.value,
                (utxo.value / 1000).min(u32::MAX as u64),
                kv,
                stake_modifier
            );
            return None;
        }

        // Compute coin age and stake reward
        let coin_age_seconds = timestamp.saturating_sub(utxo.timestamp);
        let reward = compute_pos_reward(utxo.value, coin_age_seconds as u64);

        // Build coinstake transaction
        // Input: the staked UTXO
        let stake_input = TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: Vec::new(), // filled in below after signing
            sequence: u32::MAX,
        };

        // Output 0: empty marker (PPCoin protocol: first output of coinstake is empty)
        let marker_output = TxOutput {
            value: 0,
            script_pubkey: Vec::new(),
        };

        // Output 1: stake return + reward to staking address
        let stake_script = self.address_to_script(&self.address)?;
        let stake_output = TxOutput {
            value: utxo.value.saturating_add(reward),
            script_pubkey: stake_script,
        };

        let mut coinstake = Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![stake_input],
            outputs: vec![marker_output, stake_output],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };

        // Sign the coinstake input over the same sighash the chain verifies.
        // Without a valid signature the block would be rejected by the chain's
        // script verification.
        if let Some(wif) = &self.wif {
            let script_sig = self.sign_coinstake_input(&coinstake, utxo, wif)?;
            coinstake.inputs[0].script_sig = script_sig;
        } else {
            tracing::warn!(
                "Staking engine has no signing key; coinstake would be rejected by the chain"
            );
        }

        Some(coinstake)
    }

    /// Sign the coinstake input with the staking key over the P2PKH sighash.
    fn sign_coinstake_input(
        &self,
        coinstake: &Transaction,
        utxo: &Utxo,
        wif: &str,
    ) -> Option<Vec<u8>> {
        use secp256k1::{Message, PublicKey, SecretKey};

        let key = vtorrent_core::keys::PrivateKey::from_wif(wif).ok()?;
        let secret_key = SecretKey::from_slice(key.as_bytes()).ok()?;
        let secp = &*SECP_CTX;
        let pubkey = PublicKey::from_secret_key(secp, &secret_key);

        // The subscript is the previous output's scriptPubKey (P2PKH).
        let sighash = coinstake.sighash(0, &utxo.script_pubkey);
        let message = Message::from_digest(sighash);
        let sig = secp.sign_ecdsa(&message, &secret_key);
        let mut der = sig.serialize_der().to_vec();
        der.push(0x01); // SIGHASH_ALL

        // Build P2PKH scriptSig: <sig> <pubkey>
        let pubkey_bytes = pubkey.serialize();
        if der.len() > 255 || pubkey_bytes.len() > 255 {
            return None;
        }
        let mut script = Vec::with_capacity(1 + der.len() + 1 + pubkey_bytes.len());
        script.push(der.len() as u8);
        script.extend_from_slice(&der);
        script.push(pubkey_bytes.len() as u8);
        script.extend_from_slice(&pubkey_bytes);
        Some(script)
    }

    /// Assemble a complete PoS block.
    fn assemble_block(
        &self,
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        timestamp: u32,
        coinstake: Transaction,
        pending_txs: Vec<Transaction>,
    ) -> Block {
        // PoS blocks have nonce = 0 (identified by BlockHeader::is_pos())
        let mut transactions = vec![coinstake];

        // Include pending transactions from mempool (up to block size limit)
        let mut block_size = 0usize;
        const MAX_BLOCK_SIZE: usize = 1_000_000;

        for tx in pending_txs {
            let tx_size = tx.inputs.len() * 148 + tx.outputs.len() * 34 + 10;
            if block_size + tx_size > MAX_BLOCK_SIZE {
                break;
            }
            transactions.push(tx);
            block_size += tx_size;
        }

        let merkle_root = {
            let mut txids: Vec<[u8; 32]> = transactions.iter().map(|tx| tx.txid()).collect();
            compute_merkle_root_from_txids(&mut txids)
        };

        let header = BlockHeader {
            version: 2,
            prev_block_hash: prev_hash,
            merkle_root,
            utxo_root: [0u8; 32],
            timestamp,
            bits: 0x1e0fffff,
            nonce: 0,
            stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
        };

        Block {
            header,
            transactions,
        }
    }

    /// Convert a vTorrent address to a P2PKH scriptPubKey.
    /// Decodes the Base58Check address and builds the standard script.
    /// Returns `None` if the address is invalid (avoids silent fund burn).
    fn address_to_script(&self, address: &str) -> Option<Vec<u8>> {
        let addr = vtorrent_core::address::validate_p2pkh(address).ok()?;
        Some(vtorrent_core::address::p2pkh_script_pubkey(&addr.hash))
    }
}

/// Shared signing context — `Secp256k1::new()` re-seeds the global RNG on
/// every construction, so the coinstake hot path reuses a single instance.
static SECP_CTX: LazyLock<Secp256k1<All>> = LazyLock::new(Secp256k1::new);

/// A UTXO is a staking candidate only if its script can actually be spent.
///
/// Unspendable outputs (genesis OP_RETURN legacy-distribution leaves) hold
/// enormous value and would dominate the kernel race, but a coinstake
/// spending one is rejected by script verification.
fn is_spendable(utxo: &Utxo) -> bool {
    match Script::from_bytes(utxo.script_pubkey.clone()) {
        Ok(script) => classify_script(&script) != ScriptType::OpReturn,
        Err(_) => false,
    }
}

/// Mirror a node `Transaction` into the bit-identical SPV `Transaction`.
fn spv_transaction_mirror(tx: &Transaction) -> SpvTransaction {
    SpvTransaction {
        version: tx.version,
        tx_type: match tx.tx_type {
            TxType::Standard => SpvTxType::Standard,
            TxType::Coinbase => SpvTxType::Coinbase,
            TxType::Coinstake => SpvTxType::Coinstake,
            TxType::LegacyClaim => SpvTxType::LegacyClaim,
            TxType::AtomicSwap => SpvTxType::AtomicSwap,
            TxType::TorrentIncentive => SpvTxType::TorrentIncentive,
        },
        inputs: tx
            .inputs
            .iter()
            .map(|i| SpvTxInput {
                prev_txid: i.prev_txid,
                prev_vout: i.prev_vout,
                script_sig: i.script_sig.clone(),
                sequence: i.sequence,
            })
            .collect(),
        outputs: tx
            .outputs
            .iter()
            .map(|o| SpvTxOutput {
                value: o.value,
                script_pubkey: o.script_pubkey.clone(),
            })
            .collect(),
        lock_time: tx.lock_time,
        claim_address: tx.claim_address.clone(),
        claim_signature: tx.claim_signature.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::Utxo, consensus::COIN};

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    #[test]
    fn test_eligible_utxo() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE as u32).saturating_add(3600));
        assert!(engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_young() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE / 2) as u32);
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_small() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(
            MIN_STAKE_AMOUNT / 2,
            (MIN_STAKE_AGE as u32).saturating_add(3600),
        );
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_address_to_script_length() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        // A valid P2PKH script is always 25 bytes
        let script = engine
            .address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT")
            .expect("valid address");
        assert_eq!(script.len(), 25);
    }

    #[test]
    fn test_address_to_script_p2pkh() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let script = engine
            .address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT")
            .expect("valid address");
        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
    }
}

#[cfg(test)]
mod proof_tests {
    use super::*;
    use crate::block::compute_utxo_root_sorted;
    use crate::chain::Utxo;
    use crate::consensus::{COIN, MIN_STAKE_AGE};
    use vtorrent_spv::stake::{hash_utxo, StakeProof};

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    /// build_stake_block_with_proof must emit a StakeProof whose tx Merkle
    /// proof verifies against the block header, whose UTXO inclusion proof
    /// verifies against the pre-block UTXO root, and which carries the
    /// staked UTXO and previous stake modifier.
    #[test]
    fn test_build_stake_block_produces_verifiable_proof() {
        let engine = StakingEngine::with_wif_fast(
            "VMLVUkkn4hJ6Pex3w9RdmdU4BRUarszhHH".to_string(),
            "WHqoPmvQULJ4ePseyRWP8XdzuwC67p49gSPuWTxJ5tyYPugbYLn4".to_string(),
        );
        let utxo = make_utxo(1000 * COIN, MIN_STAKE_AGE as u32 + 100);
        let prev_modifier = 0xdead_beef_u64;

        let mut result: Option<(Block, StakeProof)> = None;
        for ts in 1_700_000_000..1_700_000_000 + 3600 {
            result = engine.build_stake_block_with_proof(
                [2u8; 32],
                prev_modifier,
                101,
                ts,
                vec![utxo.clone()],
                vec![],
            );
            if result.is_some() {
                break;
            }
        }
        let (block, proof) = result.expect("kernel should hit within an hour of timestamps");

        // tx inclusion: coinstake at index 0 proven against header merkle root
        assert!(proof
            .tx_merkle_proof
            .verify(&block.header.merkle_root)
            .is_ok());

        // utxo inclusion: staked UTXO proven against pre-block utxo root
        let utxos = vec![utxo.clone()];
        let prev_root = compute_utxo_root_sorted(&utxos);
        assert!(proof
            .utxo_proof
            .verify(&prev_root, &hash_utxo(&proof.utxo))
            .is_ok());

        // proof carries the staked UTXO and previous modifier
        assert_eq!(proof.utxo.value, utxo.value);
        assert_eq!(proof.prev_stake_modifier, prev_modifier);

        // coinstake spends the staked outpoint
        assert_eq!(proof.coinstake.inputs[0].prev_txid, utxo.txid);
        assert_eq!(proof.coinstake.inputs[0].prev_vout, utxo.vout);
    }

    /// The producer's post-apply utxo_root must match what the chain journals
    /// on add_block — otherwise stored headers and block hashes diverge.
    #[test]
    fn test_producer_utxo_root_matches_chain_journal() {
        use crate::chain::Chain;
        use secp256k1::{PublicKey, Secp256k1, SecretKey};

        let secp_ctx = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 42;
        let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
        let wif = key.to_wif(198);
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp_ctx, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        let funding_ts = 1_700_000_001u32;
        let script = vtorrent_core::address::validate_p2pkh(&address)
            .map(|a| vtorrent_core::address::p2pkh_script_pubkey(&a.hash))
            .expect("valid address");
        let funding_block = {
            use crate::block::{TxInput, TxOutput, TxType};
            let transactions = vec![Transaction {
                version: 1,
                tx_type: TxType::Coinbase,
                inputs: vec![TxInput {
                    prev_txid: [0u8; 32],
                    prev_vout: 0xffffffff,
                    script_sig: vec![1],
                    sequence: 0xffffffff,
                }],
                outputs: vec![TxOutput {
                    value: 100 * COIN,
                    script_pubkey: script,
                }],
                lock_time: 1,
                claim_address: None,
                claim_signature: None,
            }];
            let mut block = Block {
                header: BlockHeader {
                    version: 1,
                    prev_block_hash: genesis_hash,
                    merkle_root: [0u8; 32],
                    utxo_root: [0u8; 32],
                    timestamp: funding_ts,
                    bits: crate::genesis::GENESIS_BITS,
                    nonce: 1,
                    stake_modifier: compute_stake_modifier(0, &genesis_hash),
                },
                transactions,
            };
            block.header.merkle_root = block.compute_merkle_root();
            block
        };
        chain.add_block(funding_block).unwrap();

        let utxos = chain.get_utxos_for_address(&address);
        assert!(!utxos.is_empty());
        let prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;

        let engine = StakingEngine::with_wif(address, wif);
        let mut found: Option<(Block, StakeProof)> = None;
        let mut ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
        for _ in 0..100_000 {
            if let Some(r) = engine.build_stake_block_with_proof(
                chain.best_hash().unwrap(),
                prev_modifier,
                2,
                ts,
                utxos.clone(),
                vec![],
            ) {
                found = Some(r);
                break;
            }
            ts += 1;
        }
        let (block, proof) = found.expect("should find stake kernel");

        // Producer's header commitment must equal the pre-block UTXO root.
        let prev_root = compute_utxo_root_sorted(&utxos);
        assert!(
            proof
                .utxo_proof
                .verify(&prev_root, &hash_utxo(&proof.utxo))
                .is_ok(),
            "utxo proof must verify against pre-block root"
        );

        let acceptance = chain.add_block(block).unwrap();
        assert!(matches!(
            acceptance,
            crate::chain::BlockAcceptance::MainChain { height: 2, .. }
        ));

        // The stored tip header must carry the producer's post-apply root —
        // chain overwrites with journal root, and both must already agree.
        let stored = chain.get_block_at_height(2).unwrap();
        assert_eq!(stored.header.utxo_root, {
            let all: Vec<Utxo> = chain.get_utxo_set().values().cloned().collect();
            compute_utxo_root_sorted(&all)
        });
    }
}

#[cfg(test)]
mod root_parity_tests;

#[cfg(test)]
mod conflict_tests {
    use super::*;
    use crate::block::{Transaction, TxInput, TxOutput, TxType};
    use crate::chain::Utxo;
    use crate::consensus::COIN;

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    fn transfer_spending(txid: [u8; 32], vout: u32) -> Transaction {
        Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: txid,
                prev_vout: vout,
                script_sig: vec![],
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value: 1000,
                script_pubkey: vec![],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        }
    }

    /// A pending mempool tx spending the same outpoint the coinstake spends
    /// must be excluded from the block template — otherwise the block fails
    /// UTXO validation and staking wedges forever.
    #[test]
    fn test_build_stake_block_excludes_conflicting_mempool_tx() {
        let engine = StakingEngine::new_fast("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(1000 * COIN, (MIN_STAKE_AGE as u32).saturating_add(3600));
        // A stale mempool tx spending the exact outpoint the coinstake will spend.
        let conflicting = transfer_spending([1u8; 32], 0);
        // An unrelated pending tx that must be kept.
        let unrelated = transfer_spending([9u8; 32], 3);

        // Scan timestamps until a kernel hits so the block actually builds.
        let prev_modifier = 0xdead_beef_u64;
        let mut block = None;
        for ts in 1_700_000_000..1_700_000_000 + 3600 {
            block = engine.build_stake_block(
                [2u8; 32],
                prev_modifier,
                101,
                ts,
                vec![utxo.clone()],
                vec![conflicting.clone(), unrelated.clone()],
            );
            if block.is_some() {
                break;
            }
        }
        let block = block.expect("kernel should hit within an hour of timestamps");
        assert_eq!(block.transactions[0].tx_type, TxType::Coinstake);
        let txids: Vec<[u8; 32]> = block.transactions[1..].iter().map(|t| t.txid()).collect();
        assert!(
            !txids.contains(&conflicting.txid()),
            "conflicting mempool tx must be excluded from the block"
        );
        assert!(
            txids.contains(&unrelated.txid()),
            "unrelated mempool tx must be included"
        );
    }
}
