//! # vtorrent-spv
//!
//! Simplified Payment Verification (SPV) and Bloom filter support for vTorrent
//! light clients. Implements:
//!
//! - **Bloom filters** (BIP-37 style) — compact probabilistic data structures
//!   that let light clients tell full nodes which transactions they care about
//!   without revealing their exact addresses.
//! - **Merkle proofs** — cryptographic proofs that a transaction is included in
//!   a block without needing the full block.
//! - **SPV chain** — a lightweight chain that stores only block headers (80 bytes
//!   each) and validates proof-of-work/proof-of-stake without storing full blocks.
//!
//! Note: compact block filters (BIP-157/158) are implemented in `vtorrent-btc`
//! using the `bitcoin` crate's `bip158` module, not here.

pub mod bloom;
pub mod error;
pub mod merkle;
pub mod spv_chain;
pub mod stake;

pub use bloom::BloomFilter;
pub use error::SpvError;
pub use merkle::{MerkleProof, MerkleTree};
pub use spv_chain::{SpvChain, SpvHeader};
