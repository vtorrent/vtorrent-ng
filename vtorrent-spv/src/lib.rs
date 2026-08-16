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
//! - **Compact block filters** (BIP-157/158 style) — a more modern, privacy-
//!   preserving alternative to Bloom filters where the filter is computed by
//!   the full node and the client downloads it.
//! - **SPV chain** — a lightweight chain that stores only block headers (80 bytes
//!   each) and validates proof-of-work/proof-of-stake without storing full blocks.

pub mod bloom;
pub mod error;
pub mod filter;
pub mod merkle;
pub mod spv_chain;

pub use bloom::BloomFilter;
pub use error::SpvError;
pub use filter::{BlockFilter, FilterMatcher};
pub use merkle::{MerkleProof, MerkleTree};
pub use spv_chain::{SpvChain, SpvHeader};
