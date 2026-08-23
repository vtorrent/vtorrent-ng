/// Peer manager — manages all active peer connections.
///
/// Handles:
/// - Accepting inbound connections
/// - Initiating outbound connections to seed nodes
/// - Tracking connected peers and their state
/// - Broadcasting messages to all peers
/// - PEX address book for decentralized peer discovery
/// - DHT bootstrap integration
/// - Peer ban management (misbehaviour scoring and IP bans)
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use vtorrent_onion::{OnionTransport, TransportConfig, TransportMode};

use crate::{
    ban_manager::{BanManager, Misbehaviour},
    error::{P2pError, Result},
    message::{AddrMsg, NetMessage},
    peer::{run_peer, Peer, PeerCommand, PeerEvent, PeerState},
    pex::AddrBook,
};

/// Maximum number of simultaneous peer connections.
pub const MAX_PEERS: usize = 125;

/// Maximum number of simultaneous inbound connections.
///
/// Inbound connections are not counted against the outbound connection
/// budget until the handshake completes, so a flood of unauthenticated
/// connections must be capped independently to prevent resource exhaustion.
pub const MAX_INBOUND: usize = 64;

/// Minimum number of outbound connections to maintain.
pub const TARGET_OUTBOUND: usize = 8;

/// vTorrent 2.0 hardcoded bootstrap peers (fallback if DHT is unavailable).
/// These are well-known long-running nodes; DNS is NOT required.
pub const BOOTSTRAP_PEERS: &[&str] = &[
    // Populated before mainnet launch — left empty for testnet
];

/// Legacy DNS seeds kept for backward compatibility (optional, not required).
///
/// The original `seed1/2/3.vtorrent.io` domains are no longer valid. New seed
/// nodes are added via `bootstrap/peers.txt` (GitHub-hosted) or `BOOTSTRAP_PEERS`
/// once deployed. This list is intentionally empty until new seeds are live.
pub const DNS_SEEDS: &[&str] = &[];

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
    /// Ban manager — tracks misbehaviour scores and IP bans.
    pub ban_manager: std::sync::Arc<tokio::sync::RwLock<BanManager>>,
    /// Outbound transport router for clearnet, Tor SOCKS5, and I2P SAM dialing.
    transport: OnionTransport,
    /// Number of active inbound connections (pre-handshake and post-handshake).
    inbound_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Receiver for inbound peer registrations from the accept loop.
    inbound_rx: mpsc::Receiver<(SocketAddr, mpsc::Sender<PeerCommand>)>,
    /// Sender half — cloned into the accept loop.
    inbound_tx: mpsc::Sender<(SocketAddr, mpsc::Sender<PeerCommand>)>,
}

impl PeerManager {
    /// Create a new mainnet peer manager (private addresses rejected in PEX).
    pub fn new(best_height: u32, listen_addr: &str) -> Self {
        Self::with_testnet(best_height, listen_addr, false)
    }

    /// Create a new testnet peer manager (private/RFC1918 addresses accepted in PEX).
    ///
    /// Use this when running multiple nodes on the same LAN or localhost for testing.
    pub fn new_testnet(best_height: u32, listen_addr: &str) -> Self {
        Self::with_testnet(best_height, listen_addr, true)
    }

    /// Create a peer manager with an explicit testnet flag.
    pub fn with_testnet(best_height: u32, listen_addr: &str, testnet: bool) -> Self {
        Self::with_transport_config(
            best_height,
            listen_addr,
            testnet,
            TransportConfig::default(),
        )
    }

    /// Create a peer manager with an explicit anonymous-transport configuration.
    pub fn with_transport_config(
        best_height: u32,
        listen_addr: &str,
        testnet: bool,
        transport_config: TransportConfig,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1024);
        let mut addr_book = AddrBook::with_testnet(testnet);

        // Set our own listen address so we don't connect to ourselves
        if let Ok(our_addr) = listen_addr.parse::<SocketAddr>() {
            addr_book.set_our_addr(our_addr);
        }

        let (inbound_tx, inbound_rx) = mpsc::channel(1024);

        Self {
            best_height,
            listen_addr: listen_addr.to_string(),
            peers: HashMap::new(),
            event_rx,
            event_tx,
            addr_book,
            ban_manager: std::sync::Arc::new(tokio::sync::RwLock::new(BanManager::new(
                100,
                Duration::from_secs(24 * 60 * 60),
            ))),
            transport: OnionTransport::new(transport_config),
            inbound_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            inbound_rx,
            inbound_tx,
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
        let inbound_count = self.inbound_count.clone();
        let inbound_tx = self.inbound_tx.clone();
        let accept_bans = self.ban_manager.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        // Ban check at the TCP boundary: banned IPs are
                        // rejected before any handshake or message processing,
                        // otherwise each reconnect buys ~10s of pre-handshake
                        // processing and burns an inbound slot.
                        if accept_bans.read().await.is_banned(peer_addr.ip()) {
                            tracing::warn!(
                                "Rejecting inbound connection from banned IP {}",
                                peer_addr
                            );
                            continue;
                        }
                        // Cap inbound connections so a flood of unauthenticated
                        // sockets cannot exhaust resources before the handshake
                        // completes (or times out).
                        let prev = inbound_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if prev >= MAX_INBOUND {
                            inbound_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            tracing::warn!(
                                "Rejecting inbound connection from {} (inbound cap reached)",
                                peer_addr
                            );
                            continue;
                        }
                        tracing::info!("Inbound connection from {}", peer_addr);
                        let (cmd_tx, cmd_rx) = mpsc::channel(64);
                        let tx = event_tx.clone();
                        let addr = addr_str.clone();
                        let count = inbound_count.clone();
                        // Register the inbound peer so broadcasts reach it.
                        let _ = inbound_tx.send((peer_addr, cmd_tx.clone())).await;
                        tokio::spawn(async move {
                            run_peer(stream, peer_addr, best_height, &addr, tx, cmd_rx).await;
                            count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            tracing::debug!("Inbound peer task finished: {}", peer_addr);
                        });
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
    ///
    /// Returns `Err(P2pError::Banned)` if the peer's IP is currently banned.
    pub async fn connect(&mut self, addr: &str) -> Result<()> {
        if self.peers.len() >= MAX_PEERS {
            return Err(P2pError::TooManyPeers(MAX_PEERS));
        }

        // Ban check: reject connections to banned IPs before even opening a socket
        if let Ok(sock_addr) = addr.parse::<SocketAddr>() {
            // Deduplicate: a second connection to the same address would
            // overwrite the first peer's map entry, orphaning its task and
            // leaving the newer connection unmanaged when the old one dies.
            if self.peers.contains_key(&sock_addr) {
                tracing::debug!("Already connected to {}, skipping duplicate connect", addr);
                return Err(P2pError::Transport(format!(
                    "already connected to {}",
                    addr
                )));
            }
            let ip = sock_addr.ip();
            if self.ban_manager.read().await.is_banned(ip) {
                tracing::debug!("Skipping banned peer {}", addr);
                return Err(P2pError::Banned(ip.to_string()));
            }
            // Record the attempt in the address book
            self.addr_book.record_attempt(sock_addr);
        }

        let (stream, transport_mode) = self
            .transport
            .connect(addr)
            .await
            .map_err(|e| P2pError::Transport(e.to_string()))?;
        // SOCKS5 and I2P streams report their local proxy endpoint as `peer_addr`.
        // Retain a deterministic synthetic socket key for anonymous destinations so
        // the existing peer lifecycle map remains usable without leaking a proxy IP.
        let peer_addr = match transport_mode {
            TransportMode::Clearnet => stream.peer_addr()?,
            TransportMode::Tor | TransportMode::I2p => anonymous_peer_key(addr),
        };

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let event_tx = self.event_tx.clone();
        let best_height = self.best_height;
        let our_addr = self.listen_addr.clone();

        tokio::spawn(async move {
            run_peer(stream, peer_addr, best_height, &our_addr, event_tx, cmd_rx).await;
            tracing::debug!("Outbound peer task finished: {}", peer_addr);
        });

        // Register as connecting peer
        self.peers.insert(
            peer_addr,
            Peer {
                addr: peer_addr,
                state: PeerState::Connecting,
                best_height: 0,
                user_agent: String::new(),
                services: 0,
                cmd_tx,
            },
        );

        tracing::info!(target = %addr, peer = %peer_addr, transport = ?transport_mode, "Connecting to peer");
        Ok(())
    }

    /// Process pending peer events (call this in the main loop).
    /// Returns events for the node to handle, and also updates the address book.
    pub async fn process_events(&mut self) -> Vec<PeerEvent> {
        let mut events = Vec::new();

        // Register inbound peers accepted by the accept loop.
        while let Ok((peer_addr, cmd_tx)) = self.inbound_rx.try_recv() {
            self.peers.entry(peer_addr).or_insert(Peer {
                addr: peer_addr,
                state: PeerState::Connecting,
                best_height: 0,
                user_agent: String::new(),
                services: 0,
                cmd_tx,
            });
        }

        while let Ok(event) = self.event_rx.try_recv() {
            match &event {
                PeerEvent::HandshakeComplete { peer_addr, version } => {
                    // Ban check on inbound connections (outbound are checked in connect())
                    let ip = peer_addr.ip();
                    if self.ban_manager.read().await.is_banned(ip) {
                        tracing::info!(
                            "Dropping inbound connection from banned peer {}",
                            peer_addr
                        );
                        // Disconnect the peer immediately
                        if let Some(peer) = self.peers.get(peer_addr) {
                            let _ = peer.cmd_tx.try_send(PeerCommand::Disconnect);
                        }
                        // Don't forward the event to the node
                        continue;
                    }

                    if let Some(peer) = self.peers.get_mut(peer_addr) {
                        peer.state = PeerState::Connected;
                        peer.best_height = version.start_height;
                        peer.user_agent = version.user_agent.clone();
                        peer.services = version.services;
                        tracing::info!(
                            "Peer {} connected: {} (height {})",
                            peer_addr,
                            version.user_agent,
                            version.start_height
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

    /// Record misbehaviour for a peer and potentially ban them.
    ///
    /// Returns `true` if the peer was banned as a result.
    /// If banned, the peer is also immediately disconnected.
    pub async fn record_misbehaviour(&mut self, addr: SocketAddr, offence: Misbehaviour) -> bool {
        let ip = addr.ip();
        let banned = self
            .ban_manager
            .write()
            .await
            .record_misbehaviour(ip, offence);
        if banned {
            tracing::warn!("Peer {} banned for misbehaviour ({:?})", addr, offence);
            // Disconnect the peer if still connected
            if let Some(peer) = self.peers.get(&addr) {
                let _ = peer.cmd_tx.try_send(PeerCommand::Disconnect);
            }
        }
        banned
    }

    /// Manually ban a peer IP with a reason.
    pub async fn ban_peer(&mut self, addr: SocketAddr, reason: String) {
        let ip = addr.ip();
        self.ban_manager.write().await.ban_ip(ip, reason.clone());
        tracing::warn!("Manually banned peer {}: {}", addr, reason);
        // Disconnect if currently connected
        if let Some(peer) = self.peers.get(&addr) {
            let _ = peer.cmd_tx.try_send(PeerCommand::Disconnect);
        }
    }

    /// Returns `true` if the given address is currently banned.
    pub async fn is_banned(&self, addr: SocketAddr) -> bool {
        self.ban_manager.read().await.is_banned(addr.ip())
    }

    /// Prune expired bans and decay old misbehaviour scores.
    /// Should be called periodically (e.g., every hour).
    pub async fn prune_bans(&mut self) {
        self.ban_manager.write().await.prune();
    }

    /// Register an already-established non-TCP peer, such as an authenticated
    /// overlay session. Its command channel is owned by the transport bridge.
    pub fn register_virtual_peer(
        &mut self,
        addr: SocketAddr,
        user_agent: String,
        cmd_tx: mpsc::Sender<PeerCommand>,
    ) -> Result<()> {
        if self.peers.len() >= MAX_PEERS && !self.peers.contains_key(&addr) {
            return Err(P2pError::TooManyPeers(MAX_PEERS));
        }
        self.peers.insert(
            addr,
            Peer {
                addr,
                state: PeerState::Connected,
                best_height: 0,
                user_agent,
                services: 0,
                cmd_tx,
            },
        );
        Ok(())
    }

    /// Remove a non-TCP peer whose transport has closed.
    pub fn remove_virtual_peer(&mut self, addr: SocketAddr) {
        self.peers.remove(&addr);
        self.addr_book.mark_disconnected(addr);
    }

    /// Broadcast a message to all connected peers.
    ///
    /// Uses `try_send` so one stalled peer (full command queue because its
    /// TCP socket stopped draining) cannot head-of-line block global message
    /// propagation. Messages to a stalled peer are dropped; the peer's idle
    /// timeout will eventually evict it.
    pub async fn broadcast(&self, msg: NetMessage) {
        for peer in self.peers.values() {
            if peer.state == PeerState::Connected {
                if let Err(e) = peer.cmd_tx.try_send(PeerCommand::Send(msg.clone())) {
                    tracing::debug!("Broadcast queue full for {}, dropping: {}", peer.addr, e);
                }
            }
        }
    }

    /// Broadcast a message to all connected peers except the given address.
    ///
    /// Used when relaying a message received from a peer so it is not echoed
    /// back to the sender, which would otherwise amplify traffic.
    pub async fn broadcast_except(&self, except: SocketAddr, msg: NetMessage) {
        for peer in self.peers.values() {
            if peer.state == PeerState::Connected && peer.addr != except {
                if let Err(e) = peer.cmd_tx.try_send(PeerCommand::Send(msg.clone())) {
                    tracing::debug!("Broadcast queue full for {}, dropping: {}", peer.addr, e);
                }
            }
        }
    }

    /// Send a message to a specific peer.
    ///
    /// Bounded wait: if the peer's queue is full for longer than a second the
    /// message is dropped rather than blocking the caller indefinitely.
    pub async fn send_to(&self, addr: SocketAddr, msg: NetMessage) {
        if let Some(peer) = self.peers.get(&addr) {
            if peer.state == PeerState::Connected {
                let send = peer.cmd_tx.send(PeerCommand::Send(msg));
                if tokio::time::timeout(std::time::Duration::from_secs(1), send)
                    .await
                    .is_err()
                {
                    tracing::debug!("Send queue full for {}, dropping message", addr);
                }
            }
        }
    }

    /// Get the number of connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers
            .values()
            .filter(|p| p.state == PeerState::Connected)
            .count()
    }

    /// Get the best known block height across all peers.
    pub fn network_best_height(&self) -> u32 {
        self.peers
            .values()
            .filter(|p| p.state == PeerState::Connected)
            .map(|p| p.best_height)
            .max()
            .unwrap_or(0)
    }

    /// Get a list of all connected peer addresses.
    pub fn connected_peers(&self) -> Vec<SocketAddr> {
        self.peers
            .values()
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

    /// Disconnect a specific peer by sending it a `Disconnect` command.
    pub async fn disconnect(&mut self, addr: SocketAddr) {
        if let Some(peer) = self.peers.get_mut(&addr) {
            let _ = peer.cmd_tx.send(PeerCommand::Disconnect).await;
            peer.state = PeerState::Disconnecting;
        }
    }
}

/// Produce a deterministic, non-routable key for an anonymous endpoint.
///
/// `PeerManager` is keyed by `SocketAddr` because TCP peers expose one naturally.
/// Tor and I2P do not, so use the benchmarking range 198.18.0.0/15 strictly as an
/// internal key; this address is never added to PEX address entries.
fn anonymous_peer_key(addr: &str) -> SocketAddr {
    use std::net::{IpAddr, Ipv4Addr};

    let mut hash: u32 = 0x811c_9dc5;
    for byte in addr.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }

    let second = 18 + ((hash >> 8) & 1) as u8;
    let third = (hash >> 16) as u8;
    let fourth = (hash >> 24) as u8;
    let port = 1_024 + (hash as u16 % (u16::MAX - 1_024));
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, second, third, fourth)), port)
}
