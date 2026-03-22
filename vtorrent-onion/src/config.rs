/// Configuration for the onion transport layer.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Whether to enable Tor transport.
    pub tor_enabled: bool,
    /// Tor SOCKS5 proxy address (default: 127.0.0.1:9050).
    pub tor_socks_addr: String,
    /// Tor control port address (default: 127.0.0.1:9051).
    pub tor_control_addr: String,
    /// Tor control port password (empty = no authentication).
    pub tor_control_password: String,

    /// Whether to enable I2P transport.
    pub i2p_enabled: bool,
    /// I2P SAM bridge address (default: 127.0.0.1:7656).
    pub i2p_sam_addr: String,

    /// Whether to prefer onion routing over clearnet for all connections.
    /// If false, onion routing is only used for .onion/.i2p addresses.
    pub prefer_onion: bool,

    /// Whether to create a Tor hidden service for inbound connections.
    pub create_hidden_service: bool,

    /// Directory to store hidden service keys (default: ~/.vtorrent/tor/).
    pub hidden_service_dir: String,

    /// Connection timeout in seconds.
    pub connect_timeout_secs: u64,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            tor_enabled: true,
            tor_socks_addr: "127.0.0.1:9050".to_string(),
            tor_control_addr: "127.0.0.1:9051".to_string(),
            tor_control_password: String::new(),
            i2p_enabled: false,
            i2p_sam_addr: "127.0.0.1:7656".to_string(),
            prefer_onion: false,
            create_hidden_service: false,
            hidden_service_dir: String::new(),
            connect_timeout_secs: 30,
        }
    }
}

impl TransportConfig {
    /// Returns true if any anonymous transport is configured and enabled.
    pub fn any_anon_enabled(&self) -> bool {
        self.tor_enabled || self.i2p_enabled
    }
}
