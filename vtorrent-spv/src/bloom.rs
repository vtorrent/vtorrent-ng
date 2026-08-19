//! BIP-37 style Bloom filter for SPV transaction filtering.
//!
//! A Bloom filter is a probabilistic data structure that answers "is this item
//! in the set?" with no false negatives and a tunable false positive rate.
//! Light clients load a Bloom filter onto full nodes to receive only the
//! transactions relevant to their wallet addresses.
//!
//! # Design
//! - Uses `k` independent hash functions derived from a single Murmur3 seed
//! - Filter size and hash count are chosen to achieve a target false-positive rate
//! - Supports `BLOOM_UPDATE_ALL` and `BLOOM_UPDATE_NONE` flags (BIP-37)

use serde::{Deserialize, Serialize};

/// Maximum filter size in bytes (36,000 bytes = 288,000 bits, per BIP-37).
pub const MAX_BLOOM_FILTER_SIZE: usize = 36_000;

/// Maximum number of hash functions (50, per BIP-37).
pub const MAX_HASH_FUNCS: u32 = 50;

/// Murmur3 rotation constant.
const C1: u32 = 0xcc9e2d51;
const C2: u32 = 0x1b873593;

/// Controls which outpoints are added to the filter when a match is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BloomFlags {
    /// Do not update the filter when a match is found.
    None = 0,
    /// Add all outpoints of matched transactions to the filter.
    All = 1,
    /// Add only P2PKH outpoints of matched transactions.
    PubKeyOnly = 2,
}

/// A probabilistic set membership filter (BIP-37 compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloomFilter {
    /// The filter bit array, stored as bytes.
    data: Vec<u8>,
    /// Number of hash functions to apply.
    hash_funcs: u32,
    /// Random tweak added to each hash to prevent filter fingerprinting.
    tweak: u32,
    /// Update flag controlling outpoint insertion behaviour.
    pub flags: BloomFlags,
}

impl BloomFilter {
    /// Create a new Bloom filter sized to hold `n_elements` items with at most
    /// `false_positive_rate` false positives (0.0–1.0).
    ///
    /// # Example
    /// ```
    /// use vtorrent_spv::BloomFilter;
    /// let filter = BloomFilter::new(1000, 0.001, 42);
    /// ```
    pub fn new(n_elements: usize, false_positive_rate: f64, tweak: u32) -> Self {
        // Optimal filter size: m = -n * ln(p) / (ln(2)^2)
        let ln2_sq = std::f64::consts::LN_2 * std::f64::consts::LN_2;
        let size_bits = (-(n_elements as f64) * false_positive_rate.ln() / ln2_sq) as usize;
        let size_bytes = size_bits.div_ceil(8).min(MAX_BLOOM_FILTER_SIZE);

        // Optimal hash count: k = m/n * ln(2)
        let hash_funcs =
            ((size_bytes * 8) as f64 / n_elements as f64 * std::f64::consts::LN_2).round() as u32;
        let hash_funcs = hash_funcs.clamp(1, MAX_HASH_FUNCS);

        Self {
            data: vec![0u8; size_bytes],
            hash_funcs,
            tweak,
            flags: BloomFlags::All,
        }
    }

    /// Create a Bloom filter from raw wire bytes (as received in a `filterload` message).
    pub fn from_bytes(data: Vec<u8>, hash_funcs: u32, tweak: u32, flags: u8) -> Self {
        let flags = match flags {
            1 => BloomFlags::All,
            2 => BloomFlags::PubKeyOnly,
            _ => BloomFlags::None,
        };
        Self {
            data,
            hash_funcs,
            tweak,
            flags,
        }
    }

    /// Insert an item into the filter.
    pub fn insert(&mut self, item: &[u8]) {
        let bits = self.data.len().saturating_mul(8);
        if bits == 0 {
            return;
        }
        for i in 0..self.hash_funcs {
            let bit = self.hash(item, i) as usize % bits;
            self.data[bit / 8] |= 1 << (bit % 8);
        }
    }

    /// Insert a 32-byte hash (txid, script hash, etc.) into the filter.
    pub fn insert_hash(&mut self, hash: &[u8; 32]) {
        self.insert(hash);
    }

    /// Insert a Bitcoin-style address (as raw script bytes) into the filter.
    pub fn insert_script(&mut self, script: &[u8]) {
        self.insert(script);
    }

    /// Test whether an item is (probably) in the filter.
    ///
    /// Returns `false` with certainty if the item is not present.
    /// Returns `true` if the item is present or (with low probability) a false positive.
    pub fn contains(&self, item: &[u8]) -> bool {
        let bits = self.data.len().saturating_mul(8);
        if bits == 0 {
            return false;
        }
        for i in 0..self.hash_funcs {
            let bit = self.hash(item, i) as usize % bits;
            if self.data[bit / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    /// Test whether a 32-byte hash is (probably) in the filter.
    pub fn contains_hash(&self, hash: &[u8; 32]) -> bool {
        self.contains(hash)
    }

    /// Returns true if the filter is empty (no items inserted).
    pub fn is_empty(&self) -> bool {
        self.data.iter().all(|&b| b == 0)
    }

    /// Returns true if the filter is full (all bits set — matches everything).
    pub fn is_full(&self) -> bool {
        self.data.iter().all(|&b| b == 0xff)
    }

    /// Serialize to wire format (for `filterload` P2P message).
    pub fn to_wire(&self) -> (Vec<u8>, u32, u32, u8) {
        (
            self.data.clone(),
            self.hash_funcs,
            self.tweak,
            self.flags as u8,
        )
    }

    /// Returns the filter size in bytes.
    pub fn size_bytes(&self) -> usize {
        self.data.len()
    }

    /// Returns the number of hash functions.
    pub fn hash_funcs(&self) -> u32 {
        self.hash_funcs
    }

    /// Murmur3 hash function (BIP-37 specifies this exact variant).
    fn hash(&self, data: &[u8], hash_num: u32) -> u32 {
        let seed = hash_num.wrapping_mul(0xfba4c795).wrapping_add(self.tweak);
        murmur3(data, seed)
    }
}

/// MurmurHash3 (32-bit) — the hash function specified by BIP-37.
fn murmur3(data: &[u8], seed: u32) -> u32 {
    let mut h1 = seed;
    let nblocks = data.len() / 4;

    // Body
    for i in 0..nblocks {
        let k1 = u32::from_le_bytes([
            data[i * 4],
            data[i * 4 + 1],
            data[i * 4 + 2],
            data[i * 4 + 3],
        ]);
        let k1 = k1.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h1 ^= k1;
        h1 = h1.rotate_left(13).wrapping_mul(5).wrapping_add(0xe6546b64);
    }

    // Tail
    let tail = &data[nblocks * 4..];
    let mut k1: u32 = 0;
    match tail.len() {
        3 => {
            k1 ^= (tail[2] as u32) << 16;
            k1 ^= (tail[1] as u32) << 8;
            k1 ^= tail[0] as u32;
        }
        2 => {
            k1 ^= (tail[1] as u32) << 8;
            k1 ^= tail[0] as u32;
        }
        1 => {
            k1 ^= tail[0] as u32;
        }
        _ => {}
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(C1).rotate_left(15).wrapping_mul(C2);
        h1 ^= k1;
    }

    // Finalization
    h1 ^= data.len() as u32;
    h1 ^= h1 >> 16;
    h1 = h1.wrapping_mul(0x85ebca6b);
    h1 ^= h1 >> 13;
    h1 = h1.wrapping_mul(0xc2b2ae35);
    h1 ^= h1 >> 16;
    h1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_contains() {
        let mut f = BloomFilter::new(100, 0.01, 0);
        let item = b"vtorrent_address_abc123";
        assert!(!f.contains(item));
        f.insert(item);
        assert!(f.contains(item));
    }

    #[test]
    fn test_not_contains_other_item() {
        let mut f = BloomFilter::new(100, 0.001, 99);
        f.insert(b"item_one");
        // With very low FP rate, this should not match
        assert!(!f.contains(b"item_two_completely_different_xyz"));
    }

    #[test]
    fn test_insert_hash() {
        let mut f = BloomFilter::new(50, 0.01, 7);
        let hash = [0xabu8; 32];
        f.insert_hash(&hash);
        assert!(f.contains_hash(&hash));
        assert!(!f.contains_hash(&[0u8; 32]));
    }

    #[test]
    fn test_empty_filter() {
        let f = BloomFilter::new(100, 0.01, 0);
        assert!(f.is_empty());
    }

    #[test]
    fn test_full_filter() {
        let f = BloomFilter::from_bytes(vec![0xff; 10], 1, 0, 1);
        assert!(f.is_full());
        assert!(f.contains(b"anything"));
    }

    #[test]
    fn test_wire_roundtrip() {
        let mut f = BloomFilter::new(200, 0.001, 42);
        f.insert(b"address1");
        f.insert(b"address2");
        let (data, hf, tweak, flags) = f.to_wire();
        let f2 = BloomFilter::from_bytes(data, hf, tweak, flags);
        assert!(f2.contains(b"address1"));
        assert!(f2.contains(b"address2"));
    }

    #[test]
    fn test_murmur3_known_value() {
        // BIP-37 test vector: murmur3("", seed=0) = 0
        assert_eq!(murmur3(b"", 0), 0);
    }

    #[test]
    fn test_filter_size_reasonable() {
        let f = BloomFilter::new(1000, 0.001, 0);
        // For 1000 items at 0.1% FP, filter should be ~1.8 KB
        assert!(f.size_bytes() > 1000);
        assert!(f.size_bytes() <= MAX_BLOOM_FILTER_SIZE);
    }
}
