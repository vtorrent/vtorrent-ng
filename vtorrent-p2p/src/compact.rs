use crate::message::{BlockTxnMsg, CmpctBlockMsg, GetBlockTxnMsg, PrefilledTx};
/// Compact block relay (BIP-152 style) for vTorrent.
///
/// Compact blocks dramatically reduce block propagation bandwidth. Instead of
/// sending full blocks (~1 MB+), a node sends a compact block (~10 KB) containing:
/// - The block header
/// - Short 6-byte transaction IDs (SipHash-2-4 of txid)
/// - Prefilled transactions (always the coinbase)
///
/// The receiver looks up the transactions in their mempool. If any are missing,
/// they request only those via `getblocktxn` / `blocktxn`.
///
/// ## Bandwidth Savings
///
/// For a typical block where the receiver already has all transactions in their
/// mempool (the common case), bandwidth drops from ~1 MB to ~10 KB — a 99% reduction.
///
/// ## Two Modes
///
/// - **High-bandwidth mode**: The sender immediately relays the compact block
///   without waiting for a `getdata` request. Used for the 3 fastest peers.
/// - **Low-bandwidth mode**: The sender announces via `inv`, waits for `getdata`,
///   then sends the compact block. Used for all other peers.
use std::collections::HashMap;

/// SipHash-2-4 implementation for short transaction ID generation.
/// Uses the same algorithm as Bitcoin BIP-152.
pub struct SipHasher {
    v0: u64,
    v1: u64,
    v2: u64,
    v3: u64,
}

impl SipHasher {
    /// Create a new SipHasher with the given 128-bit key.
    pub fn new(k0: u64, k1: u64) -> Self {
        Self {
            v0: k0 ^ 0x736f6d6570736575,
            v1: k1 ^ 0x646f72616e646f6d,
            v2: k0 ^ 0x6c7967656e657261,
            v3: k1 ^ 0x7465646279746573,
        }
    }

    fn compress(&mut self) {
        self.v0 = self.v0.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(13) ^ self.v0;
        self.v0 = self.v0.rotate_left(32);
        self.v2 = self.v2.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(16) ^ self.v2;
        self.v0 = self.v0.wrapping_add(self.v3);
        self.v3 = self.v3.rotate_left(21) ^ self.v0;
        self.v2 = self.v2.wrapping_add(self.v1);
        self.v1 = self.v1.rotate_left(17) ^ self.v2;
        self.v2 = self.v2.rotate_left(32);
    }

    /// Hash 8 bytes of data.
    pub fn hash(&mut self, data: &[u8; 8]) -> u64 {
        let m = u64::from_le_bytes(*data);
        self.v3 ^= m;
        self.compress();
        self.compress();
        self.v0 ^= m;

        // Finalization
        self.v2 ^= 0xff;
        self.compress();
        self.compress();
        self.compress();
        self.compress();
        self.v0 ^ self.v1 ^ self.v2 ^ self.v3
    }

    /// Hash a 32-byte value (four 8-byte blocks) and finalize.
    pub fn hash32(&mut self, data: &[u8; 32]) -> u64 {
        for chunk in data.chunks_exact(8) {
            let m = u64::from_le_bytes(chunk.try_into().unwrap());
            self.v3 ^= m;
            self.compress();
            self.compress();
            self.v0 ^= m;
        }
        // Finalization
        self.v2 ^= 0xff;
        self.compress();
        self.compress();
        self.compress();
        self.compress();
        self.v0 ^ self.v1 ^ self.v2 ^ self.v3
    }
}

/// Derive the SipHash key pair from a block header and nonce.
///
/// Per BIP-152: SHA256d(header || nonce), take first 16 bytes as k0, k1.
pub fn derive_siphash_keys(header_bytes: &[u8], nonce: u64) -> (u64, u64) {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(header_bytes);
    hasher.update(nonce.to_le_bytes());
    let first_hash = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(first_hash);
    let key = hasher2.finalize();

    let k0 = u64::from_le_bytes(key[0..8].try_into().unwrap());
    let k1 = u64::from_le_bytes(key[8..16].try_into().unwrap());
    (k0, k1)
}

/// Compute the 6-byte short transaction ID for a txid.
///
/// Per BIP-152: SipHash-2-4(txid, key) & 0x0000_FFFF_FFFF_FFFF
pub fn short_txid(txid: &[u8; 32], k0: u64, k1: u64) -> u64 {
    let mut hasher = SipHasher::new(k0, k1);
    // Hash the full 32-byte txid (BIP-152), not just the first 8 bytes.
    let full = hasher.hash32(txid);
    // Mask to 6 bytes (48 bits)
    full & 0x0000_FFFF_FFFF_FFFF
}

/// Compact block encoder — builds a `CmpctBlockMsg` from a block.
pub struct CompactBlockEncoder;

impl CompactBlockEncoder {
    /// Encode a block as a compact block message.
    ///
    /// `txids` is the list of transaction IDs in block order.
    /// `coinbase_tx_bytes` is the serialized coinbase transaction (always prefilled).
    #[allow(clippy::too_many_arguments)] // Mirrors the eight on-wire block-header fields plus tx data.
    pub fn encode(
        version: u32,
        prev_block_hash: [u8; 32],
        merkle_root: [u8; 32],
        utxo_root: [u8; 32],
        timestamp: u32,
        bits: u32,
        nonce: u32,
        stake_modifier: u64,
        txids: &[[u8; 32]],
        coinbase_tx_bytes: Vec<u8>,
    ) -> Result<CmpctBlockMsg, CompactBlockEncodeError> {
        use rand::Rng;

        // Build header bytes for key derivation (mirrors SpvHeader::hash 120-byte preimage)
        let mut header_bytes = Vec::with_capacity(120);
        header_bytes.extend_from_slice(&version.to_le_bytes());
        header_bytes.extend_from_slice(&prev_block_hash);
        header_bytes.extend_from_slice(&merkle_root);
        header_bytes.extend_from_slice(&utxo_root);
        header_bytes.extend_from_slice(&timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&bits.to_le_bytes());
        header_bytes.extend_from_slice(&nonce.to_le_bytes());
        header_bytes.extend_from_slice(&stake_modifier.to_le_bytes());

        // BIP-152 §4: the sender must guarantee short IDs are unique. On a
        // SipHash collision, retry with fresh nonces (probability of needing
        // more than a handful of rounds is negligible). The retry cap guards
        // against a caller passing duplicate txids, which would loop forever.
        const MAX_SHORTID_RETRIES: u32 = 64;
        let mut retries: u32 = 0;
        let mut siphash_nonce: u64 = rand::thread_rng().gen();
        let short_ids = loop {
            let (k0, k1) = derive_siphash_keys(&header_bytes, siphash_nonce);
            let ids: Vec<u64> = txids
                .iter()
                .skip(1)
                .map(|txid| short_txid(txid, k0, k1))
                .collect();
            let unique = ids.iter().collect::<std::collections::HashSet<_>>().len();
            if unique == ids.len() {
                break ids;
            }
            retries += 1;
            if retries >= MAX_SHORTID_RETRIES {
                return Err(CompactBlockEncodeError::ShortIdCollision);
            }
            siphash_nonce = siphash_nonce.wrapping_add(1);
        };

        // Coinbase is always prefilled at index 0
        let prefilled_txs = vec![PrefilledTx {
            index: 0,
            tx_bytes: coinbase_tx_bytes,
        }];

        Ok(CmpctBlockMsg {
            version,
            prev_block_hash,
            merkle_root,
            utxo_root,
            timestamp,
            bits,
            nonce,
            stake_modifier,
            siphash_nonce,
            short_ids,
            prefilled_txs,
        })
    }
}

/// Compact block decoder — reconstructs a full block from a compact block message.
pub struct CompactBlockDecoder;

/// Error produced when a compact block cannot be encoded.
#[derive(Debug)]
pub enum CompactBlockEncodeError {
    /// Short IDs could not be made unique after the retry budget — the input
    /// transaction list almost certainly contains duplicate txids.
    ShortIdCollision,
}

/// Error produced when a compact block cannot be fully reconstructed locally.
#[derive(Debug)]
pub enum CompactBlockDecodeError {
    /// Some transactions are missing from the mempool; the absolute indexes
    /// should be requested from the peer via `getblocktxn`.
    MissingTransactions(Vec<u16>),
    /// The compact block advertises more transactions than can be represented
    /// by the u16 `getblocktxn` index field.
    TooManyTransactions,
    /// A prefilled transaction index is out of range or duplicated — a
    /// protocol violation by the sender. Must not be silently skipped:
    /// skipping desynchronizes short-id mapping and yields phantom blocks.
    InvalidPrefilledIndex,
    /// The message contains duplicate short IDs — a BIP-152 protocol
    /// violation (sender must retry with a different nonce).
    DuplicateShortId,
}

impl CompactBlockDecoder {
    /// Try to reconstruct the full transaction list from a compact block.
    ///
    /// `mempool_txids` maps short_txid → full txid → serialized tx bytes.
    /// Returns `Ok(txs)` if all transactions were found, or
    /// `Err(missing_indexes)` if some transactions need to be fetched.
    pub fn decode(
        msg: &CmpctBlockMsg,
        mempool: &HashMap<u64, Vec<u8>>, // short_id → tx_bytes
    ) -> Result<Vec<Vec<u8>>, CompactBlockDecodeError> {
        // The `mempool` map is keyed by the BIP-152 short transaction ID, which
        // the caller computes from the block header's SipHash keys. Looking up
        // each peer-supplied short_id in this map performs the short-id matching.

        // Total transaction count = prefilled + short_ids
        let total = msg.prefilled_txs.len() + msg.short_ids.len();
        // The getblocktxn index field is a u16, so reject compact blocks that
        // advertise more transactions than can be indexed rather than silently
        // truncating indexes.
        if total > u16::MAX as usize {
            return Err(CompactBlockDecodeError::TooManyTransactions);
        }
        let mut txs: Vec<Option<Vec<u8>>> = vec![None; total];
        let mut missing: Vec<u16> = Vec::new();

        // Place prefilled transactions. Indexes are differential and must be
        // strictly increasing with in-range absolute positions; anything else
        // is a protocol violation by the sender.
        let mut offset = 0usize;
        for prefilled in &msg.prefilled_txs {
            let abs_index = offset + prefilled.index as usize;
            if abs_index >= total || txs[abs_index].is_some() {
                return Err(CompactBlockDecodeError::InvalidPrefilledIndex);
            }
            txs[abs_index] = Some(prefilled.tx_bytes.clone());
            offset = abs_index + 1;
        }

        // BIP-152 §5: duplicate short IDs in the message are a protocol
        // violation (the sender must have retried with a different nonce).
        // They would silently fill two slots with the same transaction.
        {
            let mut seen = std::collections::HashSet::new();
            for sid in &msg.short_ids {
                if !seen.insert(*sid) {
                    return Err(CompactBlockDecodeError::DuplicateShortId);
                }
            }
        }

        // Fill in short_id transactions from mempool
        let mut short_idx = 0usize;
        for (i, slot) in txs.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            if short_idx < msg.short_ids.len() {
                let sid = msg.short_ids[short_idx];
                if let Some(tx_bytes) = mempool.get(&sid) {
                    *slot = Some(tx_bytes.clone());
                } else {
                    missing.push(i as u16);
                }
                short_idx += 1;
            }
        }

        if missing.is_empty() {
            Ok(txs.into_iter().map(|t| t.unwrap_or_default()).collect())
        } else {
            Err(CompactBlockDecodeError::MissingTransactions(missing))
        }
    }

    /// Reconstruct a full block using both mempool lookups and previously received
    /// transactions from a `blocktxn` response.
    ///
    /// `mempool` maps short_txid → serialized tx bytes.
    /// `received` maps the absolute index → serialized tx bytes (from `blocktxn`).
    /// Returns `Ok(txs)` on success, or `Err(MissingTransactions)` if some are
    /// still unresolvable.
    pub fn decode_with_received(
        msg: &CmpctBlockMsg,
        mempool: &HashMap<u64, Vec<u8>>,
        received: &HashMap<usize, Vec<u8>>,
    ) -> Result<Vec<Vec<u8>>, CompactBlockDecodeError> {
        let total = msg.prefilled_txs.len() + msg.short_ids.len();
        if total > u16::MAX as usize {
            return Err(CompactBlockDecodeError::TooManyTransactions);
        }
        let mut txs: Vec<Option<Vec<u8>>> = vec![None; total];
        let mut missing: Vec<u16> = Vec::new();

        {
            let mut seen = std::collections::HashSet::new();
            for sid in &msg.short_ids {
                if !seen.insert(*sid) {
                    return Err(CompactBlockDecodeError::DuplicateShortId);
                }
            }
        }

        // Place prefilled transactions (strict validation, see `decode`).
        let mut offset = 0usize;
        for prefilled in &msg.prefilled_txs {
            let abs_index = offset + prefilled.index as usize;
            if abs_index >= total || txs[abs_index].is_some() {
                return Err(CompactBlockDecodeError::InvalidPrefilledIndex);
            }
            txs[abs_index] = Some(prefilled.tx_bytes.clone());
            offset = abs_index + 1;
        }

        // Fill in short_id transactions: try mempool first, then received map
        let mut short_idx = 0usize;
        for (i, slot) in txs.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            if short_idx < msg.short_ids.len() {
                let sid = msg.short_ids[short_idx];
                if let Some(tx_bytes) = mempool.get(&sid) {
                    *slot = Some(tx_bytes.clone());
                } else if let Some(tx_bytes) = received.get(&i) {
                    *slot = Some(tx_bytes.clone());
                } else {
                    missing.push(i as u16);
                }
                short_idx += 1;
            }
        }

        if missing.is_empty() {
            Ok(txs.into_iter().map(|t| t.unwrap_or_default()).collect())
        } else {
            Err(CompactBlockDecodeError::MissingTransactions(missing))
        }
    }

    /// Build a `getblocktxn` request for missing transactions.
    pub fn build_getblocktxn(block_hash: [u8; 32], missing_indexes: Vec<u16>) -> GetBlockTxnMsg {
        GetBlockTxnMsg {
            block_hash,
            indexes: missing_indexes,
        }
    }

    /// Build a `blocktxn` response with the requested transactions.
    pub fn build_blocktxn(block_hash: [u8; 32], transactions: Vec<Vec<u8>>) -> BlockTxnMsg {
        BlockTxnMsg {
            block_hash,
            transactions,
        }
    }
}

/// Track which peers support compact blocks and in which mode.
#[derive(Debug, Clone)]
pub struct CompactBlockPeerState {
    /// Whether this peer supports compact blocks.
    pub enabled: bool,
    /// Whether this peer is in high-bandwidth mode.
    pub high_bandwidth: bool,
    /// Protocol version negotiated.
    pub version: u64,
}

impl Default for CompactBlockPeerState {
    fn default() -> Self {
        Self {
            enabled: false,
            high_bandwidth: false,
            version: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_short_ids_rejected() {
        let cmpct = CompactBlockEncoder::encode(
            1,
            [1u8; 32],
            [2u8; 32],
            [0u8; 32],
            12345,
            0x1e0fffff,
            7,
            0,
            &[[3u8; 32], [4u8; 32]],
            vec![0x51],
        )
        .unwrap();
        // Force a duplicate short id (simulating a malicious or buggy sender).
        let mut bad = cmpct.clone();
        let dup = *bad.short_ids.first().unwrap_or(&42);
        bad.short_ids.push(dup);
        assert!(matches!(
            CompactBlockDecoder::decode(&bad, &std::collections::HashMap::new()),
            Err(CompactBlockDecodeError::DuplicateShortId)
        ));
    }

    /// Duplicate txids in the input make unique short IDs impossible; the
    /// encoder must give up after the retry budget instead of looping forever.
    #[test]
    fn duplicate_txids_shortid_collision_error() {
        let result = CompactBlockEncoder::encode(
            1,
            [1u8; 32],
            [2u8; 32],
            [0u8; 32],
            12345,
            0x1e0fffff,
            7,
            0,
            // First entry is the coinbase (skipped); the two identical
            // non-coinbase txids can never produce unique short IDs.
            &[[9u8; 32], [9u8; 32], [9u8; 32]],
            vec![0x51],
        );
        assert!(
            matches!(result, Err(CompactBlockEncodeError::ShortIdCollision)),
            "duplicate txids must surface as ShortIdCollision"
        );
    }

    #[test]
    fn test_siphash_deterministic() {
        let mut h1 = SipHasher::new(0x0706050403020100, 0x0f0e0d0c0b0a0908);
        let mut h2 = SipHasher::new(0x0706050403020100, 0x0f0e0d0c0b0a0908);
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(h1.hash(&data), h2.hash(&data));
    }

    #[test]
    fn test_siphash_different_keys() {
        let mut h1 = SipHasher::new(0, 0);
        let mut h2 = SipHasher::new(1, 0);
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_ne!(h1.hash(&data), h2.hash(&data));
    }

    #[test]
    fn test_short_txid_6_bytes() {
        let txid = [0u8; 32];
        let sid = short_txid(&txid, 0, 0);
        // Must fit in 6 bytes (48 bits)
        assert_eq!(sid & 0xFFFF_0000_0000_0000, 0);
    }

    #[test]
    fn test_compact_block_encode_decode_all_in_mempool() {
        let coinbase_txid = [1u8; 32];
        let tx1_txid = [2u8; 32];
        let tx2_txid = [3u8; 32];
        let txids = vec![coinbase_txid, tx1_txid, tx2_txid];

        let coinbase_bytes = vec![0xCB; 100];
        let tx1_bytes = vec![0x01; 50];
        let tx2_bytes = vec![0x02; 50];

        let msg = CompactBlockEncoder::encode(
            1,
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            1000,
            0x1d00ffff,
            42,
            0,
            &txids,
            coinbase_bytes.clone(),
        )
        .unwrap();

        assert_eq!(msg.short_ids.len(), 2); // tx1 and tx2
        assert_eq!(msg.prefilled_txs.len(), 1); // coinbase only

        // Build mempool with short IDs
        let mut header_bytes = Vec::with_capacity(120);
        header_bytes.extend_from_slice(&msg.version.to_le_bytes());
        header_bytes.extend_from_slice(&msg.prev_block_hash);
        header_bytes.extend_from_slice(&msg.merkle_root);
        header_bytes.extend_from_slice(&msg.utxo_root);
        header_bytes.extend_from_slice(&msg.timestamp.to_le_bytes());
        header_bytes.extend_from_slice(&msg.bits.to_le_bytes());
        header_bytes.extend_from_slice(&msg.nonce.to_le_bytes());
        header_bytes.extend_from_slice(&msg.stake_modifier.to_le_bytes());
        let (k0, k1) = derive_siphash_keys(&header_bytes, msg.siphash_nonce);

        let mut mempool = HashMap::new();
        mempool.insert(short_txid(&tx1_txid, k0, k1), tx1_bytes.clone());
        mempool.insert(short_txid(&tx2_txid, k0, k1), tx2_bytes.clone());

        let result = CompactBlockDecoder::decode(&msg, &mempool);
        assert!(result.is_ok());
        let txs = result.unwrap();
        assert_eq!(txs.len(), 3);
        assert_eq!(txs[0], coinbase_bytes);
    }

    #[test]
    fn test_compact_block_missing_tx() {
        let coinbase_txid = [1u8; 32];
        let tx1_txid = [2u8; 32];
        let txids = vec![coinbase_txid, tx1_txid];

        let msg = CompactBlockEncoder::encode(
            1,
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            1000,
            0x1d00ffff,
            42,
            0,
            &txids,
            vec![0xCB; 10],
        )
        .unwrap();

        // Empty mempool — tx1 is missing
        let mempool = HashMap::new();
        let result = CompactBlockDecoder::decode(&msg, &mempool);
        assert!(result.is_err());
        let missing = result.unwrap_err();
        match missing {
            CompactBlockDecodeError::MissingTransactions(indexes) => {
                assert_eq!(indexes.len(), 1);
            }
            _ => panic!("expected MissingTransactions"),
        }
    }

    #[test]
    fn test_derive_siphash_keys_deterministic() {
        let header = vec![0u8; 80];
        let (k0a, k1a) = derive_siphash_keys(&header, 42);
        let (k0b, k1b) = derive_siphash_keys(&header, 42);
        assert_eq!(k0a, k0b);
        assert_eq!(k1a, k1b);
    }

    #[test]
    fn test_derive_siphash_keys_different_nonce() {
        let header = vec![0u8; 80];
        let (k0a, _) = derive_siphash_keys(&header, 1);
        let (k0b, _) = derive_siphash_keys(&header, 2);
        assert_ne!(k0a, k0b);
    }

    #[test]
    fn test_getblocktxn_builder() {
        let hash = [0xABu8; 32];
        let msg = CompactBlockDecoder::build_getblocktxn(hash, vec![1, 3, 5]);
        assert_eq!(msg.block_hash, hash);
        assert_eq!(msg.indexes, vec![1, 3, 5]);
    }

    #[test]
    fn test_compact_block_multiple_missing_txs() {
        let coinbase_txid = [1u8; 32];
        let txids: Vec<[u8; 32]> = (0..10).map(|i| [i as u8 + 10; 32]).collect();
        let all_txids = std::iter::once(coinbase_txid)
            .chain(txids.iter().copied())
            .collect::<Vec<_>>();

        let msg = CompactBlockEncoder::encode(
            2,
            [0xAA; 32],
            [0xBB; 32],
            [0u8; 32],
            5000,
            0x1d00ffff,
            99,
            0,
            &all_txids,
            vec![0xCB; 10],
        )
        .unwrap();

        // Only provide txids [10] and [13] in mempool — rest are missing.
        let header_bytes = {
            let mut h = Vec::with_capacity(120);
            h.extend_from_slice(&msg.version.to_le_bytes());
            h.extend_from_slice(&msg.prev_block_hash);
            h.extend_from_slice(&msg.merkle_root);
            h.extend_from_slice(&msg.utxo_root);
            h.extend_from_slice(&msg.timestamp.to_le_bytes());
            h.extend_from_slice(&msg.bits.to_le_bytes());
            h.extend_from_slice(&msg.nonce.to_le_bytes());
            h.extend_from_slice(&msg.stake_modifier.to_le_bytes());
            h
        };
        let (k0, k1) = derive_siphash_keys(&header_bytes, msg.siphash_nonce);

        let mut mempool = HashMap::new();
        // Only put 2 of 10 txs in the mempool.
        let have_txids = [txids[1], txids[4]];
        for tid in &have_txids {
            mempool.insert(short_txid(tid, k0, k1), vec![0x01; 30]);
        }

        let result = CompactBlockDecoder::decode(&msg, &mempool);
        assert!(result.is_err());
        match result.unwrap_err() {
            CompactBlockDecodeError::MissingTransactions(indexes) => {
                assert_eq!(indexes.len(), 8, "should be 8 missing txs");
            }
            _ => panic!("expected MissingTransactions"),
        }
    }

    #[test]
    fn test_compact_block_empty_block_only_coinbase() {
        let coinbase_txid = [0xFF; 32];
        let txids = vec![coinbase_txid];

        let msg = CompactBlockEncoder::encode(
            1,
            [0u8; 32],
            [0u8; 32],
            [0u8; 32],
            1000,
            0x1d00ffff,
            0,
            0,
            &txids,
            vec![0xCB; 50],
        )
        .unwrap();

        assert_eq!(msg.short_ids.len(), 0, "no non-coinbase txs");
        assert_eq!(msg.prefilled_txs.len(), 1, "coinbase prefilled");

        let mempool = HashMap::new();
        let result = CompactBlockDecoder::decode(&msg, &mempool);
        assert!(
            result.is_ok(),
            "empty block with only coinbase should decode"
        );
        let txs = result.unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0], vec![0xCB; 50]);
    }

    #[test]
    fn test_getblocktxn_builder_empty_indexes() {
        let hash = [0x42u8; 32];
        let msg = CompactBlockDecoder::build_getblocktxn(hash, vec![]);
        assert_eq!(msg.block_hash, hash);
        assert!(msg.indexes.is_empty());
    }

    /// Regression for the blocktxn index-mapping bug: the response lists
    /// transactions positionally (in requested order) while decode_with_received
    /// looks them up by absolute block index. With non-contiguous missing
    /// indexes the mapping must place each tx at its absolute position.
    #[test]
    fn test_decode_with_received_maps_absolute_indexes() {
        let coinbase_txid = [1u8; 32];
        let txids: Vec<[u8; 32]> = (0..6).map(|i| [i as u8 + 20; 32]).collect();
        let all_txids = std::iter::once(coinbase_txid)
            .chain(txids.iter().copied())
            .collect::<Vec<_>>();

        let msg = CompactBlockEncoder::encode(
            2,
            [0xCC; 32],
            [0xDD; 32],
            [0u8; 32],
            6000,
            0x1d00ffff,
            42,
            0,
            &all_txids,
            vec![0xEE; 10],
        )
        .unwrap();

        // Mempool has nothing — every non-coinbase tx is missing.
        let missing: Vec<u16> = match CompactBlockDecoder::decode(&msg, &HashMap::new()) {
            Err(CompactBlockDecodeError::MissingTransactions(indexes)) => indexes,
            other => panic!("expected MissingTransactions, got {:?}", other.map(|_| ())),
        };
        // Absolute indexes 1..=6 (coinbase is prefilled).
        assert_eq!(missing, vec![1u16, 2, 3, 4, 5, 6]);

        // Simulate a blocktxn response: txs in REQUESTED order (positional).
        let requested: Vec<usize> = missing.iter().map(|&i| i as usize).collect();
        let mut received: HashMap<usize, Vec<u8>> = HashMap::new();
        for (pos, &abs_index) in requested.iter().enumerate() {
            // Distinct payload per absolute index so we can verify placement.
            received.insert(pos, vec![abs_index as u8; 12]);
        }

        // Map positions to absolute indexes exactly as handle_blocktxn now does.
        let mut mapped: HashMap<usize, Vec<u8>> = HashMap::new();
        for (pos, &abs_index) in requested.iter().enumerate() {
            if let Some(bytes) = received.get(&pos) {
                mapped.insert(abs_index, bytes.clone());
            }
        }

        let txs = CompactBlockDecoder::decode_with_received(&msg, &HashMap::new(), &mapped)
            .expect("reconstruction must succeed with correctly mapped indexes");
        assert_eq!(txs.len(), 7); // coinbase + 6
                                  // Coinbase prefilled at 0; each absolute index carries its marker byte.
        for (i, tx) in txs.iter().enumerate() {
            if i == 0 {
                assert_eq!(tx, &vec![0xEE; 10]);
            } else {
                assert_eq!(
                    tx,
                    &vec![i as u8; 12],
                    "tx at absolute index {} misplaced",
                    i
                );
            }
        }
    }
}

// (appended in tests module below)
