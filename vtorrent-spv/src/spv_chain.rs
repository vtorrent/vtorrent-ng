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

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use crate::error::{Result, SpvError};
use crate::merkle::MerkleProof;

/// A compact block header (80 bytes on the wire, similar to Bitcoin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpvHeader {
    /// Block version.
    pub version: u32,
    /// Hash of the previous block header.
    pub prev_hash: [u8; 32],
    /// Merkle root of all transactions in the block.
    pub merkle_root: [u8; 32],
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
        let mut buf = Vec::with_capacity(80);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.prev_hash);
        buf.extend_from_slice(&self.merkle_root);
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(&self.bits.to_le_bytes());
        buf.extend_from_slice(&self.nonce.to_le_bytes());
        let first = Sha256::digest(&buf);
        Sha256::digest(first).into()
    }

    /// Returns true if this is a PoS block (version bit 8 set, matching vtorrent-node).
    pub fn is_pos(&self) -> bool {
        self.version & 0x100 != 0
    }
}

/// A lightweight chain of block headers for SPV verification.
#[derive(Debug, Default)]
pub struct SpvChain {
    /// Headers indexed by their hash.
    headers: HashMap<[u8; 32], SpvHeader>,
    /// Best (highest) chain tip hash.
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
        let hash = header.hash();

        // Check for duplicate
        if self.headers.contains_key(&hash) {
            return Ok(());
        }

        // Validate chain linkage (skip for genesis block at height 0)
        if header.height > 0 {
            if !self.headers.contains_key(&header.prev_hash) {
                return Err(SpvError::UnknownParent(hex::encode(header.prev_hash)));
            }
        }

        let height = header.height;
        self.headers.insert(hash, header);

        // Update best tip if this extends the chain
        if height >= self.best_height || self.best_hash.is_none() {
            self.best_height = height;
            self.best_hash = Some(hash);
        }

        Ok(())
    }

    /// Add multiple headers in sequence (e.g., from a `headers` P2P message).
    pub fn add_headers(&mut self, headers: Vec<SpvHeader>) -> Result<usize> {
        let mut added = 0;
        for h in headers {
            self.add_header(h)?;
            added += 1;
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
    pub fn verify_tx_inclusion(
        &self,
        block_hash: &[u8; 32],
        proof: &MerkleProof,
    ) -> Result<()> {
        let header = self.headers.get(block_hash)
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
                current = current.and_then(|h| {
                    self.headers.get(&h).map(|hdr| hdr.prev_hash)
                }).filter(|h| h != &[0u8; 32]);
            }
        }

        locator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header(height: u32, prev: [u8; 32], merkle: [u8; 32]) -> SpvHeader {
        SpvHeader {
            version: 1,
            prev_hash: prev,
            merkle_root: merkle,
            timestamp: 1_700_000_000 + height,
            bits: 0x1d00ffff,
            nonce: height,
            height,
        }
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

        let txids: Vec<[u8; 32]> = (0..4u8).map(|i| { let mut h = [0u8; 32]; h[0] = i; h }).collect();
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
}
