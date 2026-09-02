//! Stake proof types for SPV verification.
//!
//! Light clients verify PoS blocks without a full UTXO set by checking
//! self-contained [`StakeProof`]s that commit to the UTXO being staked
//! and the coinstake transaction's inclusion in the block.

use crate::error::{Result, SpvError};
use crate::merkle::{combine, MerkleProof, ProofNode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A UTXO as seen by an SPV client.
///
/// Mirrors `vtorrent-node::chain::Utxo` but defined locally to avoid a
/// hard dependency cycle (vtorrent-spv already depends on vtorrent-node
/// but keeps a decoupled SPV type for serialization stability).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpvUtxo {
    pub txid: [u8; 32],
    pub vout: u32,
    pub value: u64,
    pub script_pubkey: Vec<u8>,
    pub height: u32,
    pub timestamp: u32,
}

/// Compute the canonical leaf hash for a UTXO.
///
/// Leaf preimage: `SHA256d(txid || vout LE || value LE || varint(script.len) || script || height LE || timestamp LE)`
/// Double-SHA256, identical to `vtorrent-node/src/block.rs:hash_utxo`.
pub fn hash_utxo(u: &SpvUtxo) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(u.txid);
    h.update(u.vout.to_le_bytes());
    h.update(u.value.to_le_bytes());
    let len = u.script_pubkey.len() as u64;
    if len < 0xfd {
        h.update([len as u8]);
    } else if len <= 0xffff {
        h.update([0xfd]);
        h.update((len as u16).to_le_bytes());
    } else if len <= 0xffff_ffff {
        h.update([0xfe]);
        h.update((len as u32).to_le_bytes());
    } else {
        h.update([0xff]);
        h.update(len.to_le_bytes());
    }
    h.update(&u.script_pubkey);
    h.update(u.height.to_le_bytes());
    h.update(u.timestamp.to_le_bytes());
    let first = h.finalize();
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Merkle inclusion proof for a UTXO leaf against a `utxo_root`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UtxoInclusionProof {
    pub leaf_index: usize,
    pub siblings: Vec<ProofNode>,
    pub root: [u8; 32],
}

impl UtxoInclusionProof {
    /// Verify that `leaf` is included under `expected_root`.
    ///
    /// First checks `self.root == expected_root`, then recomputes the root
    /// via `combine` (same as `MerkleProof::verify`).
    pub fn verify(&self, expected_root: &[u8; 32], leaf: &[u8; 32]) -> Result<()> {
        if &self.root != expected_root {
            return Err(SpvError::InvalidMerkleProof(format!(
                "utxo root mismatch expected {} got {}",
                hex::encode(expected_root),
                hex::encode(self.root)
            )));
        }
        let mut cur = *leaf;
        for node in &self.siblings {
            cur = if node.is_left {
                combine(&node.hash, &cur)
            } else {
                combine(&cur, &node.hash)
            };
        }
        if &cur == expected_root {
            Ok(())
        } else {
            Err(SpvError::InvalidMerkleProof(format!(
                "computed {} != expected {}",
                hex::encode(cur),
                hex::encode(expected_root)
            )))
        }
    }

    /// Serialize to JSON bytes (mirrors `MerkleProof::to_bytes` convention).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from JSON bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| SpvError::Serialization(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Transaction mirror (same wire format as vtorrent-node/src/block.rs Transaction)
// ---------------------------------------------------------------------------

/// Transaction type flags — must stay in sync with `vtorrent-node::block::TxType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum TxType {
    Standard = 0,
    Coinbase = 1,
    Coinstake = 2,
    LegacyClaim = 3,
    AtomicSwap = 4,
    TorrentIncentive = 5,
}

/// A transaction input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxInput {
    pub prev_txid: [u8; 32],
    pub prev_vout: u32,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

/// A transaction output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxOutput {
    pub value: u64,
    pub script_pubkey: Vec<u8>,
}

/// A transaction (SPV mirror of `vtorrent-node::block::Transaction`).
///
/// Wire format is identical (`bincode` + `serde` derive) so `txid()` is
/// bit-identical to the consensus implementation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    pub version: u32,
    pub tx_type: TxType,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub lock_time: u32,
    pub claim_address: Option<String>,
    pub claim_signature: Option<Vec<u8>>,
}

impl Transaction {
    /// Compute the transaction hash (txid) as `SHA256d(bincode(self))`.
    pub fn txid(&self) -> [u8; 32] {
        let serialized = bincode::serialize(self).unwrap_or_else(|e| {
            tracing::warn!("txid serialization failed: {}", e);
            Vec::new()
        });
        let first = Sha256::digest(&serialized);
        let second = Sha256::digest(first);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&second);
        hash
    }

    /// Compute the SIGHASH_ALL signature hash for a single input.
    ///
    /// Bit-identical to `vtorrent-node::block::Transaction::sighash`
    /// (incremental SHA256 over transaction fields without cloning).
    pub fn sighash(&self, input_index: usize, subscript: &[u8]) -> [u8; 32] {
        debug_assert!(
            input_index < self.inputs.len(),
            "sighash input_index {} out of range ({} inputs)",
            input_index,
            self.inputs.len()
        );
        let mut h = Sha256::new();
        h.update(self.version.to_le_bytes());
        h.update((self.tx_type as u32).to_le_bytes());
        h.update((self.inputs.len() as u64).to_le_bytes());
        for (i, inp) in self.inputs.iter().enumerate() {
            h.update(inp.prev_txid);
            h.update(inp.prev_vout.to_le_bytes());
            let sig = if i == input_index {
                subscript
            } else {
                &[] as &[u8]
            };
            h.update((sig.len() as u64).to_le_bytes());
            h.update(sig);
            h.update(inp.sequence.to_le_bytes());
        }
        h.update((self.outputs.len() as u64).to_le_bytes());
        for out in &self.outputs {
            h.update(out.value.to_le_bytes());
            h.update((out.script_pubkey.len() as u64).to_le_bytes());
            h.update(&out.script_pubkey);
        }
        h.update(self.lock_time.to_le_bytes());
        match &self.claim_address {
            None => h.update([0u8]),
            Some(addr) => {
                h.update([1u8]);
                let bytes = addr.as_bytes();
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(bytes);
            }
        }
        match &self.claim_signature {
            None => h.update([0u8]),
            Some(sig) => {
                h.update([1u8]);
                h.update((sig.len() as u64).to_le_bytes());
                h.update(sig);
            }
        }
        h.update(1u32.to_le_bytes());
        let h1 = h.finalize();
        let h2 = Sha256::digest(h1);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&h2);
        hash
    }

    pub fn is_coinbase(&self) -> bool {
        self.tx_type == TxType::Coinbase
    }

    pub fn is_coinstake(&self) -> bool {
        self.tx_type == TxType::Coinstake
    }

    pub fn is_legacy_claim(&self) -> bool {
        self.tx_type == TxType::LegacyClaim
    }

    pub fn total_output(&self) -> u64 {
        self.outputs
            .iter()
            .fold(0u64, |acc, o| acc.saturating_add(o.value))
    }
}

/// A self-contained proof that a PoS block's coinstake is valid.
///
/// The proof targets `prev_header.utxo_root` (not `header.utxo_root`) to
/// avoid circularity — it proves the staked UTXO existed *before* the block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StakeProof {
    /// The coinstake transaction (TxType::Coinstake, 1 input).
    pub coinstake: Transaction,
    /// Merkle inclusion proof: `coinstake.txid() -> header.merkle_root`.
    pub tx_merkle_proof: MerkleProof,
    /// The full UTXO being spent.
    pub utxo: SpvUtxo,
    /// Merkle inclusion proof: `hash_utxo(&utxo) -> prev_header.utxo_root`.
    pub utxo_proof: UtxoInclusionProof,
    /// The stake modifier of the previous block (for kernel continuity).
    pub prev_stake_modifier: u64,
}

impl StakeProof {
    /// Serialize to JSON bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from JSON bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| SpvError::Serialization(e.to_string()))
    }
}

/// Consensus constants (mirrors vtorrent-node/src/consensus.rs)
pub const COIN: u64 = 100_000_000;
pub const MIN_STAKE_AMOUNT: u64 = COIN;
pub const MIN_STAKE_AGE: u64 = 6 * 60 * 60;
pub const MAX_STAKE_AGE: u64 = 6 * 24 * 60 * 60;
pub const MAX_MONEY: u64 = 20_000_000 * COIN;

/// Compute the stake modifier for the next block (SHA256d(prev_modifier LE || prev_hash)).
pub fn compute_stake_modifier(prev_stake_modifier: u64, prev_block_hash: &[u8; 32]) -> u64 {
    let mut data = Vec::with_capacity(40);
    data.extend_from_slice(&prev_stake_modifier.to_le_bytes());
    data.extend_from_slice(prev_block_hash);
    let first = Sha256::digest(&data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 8];
    out.copy_from_slice(&second[..8]);
    u64::from_le_bytes(out)
}

/// Compute stake kernel hash: SHA256d(stake_modifier LE || txid || vout LE || timestamp LE)
pub fn stake_kernel_hash(
    stake_modifier: u64,
    txid: &[u8; 32],
    vout: u32,
    timestamp: u32,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(stake_modifier.to_le_bytes());
    hasher.update(txid);
    hasher.update(vout.to_le_bytes());
    hasher.update(timestamp.to_le_bytes());
    let first = hasher.finalize();
    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hasher2.finalize());
    hash
}

/// Check whether a UTXO satisfies the stake kernel difficulty.
pub fn check_stake_kernel(
    stake_modifier: u64,
    value: u64,
    txid: &[u8; 32],
    vout: u32,
    timestamp: u32,
) -> bool {
    let kernel_hash = stake_kernel_hash(stake_modifier, txid, vout, timestamp);
    let kernel_val = u32::from_le_bytes([
        kernel_hash[0],
        kernel_hash[1],
        kernel_hash[2],
        kernel_hash[3],
    ]);
    let target = (value / 1000).min(u32::MAX as u64) as u32;
    kernel_val <= target
}

/// Check kernel for SpvUtxo directly.
pub fn check_stake_kernel_for_utxo(stake_modifier: u64, utxo: &SpvUtxo, timestamp: u32) -> bool {
    check_stake_kernel(stake_modifier, utxo.value, &utxo.txid, utxo.vout, timestamp)
}

/// Compute PoS reward: stake_amount * 5% * coin_age_days / 365, age capped at MAX_STAKE_AGE.
pub fn compute_pos_reward(stake_amount: u64, coin_age_seconds: u64) -> u64 {
    let coin_age_seconds = coin_age_seconds.min(MAX_STAKE_AGE);
    let numerator = stake_amount as u128 * coin_age_seconds as u128 * 5;
    let denominator = 100u128 * 86400 * 365;
    (numerator / denominator) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle::MerkleTree;

    const COIN: u64 = 100_000_000;

    fn sample_utxo(txid_byte: u8, vout: u32, value: u64) -> SpvUtxo {
        SpvUtxo {
            txid: [txid_byte; 32],
            vout,
            value,
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd,
                0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0x88, 0xac,
            ],
            height: 100,
            timestamp: 1_700_000_000,
        }
    }

    fn sample_utxos() -> Vec<SpvUtxo> {
        vec![
            sample_utxo(0x01, 0, 100 * COIN),
            sample_utxo(0x02, 0, 200 * COIN),
            sample_utxo(0x03, 1, 300 * COIN),
        ]
    }

    fn sample_utxo_tree(
        utxos: Vec<SpvUtxo>,
        leaf_index: usize,
    ) -> ([u8; 32], UtxoInclusionProof, [u8; 32]) {
        let leaves: Vec<[u8; 32]> = utxos.iter().map(hash_utxo).collect();
        let tree = MerkleTree::build(&leaves);
        let root = tree.root();
        let proof = tree.proof(leaf_index).expect("proof should exist");
        let leaf = hash_utxo(&utxos[leaf_index]);
        let utxo_proof = UtxoInclusionProof {
            leaf_index,
            siblings: proof.siblings.clone(),
            root,
        };
        (root, utxo_proof, leaf)
    }

    fn sample_coinstake(utxo: &SpvUtxo) -> Transaction {
        Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: vec![0x30, 0x44, 0x02, 0x20, 0x11, 0x22, 0x01, 0x02],
                sequence: 0xffffffff,
            }],
            outputs: vec![
                TxOutput {
                    value: 0,
                    script_pubkey: vec![],
                },
                TxOutput {
                    value: utxo.value + 5000,
                    script_pubkey: vec![
                        0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef,
                        0xab, 0xcd, 0xef, 0xab, 0xcd, 0xef, 0xab, 0xcd, 0x88, 0xac,
                    ],
                },
            ],
            lock_time: 12345,
            claim_address: None,
            claim_signature: None,
        }
    }

    fn sample_proof() -> StakeProof {
        let utxos = sample_utxos();
        let utxo = utxos[1].clone();
        let coinstake = sample_coinstake(&utxo);

        // tx merkle proof (coinstake at index 0)
        let txids = vec![coinstake.txid(), [0x11u8; 32], [0x22u8; 32]];
        let tx_tree = MerkleTree::build(&txids);
        let tx_merkle_proof = tx_tree.proof(0).unwrap();

        // utxo proof
        let leaves: Vec<[u8; 32]> = utxos.iter().map(hash_utxo).collect();
        let utxo_tree = MerkleTree::build(&leaves);
        let leaf_index = 1;
        let mp = utxo_tree.proof(leaf_index).unwrap();
        let utxo_proof = UtxoInclusionProof {
            leaf_index,
            siblings: mp.siblings.clone(),
            root: utxo_tree.root(),
        };

        StakeProof {
            coinstake,
            tx_merkle_proof,
            utxo,
            utxo_proof,
            prev_stake_modifier: 0xdeadbeef_cafebabe,
        }
    }

    #[test]
    fn test_hash_utxo_deterministic() {
        let utxo = SpvUtxo {
            txid: [1u8; 32],
            vout: 0,
            value: 100 * COIN,
            script_pubkey: vec![0x76, 0xa9, 0x14, 0xab, 0xcd],
            height: 100,
            timestamp: 1_700_000_000,
        };
        assert_eq!(hash_utxo(&utxo), hash_utxo(&utxo));
        // different value => different hash
        let mut utxo2 = utxo.clone();
        utxo2.value = 200 * COIN;
        assert_ne!(hash_utxo(&utxo), hash_utxo(&utxo2));
    }

    #[test]
    fn test_stake_proof_serialization_roundtrip() {
        let proof = sample_proof();
        let bytes = proof.to_bytes();
        assert!(!bytes.is_empty());
        let proof2 = StakeProof::from_bytes(&bytes).unwrap();
        assert_eq!(proof.utxo.txid, proof2.utxo.txid);
        assert_eq!(proof.utxo.vout, proof2.utxo.vout);
        assert_eq!(proof.utxo.value, proof2.utxo.value);
        assert_eq!(proof.tx_merkle_proof.root, proof2.tx_merkle_proof.root);
        assert_eq!(proof.tx_merkle_proof.txid, proof2.tx_merkle_proof.txid);
        assert_eq!(proof.utxo_proof.root, proof2.utxo_proof.root);
        assert_eq!(proof.prev_stake_modifier, proof2.prev_stake_modifier);
        assert_eq!(proof.coinstake.txid(), proof2.coinstake.txid());
        // full equality
        assert_eq!(proof, proof2);
    }

    #[test]
    fn test_utxo_inclusion_proof_verify() {
        let utxos = sample_utxos();
        let (root, proof, leaf) = sample_utxo_tree(utxos, 1);
        assert!(proof.verify(&root, &leaf).is_ok());
        assert!(proof.verify(&[0xffu8; 32], &leaf).is_err());
        // wrong leaf
        let bad_leaf = [0xabu8; 32];
        assert!(proof.verify(&root, &bad_leaf).is_err());
        // root mismatch in proof object vs expected
        let mut bad_proof = proof.clone();
        bad_proof.root = [0xeeu8; 32];
        assert!(bad_proof.verify(&root, &leaf).is_err());
    }

    #[test]
    fn test_sighash_and_txid_bit_identical_with_node() {
        let utxo = sample_utxo(0x77, 2, 555 * COIN);
        let spv_tx = sample_coinstake(&utxo);
        let node_tx = vtorrent_node::block::Transaction {
            version: spv_tx.version,
            tx_type: match spv_tx.tx_type {
                TxType::Standard => vtorrent_node::block::TxType::Standard,
                TxType::Coinbase => vtorrent_node::block::TxType::Coinbase,
                TxType::Coinstake => vtorrent_node::block::TxType::Coinstake,
                TxType::LegacyClaim => vtorrent_node::block::TxType::LegacyClaim,
                TxType::AtomicSwap => vtorrent_node::block::TxType::AtomicSwap,
                TxType::TorrentIncentive => vtorrent_node::block::TxType::TorrentIncentive,
            },
            inputs: spv_tx
                .inputs
                .iter()
                .map(|i| vtorrent_node::block::TxInput {
                    prev_txid: i.prev_txid,
                    prev_vout: i.prev_vout,
                    script_sig: i.script_sig.clone(),
                    sequence: i.sequence,
                })
                .collect(),
            outputs: spv_tx
                .outputs
                .iter()
                .map(|o| vtorrent_node::block::TxOutput {
                    value: o.value,
                    script_pubkey: o.script_pubkey.clone(),
                })
                .collect(),
            lock_time: spv_tx.lock_time,
            claim_address: spv_tx.claim_address.clone(),
            claim_signature: spv_tx.claim_signature.clone(),
        };

        assert_eq!(spv_tx.txid(), node_tx.txid());
        let subscript = vec![0xaau8; 40];
        for idx in 0..spv_tx.inputs.len() {
            assert_eq!(
                spv_tx.sighash(idx, &subscript),
                node_tx.sighash(idx, &subscript)
            );
        }

        let node_utxo = vtorrent_node::chain::Utxo {
            txid: utxo.txid,
            vout: utxo.vout,
            value: utxo.value,
            script_pubkey: utxo.script_pubkey.clone(),
            height: utxo.height,
            timestamp: utxo.timestamp,
        };
        assert_eq!(
            hash_utxo(&utxo),
            vtorrent_node::block::hash_utxo(&node_utxo)
        );
    }

    #[test]
    fn test_hash_utxo_matches_block_rs_vector() {
        let spv = sample_utxo(0x42, 7, 12345);
        let h1 = hash_utxo(&spv);
        // recompute manually same as block.rs logic to ensure no drift
        let mut h = Sha256::new();
        h.update(spv.txid);
        h.update(spv.vout.to_le_bytes());
        h.update(spv.value.to_le_bytes());
        let len = spv.script_pubkey.len() as u64;
        if len < 0xfd {
            h.update([len as u8]);
        } else if len <= 0xffff {
            h.update([0xfd]);
            h.update((len as u16).to_le_bytes());
        } else if len <= 0xffff_ffff {
            h.update([0xfe]);
            h.update((len as u32).to_le_bytes());
        } else {
            h.update([0xff]);
            h.update(len.to_le_bytes());
        }
        h.update(&spv.script_pubkey);
        h.update(spv.height.to_le_bytes());
        h.update(spv.timestamp.to_le_bytes());
        let first = h.finalize();
        let second = Sha256::digest(first);
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&second);
        assert_eq!(h1, expected);
    }
}
