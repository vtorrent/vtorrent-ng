/// Top-level Overlay — the public API for the NAT traversal layer.
///
/// Usage:
/// ```rust,no_run
/// use vtorrent_overlay::{Overlay, OverlayConfig};
///
/// #[tokio::main]
/// async fn main() {
///     let config = OverlayConfig::default();
///     let (overlay, mut events) = Overlay::start(config).await.unwrap();
///
///     // The overlay publishes our endpoint to the DHT and begins
///     // accepting hole-punch requests. Events are delivered via the channel.
///     while let Some(event) = events.recv().await {
///         match event {
///             vtorrent_overlay::OverlayEvent::PeerConnected(ep) => {
///                 println!("Connected to {}", ep);
///             }
///             _ => {}
///         }
///     }
/// }
/// ```
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::crypto::NodeKeypair;
use crate::endpoint::Endpoint;
use crate::error::{OverlayError, Result};
use crate::holepunch::HolePuncher;
use crate::relay::RelayEngine;
use crate::rendezvous::{EndpointRegistry, EndpointSource};
use crate::stun;

/// Configuration for the overlay.
#[derive(Debug, Clone)]
pub struct OverlayConfig {
    /// UDP port to listen on for overlay traffic. 0 = OS-assigned.
    pub listen_port: u16,
    /// Path to persist the node keypair (so the node ID is stable across restarts).
    pub key_file: Option<PathBuf>,
    /// Maximum number of relay sessions to accept (limits bandwidth used for others).
    pub max_relay_sessions: usize,
    /// How often to re-announce our endpoint to the DHT (seconds).
    pub announce_interval: u64,
    /// How often to evict stale endpoints from the registry (seconds).
    pub evict_interval: u64,
    /// Maximum age of an endpoint entry before it is evicted (seconds).
    pub max_endpoint_age: u64,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            listen_port: 0,
            key_file: None,
            max_relay_sessions: 20,
            announce_interval: 600, // 10 minutes
            evict_interval: 300,    // 5 minutes
            max_endpoint_age: 3600, // 1 hour
        }
    }
}

/// Events emitted by the overlay to the application layer.
#[derive(Debug, Clone)]
pub enum OverlayEvent {
    /// A new peer successfully hole-punched and is now reachable.
    PeerConnected(Endpoint),
    /// A peer's session was lost (timeout or explicit disconnect).
    PeerDisconnected(String),
    /// Our external address was discovered via STUN.
    ExternalAddrDiscovered(SocketAddr),
    /// An authenticated and decrypted application packet was received from a peer.
    DataReceived {
        from_node_id: String,
        payload: Vec<u8>,
    },
}

/// The overlay network manager.
#[derive(Clone)]
pub struct Overlay {
    pub keypair: NodeKeypair,
    pub external_addr: Option<SocketAddr>,
    pub local_addr: SocketAddr,
    pub registry: EndpointRegistry,
    puncher: Arc<HolePuncher>,
    event_tx: mpsc::Sender<OverlayEvent>,
}

impl Overlay {
    /// Start the overlay network.
    ///
    /// Returns the `Overlay` handle and a channel receiver for events.
    pub async fn start(config: OverlayConfig) -> Result<(Self, mpsc::Receiver<OverlayEvent>)> {
        // Load or generate keypair
        let keypair = load_or_generate_keypair(&config.key_file)?;
        tracing::info!("Overlay node ID: {}", keypair.node_id());

        // Bind the overlay UDP socket
        let bind_addr = format!("0.0.0.0:{}", config.listen_port);
        let socket = Arc::new(
            UdpSocket::bind(&bind_addr)
                .await
                .map_err(OverlayError::Io)?,
        );
        let local_addr = socket.local_addr().map_err(OverlayError::Io)?;
        tracing::info!("Overlay listening on UDP {}", local_addr);

        // Discover external address via STUN, using the overlay's own socket so
        // the reflected address is the NAT mapping for the socket that actually
        // carries overlay traffic.
        let external_addr = match stun::discover_external_addr(&socket).await {
            Ok(addr) => {
                tracing::info!("Overlay external address: {}", addr);
                Some(addr)
            }
            Err(e) => {
                tracing::warn!(
                    "STUN discovery failed: {} — operating without external address",
                    e
                );
                None
            }
        };

        let (event_tx, event_rx) = mpsc::channel(256);

        // Build our own endpoint and register it
        let registry = EndpointRegistry::new();
        if let Some(ext) = external_addr {
            let our_endpoint = Endpoint::new(keypair.node_id(), ext).with_lan(local_addr);
            registry.upsert(our_endpoint, EndpointSource::Manual).await;
        }

        let puncher = Arc::new(HolePuncher::new(socket.clone(), keypair.clone()));
        let relay = Arc::new(RelayEngine::new(socket.clone(), config.max_relay_sessions));

        // Spawn the receive loop
        {
            let socket = socket.clone();
            let puncher = puncher.clone();
            let relay = relay.clone();
            let registry = registry.clone();
            let event_tx = event_tx.clone();
            let keypair_pub = *keypair.public.as_bytes();
            tokio::spawn(async move {
                receive_loop(socket, puncher, relay, registry, event_tx, keypair_pub).await;
            });
        }

        // Spawn the maintenance loop
        {
            let registry = registry.clone();
            let evict_interval = config.evict_interval;
            let max_age = config.max_endpoint_age;
            let relay = relay.clone();
            tokio::spawn(async move {
                maintenance_loop(registry, relay, evict_interval, max_age).await;
            });
        }

        // Emit external addr event
        if let Some(addr) = external_addr {
            let _ = event_tx
                .send(OverlayEvent::ExternalAddrDiscovered(addr))
                .await;
        }

        let overlay = Self {
            keypair,
            external_addr,
            local_addr,
            registry,
            puncher,
            event_tx,
        };

        Ok((overlay, event_rx))
    }

    /// Attempt to connect to a remote node by its endpoint.
    pub async fn connect(&self, endpoint: &Endpoint) -> Result<()> {
        self.registry
            .upsert(endpoint.clone(), EndpointSource::Manual)
            .await;
        self.puncher.punch(endpoint).await?;
        let _ = self
            .event_tx
            .send(OverlayEvent::PeerConnected(endpoint.clone()))
            .await;
        Ok(())
    }

    /// Send data to a connected peer.
    pub async fn send(&self, node_id: &str, payload: &[u8]) -> Result<()> {
        self.puncher.send_data(node_id, payload).await
    }

    /// Returns the number of active peer sessions.
    pub async fn peer_count(&self) -> usize {
        self.puncher.session_count().await
    }

    /// Returns our overlay endpoint (for publishing to DHT/PEX).
    pub fn our_endpoint(&self) -> Option<Endpoint> {
        self.external_addr
            .map(|addr| Endpoint::new(self.keypair.node_id(), addr).with_lan(self.local_addr))
    }
}

/// The main UDP receive loop.
async fn receive_loop(
    socket: Arc<UdpSocket>,
    puncher: Arc<HolePuncher>,
    relay: Arc<RelayEngine>,
    registry: EndpointRegistry,
    event_tx: mpsc::Sender<OverlayEvent>,
    _our_pubkey: [u8; 32],
) {
    let mut buf = [0u8; 65536];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Overlay recv error: {}", e);
                continue;
            }
        };

        if n == 0 {
            continue;
        }

        let tag = buf[0];
        let data = &buf[..n];

        match tag {
            crate::holepunch::TAG_PUNCH => {
                if let Err(e) = puncher.handle_punch(from, data).await {
                    tracing::debug!("PUNCH handling error from {}: {}", from, e);
                } else {
                    // Emit PeerConnected event
                    if data.len() >= 33 {
                        let node_id = hex::encode(&data[1..33]);
                        let ep = Endpoint::new(node_id.clone(), from);
                        registry.upsert(ep.clone(), EndpointSource::HolePunch).await;
                        let _ = event_tx.send(OverlayEvent::PeerConnected(ep)).await;
                    }
                }
            }
            crate::relay::TAG_RELAY_REQUEST => {
                // Build a real peer map from the live EndpointRegistry so the relay
                // engine can look up target peers by node-id and forward onion-routed
                // messages to the correct socket address. Also resolve the
                // requester's node ID from their socket address so the forwarded
                // packet carries the real source ID (not a zero placeholder).
                let all_endpoints = registry.all().await;
                let peers: std::collections::HashMap<String, std::net::SocketAddr> = all_endpoints
                    .iter()
                    .map(|ep| (ep.node_id.clone(), ep.addr))
                    .collect();
                let requester_id = all_endpoints
                    .iter()
                    .find(|ep| ep.addr == from)
                    .map(|ep| ep.node_id.clone());
                if let Err(e) = relay
                    .handle_relay_request(from, requester_id.as_deref(), data, &peers)
                    .await
                {
                    tracing::debug!("RELAY_REQUEST error from {}: {}", from, e);
                }
            }
            crate::relay::TAG_RELAY_FORWARD => {
                // A relay forwarded a packet to us. The wire format is
                // [tag | source node ID (32 bytes) | encrypted payload]; decrypt
                // it with the source's session key, mirroring the TAG_DATA path.
                if data.len() > 33 {
                    let node_id = hex::encode(&data[1..33]);
                    match puncher.decrypt_data(&node_id, &data[33..]).await {
                        Ok(payload) => {
                            let _ = event_tx
                                .send(OverlayEvent::DataReceived {
                                    from_node_id: node_id,
                                    payload,
                                })
                                .await;
                        }
                        Err(e) => {
                            tracing::debug!(
                                peer = %node_id,
                                "Discarding unauthenticated relayed data: {}",
                                e
                            );
                        }
                    }
                }
            }
            crate::relay::TAG_RELAY_DECLINE => {
                // The relay could not reach the target we asked it to forward to.
                tracing::debug!("Relay declined our request (target unreachable)");
            }
            crate::holepunch::TAG_DATA => {
                // Wire format: [tag | sender node ID (32 bytes) | encrypted payload].
                // Only emit plaintext after the established session authenticates it.
                if data.len() > 33 {
                    let node_id = hex::encode(&data[1..33]);
                    match puncher.decrypt_data(&node_id, &data[33..]).await {
                        Ok(payload) => {
                            let _ = event_tx
                                .send(OverlayEvent::DataReceived {
                                    from_node_id: node_id,
                                    payload,
                                })
                                .await;
                        }
                        Err(e) => {
                            tracing::debug!(
                                peer = %node_id,
                                "Discarding unauthenticated overlay data: {}",
                                e
                            );
                        }
                    }
                }
            }
            _ => {
                tracing::trace!("Unknown overlay packet tag 0x{:02x} from {}", tag, from);
            }
        }
    }
}

/// Periodic maintenance: evict stale endpoints and clear old relay sessions.
async fn maintenance_loop(
    registry: EndpointRegistry,
    relay: Arc<RelayEngine>,
    evict_interval: u64,
    max_age: u64,
) {
    let mut ticker = interval(Duration::from_secs(evict_interval));
    loop {
        ticker.tick().await;
        registry.evict_stale(max_age).await;
        relay.clear_sessions().await;
        tracing::debug!(
            "Overlay maintenance: {} endpoints in registry",
            registry.len().await
        );
    }
}

/// Load the node keypair from disk, or generate and save a new one.
fn load_or_generate_keypair(key_file: &Option<PathBuf>) -> Result<NodeKeypair> {
    match key_file {
        None => Ok(NodeKeypair::generate()),
        Some(path) => {
            if path.exists() {
                let bytes =
                    std::fs::read(path).map_err(|e| OverlayError::KeyFile(e.to_string()))?;
                if bytes.len() != 32 {
                    return Err(OverlayError::KeyFile("invalid key file length".into()));
                }
                let arr: [u8; 32] = bytes.try_into().unwrap();
                tracing::info!("Loaded overlay keypair from {}", path.display());
                Ok(NodeKeypair::from_bytes(arr))
            } else {
                let kp = NodeKeypair::generate();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| OverlayError::KeyFile(e.to_string()))?;
                }
                std::fs::write(path, kp.secret_bytes())
                    .map_err(|e| OverlayError::KeyFile(e.to_string()))?;
                tracing::info!("Generated new overlay keypair, saved to {}", path.display());
                Ok(kp)
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overlay_config_defaults() {
        let cfg = OverlayConfig::default();
        assert_eq!(cfg.listen_port, 0);
        assert_eq!(cfg.max_relay_sessions, 20);
    }

    #[test]
    fn test_load_or_generate_keypair_no_file() {
        let kp = load_or_generate_keypair(&None).unwrap();
        assert_eq!(kp.node_id().len(), 64);
    }

    #[test]
    fn test_load_or_generate_keypair_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlay.key");
        let kp1 = load_or_generate_keypair(&Some(path.clone())).unwrap();
        let kp2 = load_or_generate_keypair(&Some(path)).unwrap();
        assert_eq!(kp1.node_id(), kp2.node_id());
    }
}
