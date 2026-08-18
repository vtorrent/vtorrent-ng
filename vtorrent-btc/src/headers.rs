//! Bitcoin block-header chain store for SPV.

use crate::error::{BtcError, Result};
use bitcoin::blockdata::block::Header;
use bitcoin::consensus::encode::deserialize;
use bitcoin::hashes::Hash;
use std::collections::HashMap;

/// A stored header with its height.
#[derive(Debug, Clone)]
pub struct StoredHeader {
    pub header: Header,
    pub height: u32,
}

/// A lightweight Bitcoin header chain.
#[derive(Debug, Default)]
pub struct HeaderChain {
    headers: HashMap<[u8; 32], StoredHeader>,
    best_hash: Option<[u8; 32]>,
    best_height: u32,
}

impl HeaderChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a header (raw 80-byte serialization) at the given height.
    pub fn add_header(&mut self, raw: &[u8], height: u32) -> Result<()> {
        let header: Header = deserialize(raw).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        let hash: [u8; 32] = header.block_hash().to_byte_array();

        if self.headers.contains_key(&hash) {
            return Ok(());
        }

        if height > 0 {
            let prev: [u8; 32] = header.prev_blockhash.to_byte_array();
            if !self.headers.contains_key(&prev) {
                return Err(BtcError::Bitcoin(format!(
                    "unknown parent {}",
                    hex::encode(prev)
                )));
            }
        }

        self.headers.insert(hash, StoredHeader { header, height });
        if height >= self.best_height || self.best_hash.is_none() {
            self.best_height = height;
            self.best_hash = Some(hash);
        }
        Ok(())
    }

    pub fn best_height(&self) -> u32 {
        self.best_height
    }

    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.best_hash
    }

    pub fn len(&self) -> usize {
        self.headers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<&StoredHeader> {
        self.headers.get(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::blockdata::block::Header;
    use bitcoin::consensus::encode::serialize;

    fn make_header(prev: [u8; 32], nonce: u32) -> Header {
        Header {
            version: bitcoin::blockdata::block::Version::ONE,
            prev_blockhash: bitcoin::BlockHash::from_byte_array(prev),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: 1_700_000_000 + nonce,
            bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
            nonce,
        }
    }

    #[test]
    fn test_add_genesis() {
        let mut chain = HeaderChain::new();
        let h = make_header([0u8; 32], 0);
        chain.add_header(&serialize(&h), 0).unwrap();
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }

    #[test]
    fn test_chain_of_headers() {
        let mut chain = HeaderChain::new();
        let h0 = make_header([0u8; 32], 0);
        let h0_hash: [u8; 32] = h0.block_hash().to_byte_array();
        chain.add_header(&serialize(&h0), 0).unwrap();

        let h1 = make_header(h0_hash, 1);
        chain.add_header(&serialize(&h1), 1).unwrap();
        assert_eq!(chain.best_height(), 1);
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_unknown_parent_rejected() {
        let mut chain = HeaderChain::new();
        let orphan = make_header([0xffu8; 32], 1);
        assert!(chain.add_header(&serialize(&orphan), 1).is_err());
    }
}
