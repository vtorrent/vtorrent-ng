//! Bitcoin SPV wallet for vTorrent.
//!
//! Provides BIP84 native SegWit key derivation, a header-chain store,
//! merkle-proof verification, UTXO tracking, transaction building/signing,
//! and a minimal Bitcoin P2P client.

pub mod error;
pub mod headers;
pub mod htlc;
pub mod keys;
pub mod merkle;
pub mod p2p;
pub mod sync;
pub mod tx;
pub mod utxo;
pub mod wallet;
