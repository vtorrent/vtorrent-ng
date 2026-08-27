// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! Shim — re-exports split node modules for one release.
//! Remove next release — `vtorrent_node::node` is now `vtorrent_node::node::*` via `node/mod.rs`.
pub mod chain;
pub mod p2p;
pub mod mempool_bridge;
pub use self::chain::*;
pub use self::p2p::*;
pub use self::mempool_bridge::*;
// shim — remove next release
