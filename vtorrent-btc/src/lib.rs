//! Bitcoin SPV wallet for vTorrent.
//!
//! Provides BIP84 native SegWit key derivation, a header-chain store,
//! merkle-proof verification, UTXO tracking, transaction building/signing,
//! and a minimal Bitcoin P2P client.

pub mod error;
pub mod headers;
pub mod keys;
pub mod merkle;
pub mod tx;
pub mod utxo;
