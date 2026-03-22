/// Peer manager — manages all active peer connections.
///
/// Handles:
/// - Accepting inbound connections
/// - Initiating outbound connections to seed nodes
/// - Tracking connected peers and their state
/// - Broadcasting messages to all peers
/// - PEX address book for decentralized peer discovery
/// - DHT bootstrap integration

use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::{
    error::{P2pError, Result},
    message::{AddrMsg, NetMessage},
    peer::{run_peer, Peer, PeerCommand, PeerEvent, PeerState},
    pex::AddrBook,
};

/// Maximum number of simultaneous peer connections.
pub const MAX_PEERS: usize = 125;

/// Minimum number of outbound connections to maintain.
pub const TARGET_OUTBOUND: usize = 8;

/// vTorrent 2.0 hardcoded bootstrap peers (fallback if DHT is unavailable).
/// These are well-known long-running nodes; DNS is NOT required.
pub const BOOTSTRAP_PEERS: &[&str] = &[
    // Populated before mainnet launch — left empty for testnet
];

/// Legacy DNS seeds kept for backward compatibility (optional, not required).
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
    /// PEX address book for decentralized peer discovery.
    pub addr_book: AddrBook,
}

impl PeerManager {
    /// Create a new peer manager.
    pub fn new(best_height: u32, listen_addr: &str) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1024);
        let mut addr_book = AddrBook::new();

        // Set our own listen address so we don't connect to ourselves
        if let Ok(our_addr) = listen_addr.parse::<SocketAddr>() {
            addr_book.set_our_addr(our_addr);
        }

        Self {
            best_height,
            listen_addr: listen_addr.to_string(),
            peers: HashMap::new(),
            event_rx,
            event_tx,
            addr_book,
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

        // Record the attempt in the address book
        if let Ok(sock_addr) = addr.parse::<SocketAddr>() {
            self.addr_book.record_attempt(sock_addr);
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
    /// Returns events for the node to handle, and also updates the address book.
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
                    // Mark as connected in address book
                    self.addr_book.mark_connected(*peer_addr);
                }
                PeerEvent::Disconnected { peer_addr } => {
                    self.peers.remove(peer_addr);
                    self.addr_book.mark_disconnected(*peer_addr);
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

    /// Send a message to a specific peer.
    pub async fn send_to(&self, addr: SocketAddr, msg: NetMessage) {
        if let Some(peer) = self.peers.get(&addr) {
            if peer.state == PeerState::Connected {
                let _ = peer.cmd_tx.send(PeerCommand::Send(msg)).await;
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

    /// Get a list of all connected peer addresses.
    pub fn connected_peers(&self) -> Vec<SocketAddr> {
        self.peers.values()
            .filter(|p| p.state == PeerState::Connected)
            .map(|p| p.addr)
            .collect()
    }

    /// Connect to a SocketAddr directly (used by DHT/PEX bootstrap).
    pub async fn connect_addr(&mut self, addr: SocketAddr) -> Result<()> {
        self.connect(&addr.to_string()).await
    }

    /// Feed DHT-discovered peer addresses into the address book.
    pub fn add_dht_peers(&mut self, addrs: Vec<SocketAddr>) {
        self.addr_book.add_dht_peers(&addrs);
    }

    /// Process an incoming `addr` message from a peer.
    pub fn handle_addr_msg(&mut self, msg: &AddrMsg) {
        self.addr_book.add_from_addr_msg(msg);
    }

    /// Get candidate addresses to try connecting to (from PEX address book).
    pub fn get_peer_candidates(&self, count: usize) -> Vec<SocketAddr> {
        self.addr_book.get_candidates(count)
    }

    /// Build an `addr` message with our known peers for sharing.
    pub fn build_addr_response(&self) -> NetMessage {
        self.addr_book.build_addr_msg()
    }

    /// Build a `getaddr` message to request peers from a connected node.
    pub fn build_getaddr() -> NetMessage {
        AddrBook::build_getaddr()
    }

    /// Build a self-announcement `addr` message.
    pub fn build_self_announce(&self, services: u64) -> Option<NetMessage> {
        self.addr_book.build_self_announce(services)
    }

    /// Returns true if it's time to re-broadcast our own address.
    pub fn should_self_announce(&self) -> bool {
        self.addr_book.should_self_announce()
    }

    /// Returns true if it's time to request addresses from peers.
    pub fn should_getaddr(&self) -> bool {
        self.addr_book.should_getaddr()
    }

    /// Mark that we just sent a self-announcement.
    pub fn record_self_announce(&mut self) {
        self.addr_book.record_self_announce();
    }

    /// Mark that we just sent a `getaddr`.
    pub fn record_getaddr(&mut self) {
        self.addr_book.record_getaddr();
    }
}
