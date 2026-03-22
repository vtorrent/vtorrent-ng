/// Top-level transport router.
///
/// Routes outbound TCP connections through the appropriate transport:
/// - `.onion` addresses → Tor SOCKS5
/// - `.i2p` addresses → I2P SAM bridge
/// - Clearnet addresses → direct TCP (or Tor if `prefer_onion` is set)
///
/// Falls back gracefully if the preferred transport is unavailable.

use tokio::net::TcpStream;
use crate::{
    addr::{is_anon_addr, OnionAddr},
    config::TransportConfig,
    error::{OnionError, Result},
    hidden_service::HiddenServiceInfo,
    i2p::I2pTransport,
    tor::TorTransport,
};

/// Which transport was used for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMode {
    /// Direct clearnet TCP connection.
    Clearnet,
    /// Routed through Tor SOCKS5.
    Tor,
    /// Routed through I2P SAM bridge.
    I2p,
}

/// The main transport abstraction — routes connections to the right backend.
pub struct OnionTransport {
    config: TransportConfig,
    tor: TorTransport,
    i2p: I2pTransport,
}

impl OnionTransport {
    pub fn new(config: TransportConfig) -> Self {
        let tor = TorTransport::new(config.clone());
        let i2p = I2pTransport::new(config.clone());
        Self { config, tor, i2p }
    }

    /// Connect to `addr` using the most appropriate transport.
    ///
    /// Returns the connected stream and which transport was used.
    pub async fn connect(&self, addr: &str) -> Result<(TcpStream, TransportMode)> {
        // Determine if this is a .onion or .i2p address
        if is_anon_addr(addr) {
            let onion_addr = OnionAddr::parse(addr)?;
            if onion_addr.is_tor() {
                if !self.config.tor_enabled {
                    return Err(OnionError::NotConfigured(
                        "Tor is disabled but a .onion address was requested".to_string()
                    ));
                }
                let stream = self.tor.connect(addr).await?;
                return Ok((stream, TransportMode::Tor));
            } else if onion_addr.is_i2p() {
                if !self.config.i2p_enabled {
                    return Err(OnionError::NotConfigured(
                        "I2P is disabled but a .i2p address was requested".to_string()
                    ));
                }
                let stream = self.i2p.connect(onion_addr.host(), onion_addr.port()).await?;
                return Ok((stream, TransportMode::I2p));
            }
        }

        // Clearnet address
        if self.config.prefer_onion && self.config.tor_enabled {
            // Try Tor first for clearnet addresses too
            if self.tor.is_available().await {
                match self.tor.connect(addr).await {
                    Ok(stream) => return Ok((stream, TransportMode::Tor)),
                    Err(e) => {
                        tracing::warn!("Tor connect to {} failed ({}), falling back to clearnet", addr, e);
                    }
                }
            }
        }

        // Direct clearnet connection
        let stream = TcpStream::connect(addr).await?;
        Ok((stream, TransportMode::Clearnet))
    }

    /// Check which transports are currently available.
    pub async fn available_transports(&self) -> Vec<TransportMode> {
        let mut modes = vec![TransportMode::Clearnet];

        if self.config.tor_enabled && self.tor.is_available().await {
            modes.push(TransportMode::Tor);
        }
        if self.config.i2p_enabled && self.i2p.is_available().await {
            modes.push(TransportMode::I2p);
        }

        modes
    }

    /// Create a Tor hidden service for inbound vTorrent connections.
    pub async fn create_tor_hidden_service(
        &self,
        local_port: u16,
        virtual_port: u16,
    ) -> Result<HiddenServiceInfo> {
        if !self.config.tor_enabled {
            return Err(OnionError::NotConfigured("Tor is disabled".to_string()));
        }
        self.tor.create_hidden_service(local_port, virtual_port).await
    }

    /// Create an I2P destination for inbound vTorrent connections.
    pub async fn create_i2p_destination(&self, local_port: u16) -> Result<HiddenServiceInfo> {
        if !self.config.i2p_enabled {
            return Err(OnionError::NotConfigured("I2P is disabled".to_string()));
        }
        self.i2p.create_destination(local_port).await
    }

    /// Get the Tor bootstrap progress (0–100%), or None if Tor is unavailable.
    pub async fn tor_bootstrap_progress(&self) -> Option<u8> {
        if !self.config.tor_enabled {
            return None;
        }
        self.tor.bootstrap_status().await
    }

    /// Request a new Tor circuit (changes exit node).
    pub async fn new_tor_circuit(&self) -> Result<()> {
        self.tor.new_circuit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_clearnet_always_available() {
        let config = TransportConfig::default();
        let transport = OnionTransport::new(config);
        let modes = transport.available_transports().await;
        assert!(modes.contains(&TransportMode::Clearnet));
    }

    #[tokio::test]
    async fn test_tor_not_available_in_ci() {
        let config = TransportConfig {
            tor_socks_addr: "127.0.0.1:19050".to_string(),
            ..Default::default()
        };
        let transport = OnionTransport::new(config);
        let modes = transport.available_transports().await;
        assert!(!modes.contains(&TransportMode::Tor));
    }

    #[tokio::test]
    async fn test_onion_addr_requires_tor() {
        let config = TransportConfig {
            tor_enabled: false,
            ..Default::default()
        };
        let transport = OnionTransport::new(config);
        let result = transport.connect(
            "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:22526"
        ).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), OnionError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn test_i2p_addr_requires_i2p() {
        let config = TransportConfig {
            i2p_enabled: false,
            ..Default::default()
        };
        let transport = OnionTransport::new(config);
        let result = transport.connect("zzz.i2p:22526").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), OnionError::NotConfigured(_)));
    }
}
