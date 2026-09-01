//! Bitcoin block-header chain store for SPV.

use crate::error::{BtcError, Result};
use bitcoin::blockdata::block::Header;
use bitcoin::blockdata::constants::genesis_block;
use bitcoin::consensus::encode::deserialize;
use bitcoin::consensus::params::Params;
use bitcoin::hashes::Hash;
use bitcoin::pow::{CompactTarget, Target};
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
#[derive(Debug)]
pub struct HeaderChain {
    headers: HashMap<[u8; 32], StoredHeader>,
    best_hash: Option<[u8; 32]>,
    best_height: u32,
    best_work: u128,
    network: Option<bitcoin::Network>,
}

impl HeaderChain {
    pub fn new() -> Self {
        Self::anchored(bitcoin::Network::Bitcoin)
    }

    #[cfg(test)]
    pub(crate) fn unanchored_for_tests() -> Self {
        Self {
            headers: HashMap::new(),
            best_hash: None,
            best_height: 0,
            best_work: 0,
            network: None,
        }
    }

    pub fn anchored(network: bitcoin::Network) -> Self {
        let genesis = genesis_block(network);
        let hash = genesis.block_hash().to_byte_array();
        let work = header_work(genesis.header.bits);
        let mut headers = HashMap::new();
        headers.insert(
            hash,
            StoredHeader {
                header: genesis.header,
                height: 0,
                work,
            },
        );
        Self {
            headers,
            best_hash: Some(hash),
            best_height: 0,
            best_work: work,
            network: Some(network),
        }
    }

    /// Add a header (raw 80-byte serialization) at the given height.
    pub fn add_header(&mut self, raw: &[u8], height: u32) -> Result<()> {
        let header: Header = deserialize(raw).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        let hash: [u8; 32] = header.block_hash().to_byte_array();

        if self.headers.contains_key(&hash) {
            return Ok(());
        }

        // Proof-of-work validation: the header hash must meet the target
        // encoded in its own `bits`. Without this a malicious peer could
        // fabricate an arbitrarily long "chain" of zero-work headers and
        // phantom balances would pass SPV verification.
        header
            .validate_pow(header.bits.into())
            .map_err(|e| BtcError::Bitcoin(format!("PoW validation failed: {}", e)))?;

        if let Some(network) = self.network {
            let params = Params::new(network);
            let target = Target::from(header.bits);
            if target > params.max_attainable_target {
                return Err(BtcError::Bitcoin(
                    "header target exceeds the network proof-of-work limit".into(),
                ));
            }
            if height == 0 {
                return Err(BtcError::Bitcoin(
                    "anchored header chain already contains genesis".into(),
                ));
            }
            let prev_hash = header.prev_blockhash.to_byte_array();
            let parent = self.headers.get(&prev_hash).ok_or_else(|| {
                BtcError::Bitcoin(format!("unknown parent {}", hex::encode(prev_hash)))
            })?;
            let interval = params.pow_target_timespan / params.pow_target_spacing;
            let at_retarget = u64::from(height) % interval == 0;
            let pow_limit = params.max_attainable_target.to_compact_lossy();
            let expected = if at_retarget {
                let boundary_height = height.saturating_sub(interval as u32);
                let mut boundary_hash = prev_hash;
                let boundary = loop {
                    let stored = self.headers.get(&boundary_hash).ok_or_else(|| {
                        BtcError::Bitcoin("missing retarget boundary header".into())
                    })?;
                    if stored.height == boundary_height {
                        break stored;
                    }
                    boundary_hash = stored.header.prev_blockhash.to_byte_array();
                };
                CompactTarget::from_header_difficulty_adjustment(
                    boundary.header,
                    parent.header,
                    &params,
                )
            } else if params.allow_min_difficulty_blocks
                && u64::from(header.time)
                    > u64::from(parent.header.time) + params.pow_target_spacing * 2
            {
                pow_limit
            } else if params.allow_min_difficulty_blocks {
                let mut cursor = parent;
                while u64::from(cursor.height) % interval != 0 && cursor.header.bits == pow_limit {
                    let cursor_parent = cursor.header.prev_blockhash.to_byte_array();
                    cursor = self.headers.get(&cursor_parent).ok_or_else(|| {
                        BtcError::Bitcoin("missing testnet difficulty ancestor".into())
                    })?;
                }
                cursor.header.bits
            } else {
                parent.header.bits
            };
            if header.bits != expected {
                return Err(BtcError::Bitcoin(format!(
                    "unexpected difficulty at height {}: got {:08x}, expected {:08x}",
                    height,
                    header.bits.to_consensus(),
                    expected.to_consensus()
                )));
            }
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
            // Validate height continuity: the height must be exactly one more
            // than the parent's, so a caller cannot inject an arbitrary height.
            if let Some(parent) = self.headers.get(&prev) {
                if height != parent.height + 1 {
                    return Err(BtcError::Bitcoin(format!(
                        "height {} does not follow parent height {}",
                        height, parent.height
                    )));
                }
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
        let Some(mut hash) = self.best_hash else {
            return Vec::new();
        };
        let mut hashes = Vec::new();
        loop {
            let Some(stored) = self.headers.get(&hash) else {
                return Vec::new();
            };
            if stored.height < start_height {
                break;
            }
            hashes.push(hash);
            if stored.height == 0 {
                break;
            }
            hash = stored.header.prev_blockhash.to_byte_array();
        }
        hashes.reverse();
        hashes
    }
}

impl Default for HeaderChain {
    fn default() -> Self {
        Self::new()
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
        // Maximum (easiest) target; the caller-supplied nonce may not satisfy
        // PoW (50% at max target), so mine forward from it until it does.
        let bits = bitcoin::CompactTarget::from_consensus(0x207fffff);
        let mut nonce = nonce;
        loop {
            let h = Header {
                version: bitcoin::blockdata::block::Version::ONE,
                prev_blockhash: bitcoin::BlockHash::from_byte_array(prev),
                merkle_root: bitcoin::TxMerkleNode::all_zeros(),
                time: 1_700_000_000 + nonce,
                bits,
                nonce,
            };
            if h.validate_pow(bits.into()).is_ok() {
                return h;
            }
            nonce = nonce.wrapping_add(1);
        }
    }

    #[test]
    fn test_pow_invalid_header_rejected() {
        let mut chain = HeaderChain::unanchored_for_tests();
        // A header whose hash cannot meet a hard target must be rejected.
        let mut h = make_header([0u8; 32], 0);
        h.bits = bitcoin::CompactTarget::from_consensus(0x1b_00_00_01); // ~impossible
        assert!(chain.add_header(&serialize(&h), 0).is_err());
    }

    #[test]
    fn test_default_chain_is_anchored_to_bitcoin_genesis() {
        let chain = HeaderChain::new();
        let genesis_hash = genesis_block(bitcoin::Network::Bitcoin)
            .block_hash()
            .to_byte_array();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain.best_hash(), Some(genesis_hash));
    }

    #[test]
    fn test_regtest_min_difficulty_child_is_accepted() {
        let network = bitcoin::Network::Regtest;
        let genesis = genesis_block(network);
        let bits = genesis.header.bits;
        let mut header = Header {
            version: bitcoin::blockdata::block::Version::ONE,
            prev_blockhash: genesis.block_hash(),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: genesis.header.time + 1,
            bits,
            nonce: 0,
        };
        while header.validate_pow(bits.into()).is_err() {
            header.nonce = header.nonce.wrapping_add(1);
        }

        let mut chain = HeaderChain::anchored(network);
        chain.add_header(&serialize(&header), 1).unwrap();
        assert_eq!(chain.best_height(), 1);
    }

    #[test]
    fn test_add_genesis() {
        let mut chain = HeaderChain::unanchored_for_tests();
        let h = make_header([0u8; 32], 0);
        chain.add_header(&serialize(&h), 0).unwrap();
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }

    #[test]
    fn test_chain_of_headers() {
        let mut chain = HeaderChain::unanchored_for_tests();
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
        let mut chain = HeaderChain::unanchored_for_tests();
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
        let mut chain = HeaderChain::unanchored_for_tests();
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

    #[test]
    fn test_hashes_from_only_returns_best_chain() {
        let mut chain = HeaderChain::unanchored_for_tests();
        let h0 = make_header([0u8; 32], 0);
        let h0_hash = h0.block_hash().to_byte_array();
        chain.add_header(&serialize(&h0), 0).unwrap();

        let main_h1 = make_header(h0_hash, 1);
        chain.add_header(&serialize(&main_h1), 1).unwrap();
        let fork_h1 = make_header(h0_hash, 20);
        let fork_h1_hash = fork_h1.block_hash().to_byte_array();
        chain.add_header(&serialize(&fork_h1), 1).unwrap();
        let fork_h2 = make_header(fork_h1_hash, 30);
        let fork_h2_hash = fork_h2.block_hash().to_byte_array();
        chain.add_header(&serialize(&fork_h2), 2).unwrap();

        assert_eq!(
            chain.hashes_from(0),
            vec![h0_hash, fork_h1_hash, fork_h2_hash]
        );
    }
}
