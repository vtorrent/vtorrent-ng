/// vtorrent-core: Core cryptographic primitives for the vTorrent blockchain.
///
/// This crate provides:
/// - Address encoding/decoding (legacy VTR format + new format)
/// - Private/public key types with zeroize-on-drop
/// - Hash functions (SHA256d, RIPEMD160, Hash160)
/// - Transaction and UTXO types
/// - Network constants
pub mod address;
pub mod crypto;
pub mod error;
pub mod keys;
pub mod network;
pub mod time;
