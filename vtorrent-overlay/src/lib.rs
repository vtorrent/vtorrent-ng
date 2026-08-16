/// vtorrent-overlay — Lightweight NAT traversal overlay for vTorrent nodes
///
/// Architecture:
/// ┌─────────────────────────────────────────────────────────────────────┐
/// │  Application layer  (vtorrent-node / vtorrent-daemon)               │
/// ├─────────────────────────────────────────────────────────────────────┤
/// │  Overlay layer      (this crate)                                    │
/// │  ┌──────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────┐ │
/// │  │  STUN    │  │  Hole punch  │  │  Rendezvous  │  │  Relay     │ │
/// │  │  (ext IP)│  │  (NAT bypass)│  │  (DHT/PEX)   │  │  (fallback)│ │
/// │  └──────────┘  └──────────────┘  └──────────────┘  └────────────┘ │
/// │  ┌──────────────────────────────────────────────────────────────┐  │
/// │  │  Encrypted transport  (X25519 + ChaCha20-Poly1305)           │  │
/// │  └──────────────────────────────────────────────────────────────┘  │
/// └─────────────────────────────────────────────────────────────────────┘
///
/// Key properties:
/// - Zero central infrastructure required
/// - Works through NAT, CGNAT, and most firewalls
/// - End-to-end encrypted (WireGuard-equivalent crypto, pure Rust userspace)
/// - Falls back to TCP relay through a mutual peer when UDP is blocked
/// - Node identity is a Curve25519 public key (32 bytes)
pub mod crypto;
pub mod endpoint;
pub mod error;
pub mod holepunch;
pub mod overlay;
pub mod relay;
pub mod rendezvous;
pub mod stun;

pub use endpoint::Endpoint;
pub use error::OverlayError;
pub use overlay::{Overlay, OverlayConfig, OverlayEvent};
