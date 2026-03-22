//! Merkle tree construction and inclusion proof verification.
//!
//! SPV clients use Merkle proofs to verify that a transaction is included in a
//! block without downloading the full block. The proof consists of a path of
//! sibling hashes from the transaction leaf up to the Merkle root.

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use crate::error::{Result, SpvError};

/// Double-SHA256 hash (the standard hash function for vTorrent/Bitcoin data).
fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    Sha256::digest(first).into()
}

/// Hash two child nodes together to produce their parent node.
fn combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    hash256(&buf)
}

/// A complete Merkle tree built from a list of transaction IDs.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    /// All levels of the tree, from leaves (level 0) to root (last level).
    levels: Vec<Vec<[u8; 32]>>,
}

impl MerkleTree {
    /// Build a Merkle tree from a list of transaction IDs.
    ///
    /// If the list has an odd number of elements, the last element is duplicated
    /// (standard Bitcoin/vTorrent Merkle tree behaviour).
    pub fn build(txids: &[[u8; 32]]) -> Self {
        if txids.is_empty() {
            return Self { levels: vec![vec![[0u8; 32]]] };
        }

        let mut levels: Vec<Vec<[u8; 32]>> = vec![txids.to_vec()];

        loop {
            let current = levels.last().unwrap();
            if current.len() == 1 {
                break;
            }

            let mut next = Vec::new();
            let mut i = 0;
            while i < current.len() {
                let left = current[i];
                let right = if i + 1 < current.len() { current[i + 1] } else { current[i] };
                next.push(combine(&left, &right));
                i += 2;
            }
            levels.push(next);
        }

        Self { levels }
    }

    /// Returns the Merkle root.
    pub fn root(&self) -> [u8; 32] {
        *self.levels.last().unwrap().first().unwrap()
    }

    /// Generate a Merkle inclusion proof for the transaction at `index`.
    ///
    /// Returns `None` if the index is out of bounds.
    pub fn proof(&self, index: usize) -> Option<MerkleProof> {
        if self.levels.is_empty() || index >= self.levels[0].len() {
            return None;
        }

        let txid = self.levels[0][index];
        let mut siblings = Vec::new();
        let mut current_index = index;

        for level in &self.levels[..self.levels.len() - 1] {
            let sibling_index = if current_index % 2 == 0 {
                // We are the left child; sibling is to the right
                (current_index + 1).min(level.len() - 1)
            } else {
                // We are the right child; sibling is to the left
                current_index - 1
            };

            siblings.push(ProofNode {
                hash: level[sibling_index],
                is_left: current_index % 2 == 1, // sibling is left if we are right
            });

            current_index /= 2;
        }

        Some(MerkleProof {
            txid,
            index,
            siblings,
            root: self.root(),
        })
    }
}

/// A single node in a Merkle proof path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofNode {
    /// The sibling hash at this level.
    pub hash: [u8; 32],
    /// True if this sibling is the LEFT child (i.e., the proven tx is the right child).
    pub is_left: bool,
}

/// A Merkle inclusion proof: proves that `txid` is in the block with `root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    /// The transaction ID being proven.
    pub txid: [u8; 32],
    /// The index of the transaction in the block.
    pub index: usize,
    /// The sibling hashes along the path from leaf to root.
    pub siblings: Vec<ProofNode>,
    /// The expected Merkle root.
    pub root: [u8; 32],
}

impl MerkleProof {
    /// Verify the proof against a known Merkle root.
    ///
    /// Returns `Ok(())` if the proof is valid, or an error describing the failure.
    pub fn verify(&self, expected_root: &[u8; 32]) -> Result<()> {
        let mut current = self.txid;

        for node in &self.siblings {
            current = if node.is_left {
                combine(&node.hash, &current)
            } else {
                combine(&current, &node.hash)
            };
        }

        if &current == expected_root {
            Ok(())
        } else {
            Err(SpvError::InvalidMerkleProof(format!(
                "computed root {} != expected {}",
                hex::encode(current),
                hex::encode(expected_root)
            )))
        }
    }

    /// Verify against the root stored in the proof itself.
    pub fn verify_self(&self) -> Result<()> {
        let root = self.root;
        self.verify(&root)
    }

    /// Serialize the proof to JSON bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize a proof from JSON bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes)
            .map_err(|e| SpvError::Serialization(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_txids(n: usize) -> Vec<[u8; 32]> {
        (0..n).map(|i| {
            let mut h = [0u8; 32];
            h[0] = i as u8;
            h[1] = (i >> 8) as u8;
            h
        }).collect()
    }

    #[test]
    fn test_single_tx_root() {
        let txids = make_txids(1);
        let tree = MerkleTree::build(&txids);
        assert_eq!(tree.root(), txids[0]);
    }

    #[test]
    fn test_two_tx_root() {
        let txids = make_txids(2);
        let tree = MerkleTree::build(&txids);
        let expected = combine(&txids[0], &txids[1]);
        assert_eq!(tree.root(), expected);
    }

    #[test]
    fn test_odd_tx_duplicates_last() {
        let txids = make_txids(3);
        let tree = MerkleTree::build(&txids);
        // Level 1: [combine(0,1), combine(2,2)]
        // Root: combine(combine(0,1), combine(2,2))
        let l1_0 = combine(&txids[0], &txids[1]);
        let l1_1 = combine(&txids[2], &txids[2]);
        let expected_root = combine(&l1_0, &l1_1);
        assert_eq!(tree.root(), expected_root);
    }

    #[test]
    fn test_proof_verify_valid() {
        let txids = make_txids(8);
        let tree = MerkleTree::build(&txids);
        for i in 0..8 {
            let proof = tree.proof(i).expect("proof should exist");
            proof.verify_self().expect("proof should be valid");
        }
    }

    #[test]
    fn test_proof_verify_against_root() {
        let txids = make_txids(4);
        let tree = MerkleTree::build(&txids);
        let root = tree.root();
        let proof = tree.proof(2).unwrap();
        proof.verify(&root).expect("proof should be valid");
    }

    #[test]
    fn test_proof_invalid_root_fails() {
        let txids = make_txids(4);
        let tree = MerkleTree::build(&txids);
        let proof = tree.proof(0).unwrap();
        let bad_root = [0xffu8; 32];
        assert!(proof.verify(&bad_root).is_err());
    }

    #[test]
    fn test_proof_serialization_roundtrip() {
        let txids = make_txids(16);
        let tree = MerkleTree::build(&txids);
        let proof = tree.proof(7).unwrap();
        let bytes = proof.to_bytes();
        let proof2 = MerkleProof::from_bytes(&bytes).unwrap();
        proof2.verify_self().expect("deserialized proof should be valid");
    }

    #[test]
    fn test_empty_tree() {
        let tree = MerkleTree::build(&[]);
        assert_eq!(tree.root(), [0u8; 32]);
    }

    #[test]
    fn test_large_tree() {
        let txids = make_txids(100);
        let tree = MerkleTree::build(&txids);
        let proof = tree.proof(50).unwrap();
        proof.verify_self().expect("proof for large tree should be valid");
    }
}
