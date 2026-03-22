/// Peer manager — manages all active peer connections.
///
/// Handles:
/// - Accepting inbound connections
/// - Initiating outbound connections to seed nodes
/// - Tracking connected peers and their state
/// - Broadcasting messages to all peers

use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::{
    error::{P2pError, Result},
    message::{NetMessage, VersionMsg},
    peer::{run_peer, Peer, PeerCommand, PeerEvent, PeerState},
};

/// Maximum number of simultaneous peer connections.
pub const MAX_PEERS: usize = 125;

/// Minimum number of outbound connections to maintain.
pub const TARGET_OUTBOUND: usize = 8;

/// vTorrent 2.0 DNS seed nodes (to be populated before mainnet launch).
pub const DNS_SEEDS: &[&str] = &[
    "seed1.vtorrent.io",
    "seed2.vtorrent.io",
    "seed3.vtorrent.io",
];

/// Default mainnet P2P port.
pub const DEFAULT_PORT: u16 = 22526;

/// The peer manager.
pub struct PeerManager {
    /// Our best known block height.
    pub best_height: u32,
    /// Our listen address.
    pub listen_addr: String,
    /// Connected peers: addr → Peer.
    peers: HashMap<SocketAddr, Peer>,
    /// Channel for receiving peer events.
    event_rx: mpsc::Receiver<PeerEvent>,
    /// Sender for peer events (cloned for each peer task).
    event_tx: mpsc::Sender<PeerEvent>,
}

impl PeerManager {
    /// Create a new peer manager.
    pub fn new(best_height: u32, listen_addr: &str) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1024);
        Self {
            best_height,
            listen_addr: listen_addr.to_string(),
            peers: HashMap::new(),
            event_rx,
            event_tx,
        }
    }

    /// Start listening for inbound connections and connect to seed nodes.
    pub async fn start(&mut self) -> Result<()> {
        let listen_addr = self.listen_addr.clone();
        let listener = TcpListener::bind(&listen_addr).await?;
        tracing::info!("P2P listener started on {}", listen_addr);

        // Spawn the accept loop
        let event_tx = self.event_tx.clone();
        let best_height = self.best_height;
        let addr_str = listen_addr.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        tracing::info!("Inbound connection from {}", peer_addr);
                        let (cmd_tx, cmd_rx) = mpsc::channel(64);
                        let tx = event_tx.clone();
                        let addr = addr_str.clone();
                        tokio::spawn(async move {
                            run_peer(stream, peer_addr, best_height, &addr, tx, cmd_rx).await;
                        });
                        // Note: peer is registered when HandshakeComplete event arrives
                        let _ = cmd_tx; // keep alive until handshake
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    /// Connect to a specific peer address.
    pub async fn connect(&mut self, addr: &str) -> Result<()> {
        if self.peers.len() >= MAX_PEERS {
            return Err(P2pError::TooManyPeers(MAX_PEERS));
        }

        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let event_tx = self.event_tx.clone();
        let best_height = self.best_height;
        let our_addr = self.listen_addr.clone();

        tokio::spawn(async move {
            run_peer(stream, peer_addr, best_height, &our_addr, event_tx, cmd_rx).await;
        });

        // Register as connecting peer
        self.peers.insert(peer_addr, Peer {
            addr: peer_addr,
            state: PeerState::Connecting,
            best_height: 0,
            user_agent: String::new(),
            services: 0,
            cmd_tx,
        });

        tracing::info!("Connecting to {}", peer_addr);
        Ok(())
    }

    /// Process pending peer events (call this in the main loop).
    pub async fn process_events(&mut self) -> Vec<PeerEvent> {
        let mut events = Vec::new();

        while let Ok(event) = self.event_rx.try_recv() {
            match &event {
                PeerEvent::HandshakeComplete { peer_addr, version } => {
                    if let Some(peer) = self.peers.get_mut(peer_addr) {
                        peer.state = PeerState::Connected;
                        peer.best_height = version.start_height;
                        peer.user_agent = version.user_agent.clone();
                        peer.services = version.services;
                        tracing::info!(
                            "Peer {} connected: {} (height {})",
                            peer_addr, version.user_agent, version.start_height
                        );
                    }
                }
                PeerEvent::Disconnected { peer_addr } => {
                    self.peers.remove(peer_addr);
                    tracing::info!("Peer {} removed", peer_addr);
                }
                _ => {}
            }
            events.push(event);
        }

        events
    }

    /// Broadcast a message to all connected peers.
    pub async fn broadcast(&self, msg: NetMessage) {
        for peer in self.peers.values() {
            if peer.state == PeerState::Connected {
                let _ = peer.cmd_tx.send(PeerCommand::Send(msg.clone())).await;
            }
        }
    }

    /// Get the number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.values().filter(|p| p.state == PeerState::Connected).count()
    }

    /// Get the best known block height across all peers.
    pub fn network_best_height(&self) -> u32 {
        self.peers.values()
            .filter(|p| p.state == PeerState::Connected)
            .map(|p| p.best_height)
            .max()
            .unwrap_or(0)
    }
}
