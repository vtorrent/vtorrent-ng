//! Merkle inclusion proof verification for Bitcoin transactions.

use crate::error::{BtcError, Result};
use bitcoin::hashes::{sha256d, Hash};

fn hash256(data: &[u8]) -> [u8; 32] {
    sha256d::Hash::hash(data).to_byte_array()
}

fn combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    hash256(&buf)
}

/// Verify that `txid` is included in the block with the given merkle root.
///
/// `siblings` is the ordered list of sibling hashes from the leaf up to the
/// root; `index` is the leaf's position in the tree.
pub fn verify_inclusion(
    txid: &[u8; 32],
    merkle_root: &[u8; 32],
    siblings: &[[u8; 32]],
    index: u32,
) -> Result<()> {
    let mut current = *txid;
    let mut idx = index;
    for sibling in siblings {
        current = if idx.is_multiple_of(2) {
            combine(&current, sibling)
        } else {
            combine(sibling, &current)
        };
        idx /= 2;
    }
    if current == *merkle_root {
        Ok(())
    } else {
        Err(BtcError::Bitcoin("merkle proof mismatch".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_leaf() {
        let txid = [1u8; 32];
        assert!(verify_inclusion(&txid, &txid, &[], 0).is_ok());
    }

    #[test]
    fn test_two_leaves() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let root = combine(&a, &b);
        assert!(verify_inclusion(&a, &root, &[b], 0).is_ok());
        assert!(verify_inclusion(&b, &root, &[a], 1).is_ok());
    }

    #[test]
    fn test_wrong_sibling_fails() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let root = combine(&a, &b);
        assert!(verify_inclusion(&a, &root, &[a], 0).is_err());
    }
}
