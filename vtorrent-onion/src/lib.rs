/// vtorrent-onion — Tor and I2P transport layer for vTorrent.
///
/// Provides anonymous P2P routing by tunneling vTorrent P2P connections
/// through the Tor network (via SOCKS5 proxy) or I2P (via SAM bridge).
///
/// ## Architecture
///
/// ```text
/// ┌──────────────────────────────────────────────────────┐
/// │                  vTorrent P2P Layer                   │
/// └────────────────────────┬─────────────────────────────┘
///                          │ TCP stream
/// ┌────────────────────────▼─────────────────────────────┐
/// │              OnionTransport (this crate)              │
/// │                                                       │
/// │  ┌─────────────┐   ┌─────────────┐   ┌───────────┐  │
/// │  │  Tor SOCKS5 │   │  I2P SAM    │   │  Clearnet │  │
/// │  │  127.0.0.1  │   │  127.0.0.1  │   │  (direct) │  │
/// │  │  :9050      │   │  :7656      │   │           │  │
/// │  └─────────────┘   └─────────────┘   └───────────┘  │
/// └──────────────────────────────────────────────────────┘
/// ```
///
/// ## Usage
///
/// ```rust,ignore
/// use vtorrent_onion::{OnionTransport, TransportConfig};
///
/// let config = TransportConfig::default();
/// let transport = OnionTransport::new(config);
///
/// // Connect to a .onion address
/// let stream = transport.connect("abcdef1234567890.onion:22526").await?;
/// ```

pub mod config;
pub mod error;
pub mod tor;
pub mod i2p;
pub mod transport;
pub mod hidden_service;
pub mod addr;

pub use config::TransportConfig;
pub use error::OnionError;
pub use transport::{OnionTransport, TransportMode};
pub use hidden_service::HiddenServiceInfo;
pub use addr::OnionAddr;
