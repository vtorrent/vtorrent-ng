//! Bitcoin block-header chain store for SPV.

use crate::error::{BtcError, Result};
use bitcoin::blockdata::block::Header;
use bitcoin::consensus::encode::deserialize;
use bitcoin::hashes::Hash;
use std::collections::HashMap;

/// A stored header with its height and cumulative chain work.
#[derive(Debug, Clone)]
pub struct StoredHeader {
    pub header: Header,
    pub height: u32,
    /// Cumulative work from genesis to this header (sum of per-header work).
    pub work: u128,
}

/// A lightweight Bitcoin header chain.
#[derive(Debug, Default)]
pub struct HeaderChain {
    headers: HashMap<[u8; 32], StoredHeader>,
    best_hash: Option<[u8; 32]>,
    best_height: u32,
    best_work: u128,
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

        let parent_work = if height > 0 {
            let prev: [u8; 32] = header.prev_blockhash.to_byte_array();
            // Bootstrap: the first header received from a trusted peer is
            // block 1, whose parent is the network genesis block (which we do
            // not store). Accept it as the chain root.
            if !self.headers.contains_key(&prev) && !self.headers.is_empty() {
                return Err(BtcError::Bitcoin(format!(
                    "unknown parent {}",
                    hex::encode(prev)
                )));
            }
            self.headers.get(&prev).map(|h| h.work).unwrap_or(0)
        } else {
            0
        };

        let work = parent_work.saturating_add(header_work(header.bits));
        self.headers.insert(
            hash,
            StoredHeader {
                header,
                height,
                work,
            },
        );

        // Select the tip by cumulative work, not raw height, so a peer cannot
        // steer the client onto a high-height but low-work fork.
        if self.best_hash.is_none() || work > self.best_work {
            self.best_height = height;
            self.best_hash = Some(hash);
            self.best_work = work;
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

    /// Return the block hashes at heights `>= start_height`, sorted ascending.
    ///
    /// Used to request `merkleblock`s for a UTXO scan from a checkpoint.
    pub fn hashes_from(&self, start_height: u32) -> Vec<[u8; 32]> {
        let mut entries: Vec<(&u32, &[u8; 32])> = self
            .headers
            .iter()
            .filter(|(_, h)| h.height >= start_height)
            .map(|(hash, h)| (&h.height, hash))
            .collect();
        entries.sort_by_key(|(height, _)| **height);
        entries.into_iter().map(|(_, hash)| *hash).collect()
    }
}

/// Compute the work contributed by a single header from its compact `bits`.
///
/// Work is `2^256 / target`, the standard Bitcoin measure. A header with a
/// smaller target (higher difficulty) contributes more work. Saturated to
/// `u128::MAX` for absurdly small targets.
fn header_work(bits: bitcoin::CompactTarget) -> u128 {
    let bits = bits.to_consensus();
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
        // The first header is accepted as the chain root (bootstrap), even
        // though its parent (genesis) is not stored.
        let h0 = make_header([0xffu8; 32], 1);
        chain.add_header(&serialize(&h0), 1).unwrap();

        // A subsequent header whose parent is unknown must be rejected.
        let orphan = make_header([0xeeu8; 32], 2);
        assert!(chain.add_header(&serialize(&orphan), 2).is_err());
    }

    #[test]
    fn test_hashes_from() {
        let mut chain = HeaderChain::new();
        let h0 = make_header([0u8; 32], 0);
        let h0_hash: [u8; 32] = h0.block_hash().to_byte_array();
        chain.add_header(&serialize(&h0), 0).unwrap();
        let h1 = make_header(h0_hash, 1);
        let h1_hash: [u8; 32] = h1.block_hash().to_byte_array();
        chain.add_header(&serialize(&h1), 1).unwrap();
        let h2 = make_header(h1_hash, 2);
        let h2_hash: [u8; 32] = h2.block_hash().to_byte_array();
        chain.add_header(&serialize(&h2), 2).unwrap();

        let from_1 = chain.hashes_from(1);
        assert_eq!(from_1, vec![h1_hash, h2_hash]);

        let from_0 = chain.hashes_from(0);
        assert_eq!(from_0, vec![h0_hash, h1_hash, h2_hash]);

        let from_99 = chain.hashes_from(99);
        assert!(from_99.is_empty());
    }
}
