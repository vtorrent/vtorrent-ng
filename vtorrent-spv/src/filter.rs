//! Compact block filters (BIP-157/158 style).
//!
//! Unlike BIP-37 Bloom filters (which are loaded onto full nodes), compact block
//! filters are computed by full nodes and downloaded by light clients. This is
//! more privacy-preserving because the full node never learns which addresses
//! the client is watching.
//!
//! # How It Works
//! 1. The full node builds a `BlockFilter` for each block containing all
//!    scriptPubKeys of spent and created outputs.
//! 2. The light client downloads the filter (a few hundred bytes per block).
//! 3. The client tests whether any of its watched addresses match the filter.
//! 4. If yes, the client downloads the full block to find its transactions.
//! 5. If no, the client skips the block entirely.
//!
//! This approach means the full node never sees the client's addresses.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A compact block filter for a single block.
///
/// Encodes all scriptPubKeys of outputs created and spent in the block
/// using a Golomb-Rice coded set (GCS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockFilter {
    /// The block hash this filter covers.
    pub block_hash: [u8; 32],
    /// The block height.
    pub height: u32,
    /// The encoded GCS filter bytes.
    pub filter_bytes: Vec<u8>,
    /// Number of elements encoded in the filter.
    pub n_elements: u32,
    /// The SipHash key derived from the block hash (first 16 bytes).
    key: [u8; 16],
}

impl BlockFilter {
    /// Build a compact block filter from a list of scriptPubKeys.
    ///
    /// `scripts` should contain all scriptPubKeys of outputs created and
    /// consumed in the block (both inputs' previous outputs and new outputs).
    pub fn build(block_hash: [u8; 32], height: u32, scripts: &[Vec<u8>]) -> Self {
        // Derive SipHash key from block hash (first 16 bytes)
        let mut key = [0u8; 16];
        key.copy_from_slice(&block_hash[..16]);

        // Hash all scripts using SipHash, then reduce modulo N*M per BIP-158.
        // This bounds all values to [0, N * 2^P) so Golomb-Rice quotients stay small.
        let n = scripts.len().max(1) as u64;
        let modulus = n.saturating_mul(1u64 << GCS_P);

        let mut hashes: Vec<u64> = scripts.iter().map(|s| siphash(&key, s) % modulus).collect();

        // Deduplicate and sort for GCS encoding
        hashes.sort_unstable();
        hashes.dedup();

        let n_elements = hashes.len() as u32;
        let filter_bytes = gcs_encode(&hashes, GCS_P);

        Self {
            block_hash,
            height,
            filter_bytes,
            n_elements,
            key,
        }
    }

    /// Test whether any of the given scripts match this filter.
    ///
    /// Returns `true` if at least one script is (probably) in the filter.
    /// False positives are possible at rate 1/2^P.
    pub fn match_any(&self, scripts: &[Vec<u8>]) -> bool {
        if scripts.is_empty() || self.filter_bytes.is_empty() {
            return false;
        }

        let decoded = gcs_decode(&self.filter_bytes, self.n_elements as usize, GCS_P);

        // Use the same modulus as build() — N * 2^P
        let n = self.n_elements.max(1) as u64;
        let modulus = n.saturating_mul(1u64 << GCS_P);

        for script in scripts {
            let h = siphash(&self.key, script) % modulus;
            if decoded.binary_search(&h).is_ok() {
                return true;
            }
        }
        false
    }

    /// Returns the filter size in bytes.
    pub fn size_bytes(&self) -> usize {
        self.filter_bytes.len()
    }

    /// Compute the filter hash (used to chain filter headers).
    pub fn filter_hash(&self) -> [u8; 32] {
        Sha256::digest(Sha256::digest(&self.filter_bytes)).into()
    }
}

/// A matcher that holds a set of watched scripts and tests them against filters.
#[derive(Debug, Default)]
pub struct FilterMatcher {
    /// Scripts (scriptPubKeys) to watch.
    scripts: Vec<Vec<u8>>,
}

impl FilterMatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a scriptPubKey to watch.
    pub fn add_script(&mut self, script: Vec<u8>) {
        if !self.scripts.contains(&script) {
            self.scripts.push(script);
        }
    }

    /// Add a P2PKH script for a given public key hash.
    pub fn add_p2pkh(&mut self, pubkey_hash: &[u8; 20]) {
        // OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(pubkey_hash);
        script.extend_from_slice(&[0x88, 0xac]);
        self.add_script(script);
    }

    /// Test a block filter against all watched scripts.
    pub fn matches(&self, filter: &BlockFilter) -> bool {
        filter.match_any(&self.scripts)
    }

    /// Returns the number of watched scripts.
    pub fn len(&self) -> usize {
        self.scripts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }
}

// ── GCS (Golomb-Rice Coded Set) ───────────────────────────────────────────────

/// BIP-158 parameter P: false positive rate = 1/2^P.
const GCS_P: u8 = 19;

/// Encode a sorted, deduplicated list of u64 values as a GCS.
fn gcs_encode(values: &[u64], p: u8) -> Vec<u8> {
    if values.is_empty() {
        return vec![];
    }

    let mut bits = BitWriter::new();
    let mut prev = 0u64;

    for &v in values {
        let delta = v - prev;
        prev = v;

        // Golomb-Rice encode delta: quotient in unary, remainder in binary
        let q = delta >> p;
        let r = delta & ((1u64 << p) - 1);

        // Write q ones followed by a zero
        for _ in 0..q {
            bits.write_bit(1);
        }
        bits.write_bit(0);

        // Write p-bit remainder
        for i in (0..p).rev() {
            bits.write_bit(((r >> i) & 1) as u8);
        }
    }

    bits.finish()
}

/// Decode a GCS back to a sorted list of u64 values.
fn gcs_decode(data: &[u8], n: usize, p: u8) -> Vec<u64> {
    let mut bits = BitReader::new(data);
    let mut values = Vec::with_capacity(n);
    let mut acc = 0u64;

    for _ in 0..n {
        // Read unary quotient
        let mut q = 0u64;
        while bits.read_bit() == 1 {
            q += 1;
            if q > 1_000_000 {
                break;
            } // safety limit
        }

        // Read p-bit remainder
        let mut r = 0u64;
        for _ in 0..p {
            r = (r << 1) | bits.read_bit() as u64;
        }

        let delta = (q << p) | r;
        acc += delta;
        values.push(acc);
    }

    values
}

// ── SipHash-2-4 ──────────────────────────────────────────────────────────────

fn siphash(key: &[u8; 16], data: &[u8]) -> u64 {
    let k0 = u64::from_le_bytes(key[..8].try_into().unwrap());
    let k1 = u64::from_le_bytes(key[8..].try_into().unwrap());

    let mut v0 = k0 ^ 0x736f6d6570736575u64;
    let mut v1 = k1 ^ 0x646f72616e646f6du64;
    let mut v2 = k0 ^ 0x6c7967656e657261u64;
    let mut v3 = k1 ^ 0x7465646279746573u64;

    let mut m: u64;
    let length = data.len();
    let blocks = length / 8;

    for i in 0..blocks {
        m = u64::from_le_bytes(data[i * 8..(i + 1) * 8].try_into().unwrap());
        v3 ^= m;
        for _ in 0..2 {
            sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
        }
        v0 ^= m;
    }

    let tail = &data[blocks * 8..];
    m = (length as u64 & 0xff) << 56;
    for (i, &b) in tail.iter().enumerate() {
        m |= (b as u64) << (i * 8);
    }

    v3 ^= m;
    for _ in 0..2 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^= m;

    v2 ^= 0xff;
    for _ in 0..4 {
        sip_round(&mut v0, &mut v1, &mut v2, &mut v3);
    }

    v0 ^ v1 ^ v2 ^ v3
}

#[inline]
fn sip_round(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v1 = v1.rotate_left(13);
    *v1 ^= *v0;
    *v0 = v0.rotate_left(32);
    *v2 = v2.wrapping_add(*v3);
    *v3 = v3.rotate_left(16);
    *v3 ^= *v2;
    *v0 = v0.wrapping_add(*v3);
    *v3 = v3.rotate_left(21);
    *v3 ^= *v0;
    *v2 = v2.wrapping_add(*v1);
    *v1 = v1.rotate_left(17);
    *v1 ^= *v2;
    *v2 = v2.rotate_left(32);
}

// ── Bit I/O helpers ───────────────────────────────────────────────────────────

struct BitWriter {
    buf: Vec<u8>,
    bit_pos: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: vec![0],
            bit_pos: 0,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        if self.bit_pos == 8 {
            self.buf.push(0);
            self.bit_pos = 0;
        }
        let last = self.buf.last_mut().unwrap();
        *last |= (bit & 1) << (7 - self.bit_pos);
        self.bit_pos += 1;
    }

    fn finish(self) -> Vec<u8> {
        self.buf
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read_bit(&mut self) -> u8 {
        if self.byte_pos >= self.data.len() {
            return 0;
        }
        let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p2pkh_script(hash: &[u8; 20]) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(hash);
        s.extend_from_slice(&[0x88, 0xac]);
        s
    }

    #[test]
    fn test_filter_build_and_match() {
        let hash = [0xabu8; 20];
        let script = p2pkh_script(&hash);
        let block_hash = [1u8; 32];
        let filter = BlockFilter::build(block_hash, 100, std::slice::from_ref(&script));
        assert!(filter.match_any(&[script]));
    }

    #[test]
    fn test_filter_no_match() {
        let hash1 = [0x01u8; 20];
        let hash2 = [0x02u8; 20];
        let script1 = p2pkh_script(&hash1);
        let script2 = p2pkh_script(&hash2);
        let filter = BlockFilter::build([0u8; 32], 0, &[script1]);
        // script2 should not match (with overwhelming probability)
        assert!(!filter.match_any(&[script2]));
    }

    #[test]
    fn test_filter_matcher_p2pkh() {
        let hash = [0x33u8; 20];
        let script = p2pkh_script(&hash);
        let block_hash = [5u8; 32];
        let filter = BlockFilter::build(block_hash, 50, &[script]);

        let mut matcher = FilterMatcher::new();
        matcher.add_p2pkh(&hash);
        assert!(matcher.matches(&filter));
    }

    #[test]
    fn test_filter_matcher_no_match() {
        let hash1 = [0x11u8; 20];
        let hash2 = [0x22u8; 20];
        let script1 = p2pkh_script(&hash1);
        let filter = BlockFilter::build([0u8; 32], 0, &[script1]);

        let mut matcher = FilterMatcher::new();
        matcher.add_p2pkh(&hash2);
        assert!(!matcher.matches(&filter));
    }

    #[test]
    fn test_gcs_roundtrip() {
        let mut values: Vec<u64> = (0..50).map(|i| i * 1000 + 7).collect();
        values.sort();
        values.dedup();
        let encoded = gcs_encode(&values, GCS_P);
        let decoded = gcs_decode(&encoded, values.len(), GCS_P);
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_empty_filter() {
        let filter = BlockFilter::build([0u8; 32], 0, &[]);
        assert!(!filter.match_any(&[vec![0x76, 0xa9]]));
    }

    #[test]
    fn test_filter_size_compact() {
        let scripts: Vec<Vec<u8>> = (0..100u8)
            .map(|i| {
                let mut h = [0u8; 20];
                h[0] = i;
                p2pkh_script(&h)
            })
            .collect();
        let filter = BlockFilter::build([0u8; 32], 0, &scripts);
        // 100 scripts should produce a filter well under 1 KB
        assert!(filter.size_bytes() < 1024);
    }
}
