pub mod ban_manager;
pub mod codec;
pub mod compact;
pub mod dht;
/// vTorrent P2P Networking Layer
///
/// Implements the peer-to-peer network protocol for the vTorrent 2.0 chain.
/// Uses tokio for async I/O and a custom binary message framing protocol.
pub mod error;
pub mod message;
pub mod peer;
pub mod peer_manager;
pub mod pex;
