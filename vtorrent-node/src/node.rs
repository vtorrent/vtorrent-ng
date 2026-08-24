/// vTorrent Node — main event loop.
///
/// Wires together:
/// - Chain state (vtorrent-node)
/// - P2P peer manager (vtorrent-p2p)
/// - PoS staking engine (vtorrent-node::staking)
/// - Mempool (vtorrent-node)
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{interval, Duration};

use std::path::PathBuf;

use vtorrent_onion::TransportConfig;
use vtorrent_overlay::{Overlay, OverlayConfig, OverlayEvent};

use vtorrent_p2p::{
    compact::{
        derive_siphash_keys, short_txid, CompactBlockDecodeError, CompactBlockDecoder,
        CompactBlockPeerState,
    },
    dht::{discover_peers_via_doh, discover_peers_via_github, DhtBootstrap},
    message::{
        AddrMsg, BlockTxnMsg, CmpctBlockMsg, FeeFilterMsg, GetBlockTxnMsg, GetBlocksMsg,
        GetHeadersMsg, HeaderEntry, HeadersMsg, InvItem, InvMsg, InvType, NetMessage, PingMsg,
        SendCmpctMsg, VersionMsg, MAX_PAYLOAD_SIZE, NODE_NETWORK, NODE_TORRENT,
    },
    peer::{PeerCommand, PeerEvent},
    peer_manager::{PeerManager, DEFAULT_PORT, TARGET_OUTBOUND},
};

use crate::{
    atomic_swap::{OrderAnnouncement, SwapOrderBook},
    block::{Block, BlockHeader, Transaction},
    chain::Chain,
    consensus::{compute_stake_modifier, TARGET_BLOCK_TIME},
    error::{NodeError, Result},
    events::{EventSender, NodeEvent},
    mempool::Mempool,
    staking::{StakingCommand, StakingEngine},
};

/// Authenticated overlay notifications queued for the node event loop.
enum OverlayIngress {
    PeerConnected { node_id: String },
    PeerDisconnected { node_id: String },
    Message { node_id: String, msg: NetMessage },
}

/// Maximum number of messages a single peer may send within one rate-limit
/// window before it is banned. Protects the node's event loop from flood DoS.
pub const MAX_MSGS_PER_WINDOW: u64 = 500;

/// Rate-limit window length in seconds.
pub const MSG_WINDOW_SECS: u64 = 10;

/// How often (seconds) to sync blocks from peers.
const SYNC_INTERVAL_SECS: u64 = 30;

/// How often (seconds) to run peer maintenance (prune, eviction).
const PEER_MAINTENANCE_SECS: u64 = 60;

/// How often (seconds) to send keepalive pings.
const PING_INTERVAL_SECS: u64 = 120;

/// How often (seconds) to request peer addresses via PEX.
const PEX_INTERVAL_SECS: u64 = 600;

/// How often (seconds) to re-announce to the DHT.
const DHT_REANNOUNCE_SECS: u64 = 1800;

/// Number of headers to request per batch during header sync.
const HEADERS_PER_BATCH: usize = 2000;

/// Current Unix timestamp in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Node configuration.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// P2P listen address.
    pub listen_addr: String,
    /// Whether to enable staking.
    pub staking_enabled: bool,
    /// The staking address (must have UTXOs).
    pub staking_address: Option<String>,
    /// The WIF-encoded private key used to sign coinstake inputs.
    pub staking_wif: Option<String>,
    /// Maximum mempool size.
    pub max_mempool: usize,
    /// Additional seed nodes to connect to.
    pub extra_seeds: Vec<String>,
    /// Whether to use DHT bootstrap for peer discovery.
    pub use_dht: bool,
    /// When `true`, skip all internet bootstrap (DHT, DoH, DNS seeds, GitHub
    /// peer list) and only talk to explicitly configured `--seed` peers.
    /// Used for isolated local testnets so they never reach production seeds.
    pub isolated: bool,
    /// Node data directory (for peer cache, chain data, etc.).
    /// Defaults to `~/.vtorrent` on all platforms.
    pub data_dir: PathBuf,
    /// Whether to start the overlay NAT traversal layer.
    pub use_overlay: bool,
    /// When `true`, the node operates on testnet:
    /// - PEX accepts private/RFC1918 addresses
    /// - Looser validation rules may apply in future
    pub testnet: bool,
    /// When `true`, the node operates in regtest mode:
    /// - A faucet RPC endpoint mints coins to arbitrary addresses
    pub regtest: bool,
    /// Outbound routing policy for clearnet, Tor SOCKS5, and I2P SAM peers.
    pub transport: TransportConfig,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen_addr: format!("0.0.0.0:{}", DEFAULT_PORT),
            staking_enabled: false,
            staking_address: None,
            staking_wif: None,
            max_mempool: 10_000,
            extra_seeds: Vec::new(),
            use_dht: true,
            isolated: false,
            data_dir: default_data_dir(),
            use_overlay: true,
            testnet: false,
            regtest: false,
            transport: TransportConfig::default(),
        }
    }
}

/// Returns the default node data directory (`~/.vtorrent`).
fn default_data_dir() -> PathBuf {
    dirs_home().join(".vtorrent")
}

/// Cross-platform home directory lookup.
fn dirs_home() -> PathBuf {
    // Try $HOME first, then fall back to current directory
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// The vTorrent node.
pub struct Node {
    chain: Arc<Mutex<Chain>>,
    mempool: Arc<Mutex<Mempool>>,
    peer_manager: PeerManager,
    staking: Option<StakingEngine>,
    config: NodeConfig,
    overlay: Option<Overlay>,
    /// Authenticated overlay events awaiting centralized P2P dispatch.
    overlay_rx: mpsc::Receiver<OverlayIngress>,
    overlay_tx: mpsc::Sender<OverlayIngress>,
    /// Overlay peer identity by its internal virtual socket key.
    overlay_peers: HashMap<std::net::SocketAddr, String>,
    /// Virtual peers that have completed the P2P version/verack exchange.
    overlay_handshaken: HashSet<std::net::SocketAddr>,
    /// Optional event sender — when set, the node emits live events to subscribers.
    event_tx: Option<EventSender>,
    /// Per-peer compact block relay state (BIP-152).
    compact_peers: std::collections::HashMap<std::net::SocketAddr, CompactBlockPeerState>,
    /// Partial compact blocks awaiting `blocktxn` responses (BIP-152).
    /// Keyed by block hash; populated when `cmpctblock` reports missing txs.
    pending_compact_blocks: HashMap<[u8; 32], CmpctBlockMsg>,
    /// Per-peer minimum fee rate (from feefilter messages), satoshis per 1000 bytes.
    peer_fee_filters: std::collections::HashMap<std::net::SocketAddr, u64>,
    /// Per-peer last-seen ping nonce (for pong matching).
    peer_ping_nonces: std::collections::HashMap<std::net::SocketAddr, u64>,
    /// Per-peer message counts for flood rate limiting: (count, window start).
    peer_msg_counts: std::collections::HashMap<std::net::SocketAddr, (u64, u64)>,
    /// Shared DEX order book (set by the daemon; used for gossip).
    order_book: Option<Arc<RwLock<SwapOrderBook>>>,
    /// Order IDs already seen via gossip, for deduplication.
    seen_orders: HashSet<[u8; 32]>,
    /// Receiver for locally-submitted transactions (from RPC/wallet).
    /// When a transaction is placed here, the node broadcasts it to all peers.
    tx_submit_rx: mpsc::Receiver<Transaction>,
    /// Sender half — cloned and given to AppState so RPC can inject transactions.
    tx_submit_tx: mpsc::Sender<Transaction>,
    /// Receiver for locally-minted blocks (from the regtest faucet).
    /// When a block is placed here, the node announces it to all peers.
    block_submit_rx: mpsc::Receiver<Block>,
    /// Sender half — cloned and given to AppState so the faucet can inject blocks.
    block_submit_tx: mpsc::Sender<Block>,
    /// Receiver for runtime staking control commands (from RPC/tauri).
    staking_rx: mpsc::Receiver<StakingCommand>,
    /// Sender half — cloned and given to AppState to start/stop staking at runtime.
    staking_control_tx: mpsc::Sender<StakingCommand>,
}

/// Encode a P2P message for transport inside the encrypted overlay payload.
///
/// Envelope: fixed 12-byte command, little-endian payload length, then payload.
fn encode_overlay_message(msg: &NetMessage) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + msg.payload.len());
    bytes.extend_from_slice(&msg.command);
    bytes.extend_from_slice(&(msg.payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&msg.payload);
    bytes
}

/// Decode and validate an encrypted overlay payload before P2P dispatch.
fn decode_overlay_message(bytes: &[u8]) -> Result<NetMessage> {
    if bytes.len() < 16 {
        return Err(NodeError::Chain(
            "Overlay message is shorter than its envelope".into(),
        ));
    }

    let mut command = [0u8; 12];
    command.copy_from_slice(&bytes[..12]);
    let command_len = command.iter().position(|byte| *byte == 0).unwrap_or(12);
    std::str::from_utf8(&command[..command_len])
        .map_err(|_| NodeError::Chain("Overlay message command is not UTF-8".into()))?;
    if command_len == 0 || command[command_len..].iter().any(|byte| *byte != 0) {
        return Err(NodeError::Chain(
            "Overlay message command is malformed".into(),
        ));
    }

    let payload_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD_SIZE as usize || bytes.len() != 16 + payload_len {
        return Err(NodeError::Chain(
            "Overlay message payload length is invalid".into(),
        ));
    }

    Ok(NetMessage {
        command,
        payload: bytes[16..].to_vec(),
    })
}

/// Derive a stable private virtual address from an overlay node ID.
fn overlay_peer_addr(node_id: &str) -> Result<std::net::SocketAddr> {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    let bytes = hex::decode(node_id)
        .map_err(|_| NodeError::Chain("Overlay node ID is not hexadecimal".into()))?;
    if bytes.len() != 32 {
        return Err(NodeError::Chain("Overlay node ID must be 32 bytes".into()));
    }
    let port = 1_024 + (u16::from_le_bytes([bytes[2], bytes[3]]) % (u16::MAX - 1_024));
    Ok(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::new(198, 18 + (bytes[0] & 1), bytes[1], bytes[4])),
        port,
    ))
}

impl Node {
    /// Create a new node.
    pub fn new(config: NodeConfig) -> Result<Self> {
        let chain = Chain::new()?;
        let best_height = chain.best_height();
        let mempool = Mempool::new(config.max_mempool);
        let peer_manager = PeerManager::with_transport_config(
            best_height,
            &config.listen_addr,
            config.testnet,
            config.transport.clone(),
        );
        let (tx_submit_tx, tx_submit_rx) = mpsc::channel(256);
        let (overlay_tx, overlay_rx) = mpsc::channel(256);

        let staking = if config.staking_enabled {
            config
                .staking_address
                .as_ref()
                .map(|addr| match &config.staking_wif {
                    Some(wif) => StakingEngine::with_wif(addr.clone(), wif.clone()),
                    None => StakingEngine::new(addr.clone()),
                })
        } else {
            None
        };

        let (block_submit_tx, block_submit_rx) = mpsc::channel(64);
        let (staking_control_tx, staking_rx) = mpsc::channel(16);

        Ok(Self {
            chain: Arc::new(Mutex::new(chain)),
            mempool: Arc::new(Mutex::new(mempool)),
            peer_manager,
            staking,
            config,
            overlay: None,
            overlay_rx,
            overlay_tx,
            overlay_peers: HashMap::new(),
            overlay_handshaken: HashSet::new(),
            event_tx: None,
            compact_peers: std::collections::HashMap::new(),
            pending_compact_blocks: HashMap::new(),
            peer_fee_filters: std::collections::HashMap::new(),
            peer_ping_nonces: std::collections::HashMap::new(),
            peer_msg_counts: std::collections::HashMap::new(),
            order_book: None,
            seen_orders: HashSet::new(),
            tx_submit_rx,
            tx_submit_tx,
            block_submit_rx,
            block_submit_tx,
            staking_rx,
            staking_control_tx,
        })
    }

    /// Create a new node with a pre-loaded chain (e.g. loaded from BlockStore on disk).
    ///
    /// This is used by `vtorrent-daemon` when resuming from a persisted chain state
    /// rather than starting from genesis.
    pub fn new_with_chain(config: NodeConfig, chain: Chain) -> Result<Self> {
        let best_height = chain.best_height();
        let mempool = Mempool::new(config.max_mempool);
        let peer_manager = PeerManager::with_transport_config(
            best_height,
            &config.listen_addr,
            config.testnet,
            config.transport.clone(),
        );
        let (tx_submit_tx, tx_submit_rx) = mpsc::channel(256);
        let (overlay_tx, overlay_rx) = mpsc::channel(256);

        let staking = if config.staking_enabled {
            config
                .staking_address
                .as_ref()
                .map(|addr| match &config.staking_wif {
                    Some(wif) => StakingEngine::with_wif(addr.clone(), wif.clone()),
                    None => StakingEngine::new(addr.clone()),
                })
        } else {
            None
        };

        let (block_submit_tx, block_submit_rx) = mpsc::channel(64);
        let (staking_control_tx, staking_rx) = mpsc::channel(16);

        Ok(Self {
            chain: Arc::new(Mutex::new(chain)),
            mempool: Arc::new(Mutex::new(mempool)),
            peer_manager,
            staking,
            config,
            overlay: None,
            overlay_rx,
            overlay_tx,
            overlay_peers: HashMap::new(),
            overlay_handshaken: HashSet::new(),
            event_tx: None,
            compact_peers: std::collections::HashMap::new(),
            pending_compact_blocks: HashMap::new(),
            peer_fee_filters: std::collections::HashMap::new(),
            peer_ping_nonces: std::collections::HashMap::new(),
            peer_msg_counts: std::collections::HashMap::new(),
            order_book: None,
            seen_orders: HashSet::new(),
            tx_submit_rx,
            tx_submit_tx,
            block_submit_rx,
            block_submit_tx,
            staking_rx,
            staking_control_tx,
        })
    }

    /// Returns a clone of the Arc wrapping the chain Mutex.
    /// Used by vtorrent-daemon to share the live chain with the RPC server.
    pub fn chain_arc(&self) -> Arc<Mutex<Chain>> {
        Arc::clone(&self.chain)
    }

    /// Returns a clone of the Arc wrapping the mempool Mutex.
    /// Used by vtorrent-daemon to share the live mempool with the RPC server.
    pub fn mempool_arc(&self) -> Arc<Mutex<Mempool>> {
        Arc::clone(&self.mempool)
    }

    /// Attach an event sender so the node can emit live events to subscribers.
    /// Call this before `start()` — typically done by `vtorrent-daemon` to bridge
    /// the node event channel to the RPC WebSocket broadcaster.
    pub fn set_event_sender(&mut self, tx: EventSender) {
        self.event_tx = Some(tx);
    }

    /// Attach the shared DEX order book so the node can gossip orders.
    pub fn set_order_book(&mut self, order_book: Arc<RwLock<SwapOrderBook>>) {
        self.order_book = Some(order_book);
    }

    /// Broadcast a new order announcement to all connected peers.
    pub async fn broadcast_order(&mut self, order: &crate::atomic_swap::SwapOrder) {
        let ann = OrderAnnouncement::from_order(order);
        self.seen_orders.insert(order.order_id);
        let payload = match serde_json::to_vec(&ann) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Failed to serialize order announcement: {}", e);
                return;
            }
        };
        self.peer_manager
            .broadcast(NetMessage::new("dexorder", payload))
            .await;
    }

    /// Returns a cloned sender that can be used to submit locally-created
    /// transactions (e.g. from the RPC wallet) into the node's event loop.
    /// The node will add them to the mempool and broadcast an `inv` to peers.
    pub fn tx_submit_sender(&self) -> mpsc::Sender<Transaction> {
        self.tx_submit_tx.clone()
    }

    /// Returns a cloned sender that can be used to submit locally-minted
    /// blocks (e.g. from the regtest faucet) into the node's event loop.
    /// The node will announce them to peers via an `inv`.
    pub fn block_submit_sender(&self) -> mpsc::Sender<Block> {
        self.block_submit_tx.clone()
    }

    /// Returns a cloned sender used to enable/disable staking at runtime.
    /// Commands are processed in the node's event loop, updating the staking
    /// engine without restarting the node.
    pub fn staking_control(&self) -> mpsc::Sender<StakingCommand> {
        self.staking_control_tx.clone()
    }

    /// Emit an event to all subscribers (best-effort; silently drops if no subscribers).
    fn emit(&self, event: NodeEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(std::sync::Arc::new(event));
        }
    }

    /// Returns the path to the peer address book cache file.
    fn peers_cache_path(&self) -> PathBuf {
        self.config.data_dir.join("peers.dat")
    }

    /// Start the node — connects to peers and begins the event loop.
    pub async fn start(&mut self) -> Result<()> {
        tracing::info!("Starting vTorrent node on {}", self.config.listen_addr);

        // Ensure data directory exists
        if let Err(e) = std::fs::create_dir_all(&self.config.data_dir) {
            tracing::warn!(
                "Could not create data dir {:?}: {}",
                self.config.data_dir,
                e
            );
        }

        // ── Stage 0: Load peer cache from previous run (instant warm start) ──
        let cache_path = self.peers_cache_path();
        match self.peer_manager.addr_book.load(&cache_path) {
            Ok(n) if n > 0 => {
                tracing::info!("Warm start: {} peers loaded from cache", n);
                // Immediately try cached peers before doing any network bootstrap
                let candidates = self.peer_manager.get_peer_candidates(TARGET_OUTBOUND);
                for addr in candidates {
                    if let Err(e) = self.peer_manager.connect_addr(addr).await {
                        tracing::debug!("Cache: Could not connect to {}: {}", addr, e);
                    }
                }
            }
            Ok(_) => tracing::info!("No peer cache found — cold start"),
            Err(e) => tracing::warn!("Could not load peer cache: {}", e),
        }

        // Start P2P listener
        self.peer_manager
            .start()
            .await
            .map_err(|e| NodeError::Chain(format!("P2P start failed: {}", e)))?;

        // ── Stage 0.5: Start the overlay NAT traversal layer ─────────────────
        if self.config.use_overlay {
            self.start_overlay().await;
        }

        // ── Stage 1: DHT + Cloudflare DoH in parallel (decentralized) ────────
        if self.config.use_dht && !self.config.isolated {
            self.bootstrap_via_dht().await;
        }

        // ── Stage 2: Explicitly configured extra seeds ────────────────────────
        if !self.config.extra_seeds.is_empty() {
            self.connect_to_extra_seeds().await;
        }

        // ── Stage 3: GitHub-hosted peer list (if still no peers) ─────────────
        if !self.config.isolated && self.peer_manager.peer_count() == 0 {
            tracing::info!("No peers yet — trying GitHub bootstrap peer list...");
            self.bootstrap_via_github().await;
        }

        // ── Stage 4: Legacy DNS seeds (absolute last resort) ──────────────────
        if !self.config.isolated && self.peer_manager.peer_count() == 0 {
            tracing::warn!("No peers found via any decentralized source, trying legacy DNS seeds");
            self.connect_to_dns_seeds().await;
        }

        // Periodic timers
        let mut sync_ticker = interval(Duration::from_secs(SYNC_INTERVAL_SECS));
        let mut stake_ticker = interval(Duration::from_secs(TARGET_BLOCK_TIME));
        let mut peer_ticker = interval(Duration::from_secs(PEER_MAINTENANCE_SECS));
        let mut ping_ticker = interval(Duration::from_secs(PING_INTERVAL_SECS));
        let mut pex_ticker = interval(Duration::from_secs(PEX_INTERVAL_SECS));
        let mut dht_ticker = interval(Duration::from_secs(DHT_REANNOUNCE_SECS));

        loop {
            tokio::select! {
                // Poll peer events every 100ms
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    let events = self.peer_manager.process_events().await;
                    for event in events {
                        if let Err(e) = self.handle_peer_event(event).await {
                            tracing::warn!("Peer event error: {}", e);
                        }
                    }
                }

                // Authenticated overlay payloads share the same protocol handler as TCP peers.
                Some(ingress) = self.overlay_rx.recv() => {
                    if let Err(e) = self.handle_overlay_ingress(ingress).await {
                        tracing::warn!("Overlay peer event error: {}", e);
                    }
                }

                // Periodic sync
                _ = sync_ticker.tick() => {
                    self.request_blocks_from_peers().await;
                }

                // Periodic staking attempt
                _ = stake_ticker.tick() => {
                    if self.staking.is_some() {
                        if let Err(e) = self.attempt_stake().await {
                            tracing::debug!("Stake attempt: {}", e);
                        }
                    }
                }

                // Periodic peer maintenance
                _ = peer_ticker.tick() => {
                    self.maintain_peers().await;
                    let height = {
                        let chain = self.chain.lock().await;
                        chain.best_height()
                    };
                    let mp_size = {
                        let mp = self.mempool.lock().await;
                        mp.size()
                    };
                    tracing::info!(
                        "Peers: {} | Height: {} | Mempool: {} txs | AddrBook: {}",
                        self.peer_manager.peer_count(),
                        height,
                        mp_size,
                        self.peer_manager.addr_book.len()
                    );
                    // Persist address book to disk every minute
                    let cache_path = self.peers_cache_path();
                    if let Err(e) = self.peer_manager.addr_book.save(&cache_path) {
                        tracing::debug!("Could not save peer cache: {}", e);
                    }
                }

                // Keepalive: send ping to all connected peers every 2 minutes
                _ = ping_ticker.tick() => {
                    self.send_keepalive_pings().await;
                }

                // PEX: periodically send getaddr and self-announce
                _ = pex_ticker.tick() => {
                    self.do_pex_maintenance().await;
                }

                // DHT: periodically re-announce ourselves
                _ = dht_ticker.tick() => {
                    if self.config.use_dht && !self.config.isolated {
                        self.dht_announce().await;
                    }
                }
                // Locally-submitted transactions from RPC/wallet
                Some(tx) = self.tx_submit_rx.recv() => {
                    let txid = tx.txid();
                    let fee_sats = tx.fee_sats();
                    let size_bytes = tx.serialized_size();
                    let mut mp = self.mempool.lock().await;
                    let already_admitted = mp.get_transaction(&txid).is_some();
                    let admission = if already_admitted {
                        Ok(())
                    } else {
                        mp.add_transaction(tx)
                    };
                    match admission {
                        Ok(()) => {
                            if !already_admitted {
                                self.emit(NodeEvent::TxUnconfirmed { txid, fee_sats, size_bytes });
                            }
                            let payload = match serde_json::to_vec(&InvMsg {
                                items: vec![InvItem {
                                    inv_type: InvType::Transaction,
                                    hash: txid,
                                }],
                            }) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!("Failed to serialize inv message: {}", e);
                                    drop(mp);
                                    return Ok(());
                                }
                            };
                            drop(mp);
                            self.peer_manager.broadcast(
                                NetMessage::new("inv", payload)
                            ).await;
                            tracing::info!(
                                already_admitted,
                                "Local tx {} announced to peers",
                                hex::encode(txid)
                            );
                        }
                        Err(e) => {
                            tracing::warn!("Local tx rejected by mempool: {}", e);
                        }
                    }
                }
                // Locally-minted blocks from the regtest faucet
                Some(block) = self.block_submit_rx.recv() => {
                    let block_hash = block.hash();
                    let payload = match serde_json::to_vec(&InvMsg {
                        items: vec![InvItem {
                            inv_type: InvType::Block,
                            hash: block_hash,
                        }],
                    }) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!("Failed to serialize block inv: {}", e);
                            continue;
                        }
                    };
                    self.peer_manager.broadcast(
                        NetMessage::new("inv", payload)
                    ).await;
                    tracing::info!(
                        "Local block {} announced to peers",
                        hex::encode(block_hash)
                    );
                }
                // Runtime staking control from RPC/tauri
                Some(cmd) = self.staking_rx.recv() => {
                    match cmd {
                        StakingCommand::Start { address, wif } => {
                            self.staking = Some(match wif {
                                Some(w) => StakingEngine::with_wif(address, w),
                                None => StakingEngine::new(address),
                            });
                            tracing::info!("Staking enabled via runtime control");
                        }
                        StakingCommand::Stop => {
                            self.staking = None;
                            tracing::info!("Staking disabled via runtime control");
                        }
                    }
                }
            }
        }
    }

    /// Bootstrap peer discovery using the BitTorrent DHT network AND
    /// Cloudflare DNS-over-HTTPS simultaneously.
    ///
    /// Both sources run in parallel (via `spawn_blocking`) and their results
    /// are merged into the PEX address book before connection attempts begin.
    /// This ensures we can bootstrap even when UDP (DHT) or plain DNS is blocked.
    async fn bootstrap_via_dht(&mut self) {
        tracing::info!("Starting parallel DHT + Cloudflare DoH bootstrap...");

        let port = self
            .config
            .listen_addr
            .split(':')
            .next_back()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        // Spawn both bootstrap methods concurrently
        let dht_task = tokio::task::spawn_blocking(move || {
            let dht = DhtBootstrap::new();
            dht.discover_peers()
        });

        let doh_task = tokio::task::spawn_blocking(move || discover_peers_via_doh(port));

        // Wait for both to complete (each has its own internal timeout)
        let (dht_peers, doh_peers) = tokio::join!(dht_task, doh_task);

        let dht_peers = dht_peers.unwrap_or_default();
        let doh_peers = doh_peers.unwrap_or_default();

        tracing::info!(
            "Bootstrap complete: DHT={} candidates, DoH={} candidates",
            dht_peers.len(),
            doh_peers.len()
        );

        // Merge both sources into the PEX address book
        if !dht_peers.is_empty() {
            self.peer_manager.add_dht_peers(dht_peers);
        }
        if !doh_peers.is_empty() {
            self.peer_manager.add_dht_peers(doh_peers);
        }

        if self.peer_manager.addr_book.is_empty() {
            tracing::warn!("Both DHT and DoH bootstrap returned no peers");
            return;
        }

        // Attempt to connect to the best candidates immediately
        let candidates = self.peer_manager.get_peer_candidates(TARGET_OUTBOUND);
        for addr in candidates {
            tracing::info!("Bootstrap: Connecting to peer candidate {}", addr);
            if let Err(e) = self.peer_manager.connect_addr(addr).await {
                tracing::debug!("Bootstrap: Could not connect to {}: {}", addr, e);
            }
        }
    }

    /// Announce ourselves on the DHT so other nodes can find us.
    async fn dht_announce(&self) {
        let port = self
            .config
            .listen_addr
            .split(':')
            .next_back()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        let dht = DhtBootstrap::new();
        tokio::task::spawn_blocking(move || {
            dht.announce(port);
        });
    }

    /// Connect to explicitly configured extra seed nodes.
    async fn connect_to_extra_seeds(&mut self) {
        for seed in self.config.extra_seeds.clone() {
            tracing::info!("Connecting to extra seed: {}", seed);
            if let Err(e) = self.peer_manager.connect(&seed).await {
                tracing::debug!("Could not connect to {}: {}", seed, e);
            }
        }
    }

    /// Bootstrap from the GitHub-hosted peer list (Stage 3 fallback).
    async fn bootstrap_via_github(&mut self) {
        let port = self
            .config
            .listen_addr
            .split(':')
            .next_back()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        let peers = tokio::task::spawn_blocking(move || discover_peers_via_github(port))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("GitHub bootstrap task failed: {}", e);
                Vec::new()
            });

        if peers.is_empty() {
            tracing::debug!("GitHub bootstrap: no peers returned");
            return;
        }

        tracing::info!("GitHub bootstrap: {} peer candidates found", peers.len());
        self.peer_manager.add_dht_peers(peers);

        let candidates = self.peer_manager.get_peer_candidates(TARGET_OUTBOUND);
        for addr in candidates {
            tracing::info!("GitHub: Connecting to peer candidate {}", addr);
            if let Err(e) = self.peer_manager.connect_addr(addr).await {
                tracing::debug!("GitHub: Could not connect to {}: {}", addr, e);
            }
        }
    }

    /// Connect to legacy DNS seed nodes (fallback only).
    async fn connect_to_dns_seeds(&mut self) {
        use vtorrent_p2p::peer_manager::DNS_SEEDS;
        let seeds: Vec<String> = DNS_SEEDS
            .iter()
            .map(|s| format!("{}:{}", s, DEFAULT_PORT))
            .collect();

        for seed in seeds {
            tracing::info!("Connecting to DNS seed (fallback): {}", seed);
            if let Err(e) = self.peer_manager.connect(&seed).await {
                tracing::debug!("Could not connect to {}: {}", seed, e);
            }
        }
    }

    /// Handle a peer event from the P2P layer.
    async fn handle_peer_event(&mut self, event: PeerEvent) -> Result<()> {
        match event {
            PeerEvent::HandshakeComplete { peer_addr, version } => {
                tracing::info!(
                    "Peer {} handshake complete: {} (height {})",
                    peer_addr,
                    version.user_agent,
                    version.start_height
                );
                self.emit(NodeEvent::PeerConnected {
                    addr: peer_addr,
                    user_agent: version.user_agent.clone(),
                    version: version.version,
                    height: version.start_height,
                });
                // Negotiate compact block relay (BIP-152)
                // We use low-bandwidth mode (0) by default; high-bandwidth (1) is for the 3 fastest peers
                let sendcmpct_payload = match serde_json::to_vec(&SendCmpctMsg {
                    high_bandwidth: false,
                    version: 1,
                }) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("Failed to serialize sendcmpct: {}", e);
                        return Ok(());
                    }
                };
                self.peer_manager
                    .send_to(peer_addr, NetMessage::new("sendcmpct", sendcmpct_payload))
                    .await;
                // Track compact block state for this peer
                self.compact_peers
                    .insert(peer_addr, CompactBlockPeerState::default());
                // Ask peer for blocks if they are ahead of us
                let our_height = {
                    let chain = self.chain.lock().await;
                    chain.best_height()
                };
                if version.start_height > our_height {
                    let best_hash = {
                        let chain = self.chain.lock().await;
                        chain.best_hash().unwrap_or([0u8; 32])
                    };
                    let payload = serde_json::to_vec(&GetBlocksMsg {
                        version: 70001,
                        block_locator_hashes: vec![best_hash],
                        hash_stop: [0u8; 32],
                    })
                    .unwrap_or_default();
                    let msg = NetMessage::new("getblocks", payload);
                    self.peer_manager.broadcast(msg).await;
                }

                // Immediately request peer's address list (PEX bootstrap)
                if self.peer_manager.should_getaddr() {
                    let getaddr = PeerManager::build_getaddr();
                    self.peer_manager.send_to(peer_addr, getaddr).await;
                    self.peer_manager.record_getaddr();
                }
            }

            PeerEvent::Message { peer_addr, msg } => {
                self.handle_message(peer_addr, msg).await?;
            }

            PeerEvent::Disconnected { peer_addr } => {
                tracing::info!("Peer {} disconnected", peer_addr);
                self.emit(NodeEvent::PeerDisconnected { addr: peer_addr });
                // Clean up per-peer state
                self.compact_peers.remove(&peer_addr);
                self.peer_fee_filters.remove(&peer_addr);
                self.peer_ping_nonces.remove(&peer_addr);
                self.peer_msg_counts.remove(&peer_addr);
            }
        }
        Ok(())
    }

    /// Handle an authenticated overlay event through the same P2P command path
    /// used for TCP peers. Overlay sessions retain their own encryption and send
    /// bridge, while protocol semantics stay centralized in `handle_message`.
    async fn handle_overlay_ingress(&mut self, ingress: OverlayIngress) -> Result<()> {
        match ingress {
            OverlayIngress::PeerConnected { node_id } => {
                let peer_addr = overlay_peer_addr(&node_id)?;
                let overlay = self.overlay.clone().ok_or_else(|| {
                    NodeError::Chain("Received overlay peer event before overlay startup".into())
                })?;
                let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
                let relay_node_id = node_id.clone();

                tokio::spawn(async move {
                    while let Some(command) = cmd_rx.recv().await {
                        match command {
                            PeerCommand::Send(msg) => {
                                let packet = encode_overlay_message(&msg);
                                if let Err(e) = overlay.send(&relay_node_id, &packet).await {
                                    tracing::debug!(peer = %relay_node_id, "Overlay send failed: {}", e);
                                    break;
                                }
                            }
                            PeerCommand::Disconnect => break,
                        }
                    }
                });

                self.peer_manager
                    .register_virtual_peer(
                        peer_addr,
                        format!("/vTorrent-overlay:{}/", &node_id[..8]),
                        cmd_tx,
                    )
                    .map_err(|e| {
                        NodeError::Chain(format!("Overlay peer registration failed: {}", e))
                    })?;
                self.overlay_peers.insert(peer_addr, node_id);
                self.overlay_handshaken.remove(&peer_addr);

                let best_height = {
                    let chain = self.chain.lock().await;
                    chain.best_height()
                };
                let version = VersionMsg::new(best_height, &self.config.listen_addr);
                let payload = serde_json::to_vec(&version).map_err(|e| {
                    NodeError::Chain(format!("Overlay version serialization failed: {}", e))
                })?;
                self.peer_manager
                    .send_to(peer_addr, NetMessage::new("version", payload))
                    .await;
            }
            OverlayIngress::PeerDisconnected { node_id } => {
                if let Some(peer_addr) = self
                    .overlay_peers
                    .iter()
                    .find_map(|(addr, known_id)| (known_id == &node_id).then_some(*addr))
                {
                    self.peer_manager.remove_virtual_peer(peer_addr);
                    self.overlay_peers.remove(&peer_addr);
                    self.overlay_handshaken.remove(&peer_addr);
                    self.compact_peers.remove(&peer_addr);
                    self.peer_fee_filters.remove(&peer_addr);
                    self.peer_ping_nonces.remove(&peer_addr);
                    self.emit(NodeEvent::PeerDisconnected { addr: peer_addr });
                }
            }
            OverlayIngress::Message { node_id, msg } => {
                let peer_addr = overlay_peer_addr(&node_id)?;
                if self.overlay_peers.get(&peer_addr) != Some(&node_id) {
                    tracing::debug!(peer = %node_id, "Ignoring message from unknown overlay peer");
                    return Ok(());
                }

                match msg.command_str() {
                    "version" => {
                        let version: VersionMsg =
                            serde_json::from_slice(&msg.payload).map_err(|e| {
                                NodeError::Chain(format!("Invalid overlay version message: {}", e))
                            })?;
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("verack", Vec::new()))
                            .await;
                        if self.overlay_handshaken.insert(peer_addr) {
                            self.handle_peer_event(PeerEvent::HandshakeComplete {
                                peer_addr,
                                version,
                            })
                            .await?;
                        }
                    }
                    "verack" => {
                        tracing::trace!(peer = %node_id, "Overlay peer acknowledged version");
                    }
                    _ if !self.overlay_handshaken.contains(&peer_addr) => {
                        tracing::debug!(peer = %node_id, command = msg.command_str(), "Ignoring pre-handshake overlay message");
                    }
                    _ => self.handle_message(peer_addr, msg).await?,
                }
            }
        }
        Ok(())
    }

    /// Handle a raw network message from a peer.
    async fn handle_message(
        &mut self,
        peer_addr: std::net::SocketAddr,
        msg: NetMessage,
    ) -> Result<()> {
        use vtorrent_p2p::ban_manager::Misbehaviour;

        // Per-peer flood rate limiting: a peer that exceeds the message budget
        // within a window is banned and disconnected.
        let now = now_secs();
        let (count, window_start) = self.peer_msg_counts.entry(peer_addr).or_insert((0, now));
        if now.saturating_sub(*window_start) >= MSG_WINDOW_SECS {
            *count = 0;
            *window_start = now;
        }
        *count += 1;
        if *count > MAX_MSGS_PER_WINDOW {
            tracing::warn!(
                "Peer {} exceeded {} messages/{}s; banning",
                peer_addr,
                MAX_MSGS_PER_WINDOW,
                MSG_WINDOW_SECS
            );
            self.peer_manager
                .record_misbehaviour(peer_addr, Misbehaviour::Custom(100))
                .await;
            return Ok(());
        }

        match msg.command_str() {
            // ── PEX: Peer Exchange ────────────────────────────────────────────
            "addr" => {
                if let Ok(addr_msg) = serde_json::from_slice::<AddrMsg>(&msg.payload) {
                    let count = addr_msg.addrs.len();
                    self.peer_manager.handle_addr_msg(&addr_msg);
                    tracing::debug!("PEX: Received {} addresses from {}", count, peer_addr);
                } else {
                    self.peer_manager
                        .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                        .await;
                }
            }

            "getaddr" => {
                // Respond with our known peer list
                let response = self.peer_manager.build_addr_response();
                self.peer_manager.send_to(peer_addr, response).await;
                tracing::debug!("PEX: Sent addr response to {}", peer_addr);
            }

            // ── Inventory ─────────────────────────────────────────────────────
            "inv" => {
                if let Ok(inv) = serde_json::from_slice::<InvMsg>(&msg.payload) {
                    let mut want = Vec::new();
                    for item in &inv.items {
                        match item.inv_type {
                            InvType::Block => {
                                let chain = self.chain.lock().await;
                                if chain.get_block(&item.hash).is_none() {
                                    want.push(item.clone());
                                }
                            }
                            InvType::Transaction => {
                                let mp = self.mempool.lock().await;
                                if mp.get_transaction(&item.hash).is_none() {
                                    want.push(item.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                    if !want.is_empty() {
                        let payload =
                            serde_json::to_vec(&vtorrent_p2p::message::GetDataMsg { items: want })
                                .unwrap_or_default();
                        self.peer_manager
                            .broadcast(NetMessage::new("getdata", payload))
                            .await;
                    }
                }
            }

            "block" => {
                // Block payload is raw bytes — deserialize and add to chain
                match self.deserialize_block(&msg.payload) {
                    Ok(block) => {
                        // Extract metadata before add_block consumes the block.
                        let hash = block.hash();
                        let tx_count = block.transactions.len();
                        let timestamp = block.header.timestamp;
                        let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                        let block_arc = std::sync::Arc::new(block);
                        let mut chain = self.chain.lock().await;
                        match chain.add_block((*block_arc).clone()) {
                            Ok(acceptance) => {
                                use crate::chain::BlockAcceptance;
                                let should_relay = match acceptance {
                                    BlockAcceptance::MainChain {
                                        height,
                                        utxos_added,
                                        utxos_removed,
                                        claimed_addresses,
                                    } => {
                                        tracing::info!(
                                            "Accepted block {} at height {}",
                                            hex::encode(hash),
                                            height
                                        );
                                        {
                                            let confirmed: Vec<[u8; 32]> = block_arc
                                                .transactions
                                                .iter()
                                                .map(|tx| tx.txid())
                                                .collect();
                                            let mut mp = self.mempool.lock().await;
                                            mp.handle_confirmed_block(&confirmed, &utxos_removed);
                                        }
                                        self.emit(NodeEvent::NewBlock {
                                            height,
                                            hash,
                                            tx_count,
                                            timestamp,
                                            size_bytes,
                                            block: block_arc.clone(),
                                            utxos_added,
                                            utxos_removed,
                                            claimed_addresses,
                                        });
                                        // Emit tx_confirmed for each transaction
                                        for tx in block_arc.transactions.iter() {
                                            self.emit(NodeEvent::TxConfirmed {
                                                txid: tx.txid(),
                                                block_height: height,
                                                block_hash: hash,
                                            });
                                        }
                                        true
                                    }
                                    BlockAcceptance::Reorg {
                                        old_tip,
                                        new_tip,
                                        depth,
                                    } => {
                                        tracing::warn!(
                                            "Reorg depth {}: {} -> {}",
                                            depth,
                                            hex::encode(old_tip),
                                            hex::encode(new_tip)
                                        );
                                        self.emit(NodeEvent::Reorg {
                                            old_tip,
                                            new_tip,
                                            depth,
                                        });
                                        true
                                    }
                                    BlockAcceptance::Fork { fork_tip } => {
                                        tracing::debug!(
                                            "Fork block {} stored",
                                            hex::encode(fork_tip)
                                        );
                                        false
                                    }
                                    BlockAcceptance::Duplicate => false,
                                };
                                if should_relay {
                                    let payload = serde_json::to_vec(&InvMsg {
                                        items: vec![InvItem {
                                            inv_type: InvType::Block,
                                            hash,
                                        }],
                                    })
                                    .unwrap_or_default();
                                    drop(chain);
                                    self.peer_manager
                                        .broadcast_except(
                                            peer_addr,
                                            NetMessage::new("inv", payload),
                                        )
                                        .await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Rejected block from {}: {}", peer_addr, e);
                                self.peer_manager
                                    .record_misbehaviour(peer_addr, Misbehaviour::InvalidBlock)
                                    .await;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to deserialize block from {}: {}", peer_addr, e);
                        self.peer_manager
                            .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                            .await;
                    }
                }
            }

            "tx" => match self.deserialize_tx(&msg.payload) {
                Ok(tx) => {
                    // Compute the real fee from the UTXO set rather than the
                    // fabricated per-input estimate, so zero-fee transactions
                    // cannot be relayed through the network.
                    let real_fee = {
                        let chain = self.chain.lock().await;
                        chain.compute_tx_fee(&tx)
                    };
                    let mut mp = self.mempool.lock().await;
                    let result = match real_fee {
                        Some(fee) => mp.add_transaction_with_fee(tx.clone(), fee),
                        None => Err(NodeError::Chain("Inputs not found in UTXO set".into())),
                    };
                    match result {
                        Ok(()) => {
                            let txid = tx.txid();
                            let fee_sats = real_fee.unwrap_or(0);
                            let size_bytes = tx.serialized_size();
                            tracing::debug!("Accepted tx {}", hex::encode(txid));
                            self.emit(NodeEvent::TxUnconfirmed {
                                txid,
                                fee_sats,
                                size_bytes,
                            });
                            let payload = serde_json::to_vec(&InvMsg {
                                items: vec![InvItem {
                                    inv_type: InvType::Transaction,
                                    hash: txid,
                                }],
                            })
                            .unwrap_or_default();
                            drop(mp);
                            self.peer_manager
                                .broadcast_except(peer_addr, NetMessage::new("inv", payload))
                                .await;
                        }
                        Err(NodeError::PolicyRejected(_)) => {
                            // Policy rejection (e.g. fee below relay floor):
                            // the tx may be valid, so do not penalize the peer.
                            tracing::debug!("Rejected tx by policy");
                        }
                        Err(e) => {
                            tracing::debug!("Rejected tx: {}", e);
                            self.peer_manager
                                .record_misbehaviour(peer_addr, Misbehaviour::InvalidTransaction)
                                .await;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to deserialize tx from {}: {}", peer_addr, e);
                    self.peer_manager
                        .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                        .await;
                }
            },

            "getblocks" => {
                if let Ok(req) = serde_json::from_slice::<GetBlocksMsg>(&msg.payload) {
                    let chain = self.chain.lock().await;
                    let our_height = chain.best_height();

                    // Find the peer's best known block height
                    let start_height = req
                        .block_locator_hashes
                        .iter()
                        .find_map(|hash| {
                            for h in 0..=our_height {
                                if let Some(b) = chain.get_block_at_height(h) {
                                    if b.hash() == *hash {
                                        return Some(h + 1);
                                    }
                                }
                            }
                            None
                        })
                        .unwrap_or(1);

                    let mut items = Vec::new();
                    for h in start_height..=our_height.min(start_height + 500) {
                        if let Some(block) = chain.get_block_at_height(h) {
                            items.push(InvItem {
                                inv_type: InvType::Block,
                                hash: block.hash(),
                            });
                        }
                    }

                    if !items.is_empty() {
                        let payload = serde_json::to_vec(&InvMsg { items }).unwrap_or_default();
                        drop(chain);
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("inv", payload))
                            .await;
                    }
                }
            }

            // ── Compact Block Relay (BIP-152) ─────────────────────────────────
            "sendcmpct" => {
                if let Ok(msg_data) = serde_json::from_slice::<SendCmpctMsg>(&msg.payload) {
                    let state = self.compact_peers.entry(peer_addr).or_default();
                    state.enabled = true;
                    state.high_bandwidth = msg_data.high_bandwidth;
                    state.version = msg_data.version;
                    tracing::debug!(
                        "Peer {} supports compact blocks (high_bw={}, v={})",
                        peer_addr,
                        msg_data.high_bandwidth,
                        msg_data.version
                    );
                }
            }

            "cmpctblock" => {
                if let Ok(cmpct) = serde_json::from_slice::<CmpctBlockMsg>(&msg.payload) {
                    // Build the header bytes for SipHash key derivation
                    let mut header_bytes = Vec::with_capacity(80);
                    header_bytes.extend_from_slice(&cmpct.version.to_le_bytes());
                    header_bytes.extend_from_slice(&cmpct.prev_block_hash);
                    header_bytes.extend_from_slice(&cmpct.merkle_root);
                    header_bytes.extend_from_slice(&cmpct.timestamp.to_le_bytes());
                    header_bytes.extend_from_slice(&cmpct.bits.to_le_bytes());
                    header_bytes.extend_from_slice(&cmpct.nonce.to_le_bytes());
                    let (k0, k1) = derive_siphash_keys(&header_bytes, cmpct.siphash_nonce);

                    // Build a mempool lookup map: short_txid → serialized tx bytes
                    let mempool_map = {
                        let mp = self.mempool.lock().await;
                        let entries = mp.get_entries();
                        let mut map = std::collections::HashMap::new();
                        for entry in entries {
                            let txid = entry.tx.txid();
                            let sid = short_txid(&txid, k0, k1);
                            if let Ok(bytes) = serde_json::to_vec(&entry.tx) {
                                map.insert(sid, bytes);
                            }
                        }
                        map
                    };

                    match CompactBlockDecoder::decode(&cmpct, &mempool_map) {
                        Ok(tx_bytes_list) => {
                            // Reconstruct the full block from the decoded transactions
                            let mut txs: Vec<Transaction> = Vec::new();
                            let mut all_ok = true;
                            for bytes in &tx_bytes_list {
                                match serde_json::from_slice::<Transaction>(bytes) {
                                    Ok(tx) => txs.push(tx),
                                    Err(e) => {
                                        tracing::warn!(
                                            "cmpctblock: failed to decode tx from {}: {}",
                                            peer_addr,
                                            e
                                        );
                                        all_ok = false;
                                        break;
                                    }
                                }
                            }
                            if all_ok {
                                // The compact-block header does not carry the stake
                                // modifier; derive it from the parent block so the
                                // reconstructed block validates against the chain.
                                let stake_modifier = {
                                    let chain = self.chain.lock().await;
                                    chain
                                        .get_block(&cmpct.prev_block_hash)
                                        .map(|p| {
                                            compute_stake_modifier(
                                                p.header.stake_modifier,
                                                &cmpct.prev_block_hash,
                                            )
                                        })
                                        .unwrap_or(0)
                                };
                                let block = Block {
                                    header: BlockHeader {
                                        version: cmpct.version,
                                        prev_block_hash: cmpct.prev_block_hash,
                                        merkle_root: cmpct.merkle_root,
                                        timestamp: cmpct.timestamp,
                                        bits: cmpct.bits,
                                        nonce: cmpct.nonce,
                                        stake_modifier,
                                    },
                                    transactions: txs,
                                };
                                let hash = block.hash();
                                let tx_count = block.transactions.len();
                                let timestamp = block.header.timestamp;
                                let size_bytes =
                                    serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                                let block_arc = std::sync::Arc::new(block);
                                let mut chain = self.chain.lock().await;
                                match chain.add_block((*block_arc).clone()) {
                                    Ok(acceptance) => {
                                        use crate::chain::BlockAcceptance;
                                        if let BlockAcceptance::MainChain {
                                            height,
                                            utxos_added,
                                            utxos_removed,
                                            claimed_addresses,
                                        } = acceptance
                                        {
                                            tracing::info!(
                                                "cmpctblock: accepted block {} at height {}",
                                                hex::encode(hash),
                                                height
                                            );
                                            {
                                                let confirmed: Vec<[u8; 32]> = block_arc
                                                    .transactions
                                                    .iter()
                                                    .map(|tx| tx.txid())
                                                    .collect();
                                                let mut mp = self.mempool.lock().await;
                                                mp.handle_confirmed_block(
                                                    &confirmed,
                                                    &utxos_removed,
                                                );
                                            }
                                            self.emit(NodeEvent::NewBlock {
                                                height,
                                                hash,
                                                tx_count,
                                                timestamp,
                                                size_bytes,
                                                block: block_arc.clone(),
                                                utxos_added,
                                                utxos_removed,
                                                claimed_addresses,
                                            });
                                            for tx in block_arc.transactions.iter() {
                                                self.emit(NodeEvent::TxConfirmed {
                                                    txid: tx.txid(),
                                                    block_height: height,
                                                    block_hash: hash,
                                                });
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "cmpctblock: rejected block from {}: {}",
                                            peer_addr,
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        Err(CompactBlockDecodeError::MissingTransactions(missing_indexes)) => {
                            // Some transactions are missing from our mempool — request them
                            tracing::debug!(
                                "cmpctblock: {} missing txs from {}, sending getblocktxn",
                                missing_indexes.len(),
                                peer_addr
                            );
                            let mut header_bytes2 = Vec::with_capacity(80);
                            header_bytes2.extend_from_slice(&cmpct.version.to_le_bytes());
                            header_bytes2.extend_from_slice(&cmpct.prev_block_hash);
                            header_bytes2.extend_from_slice(&cmpct.merkle_root);
                            header_bytes2.extend_from_slice(&cmpct.timestamp.to_le_bytes());
                            header_bytes2.extend_from_slice(&cmpct.bits.to_le_bytes());
                            header_bytes2.extend_from_slice(&cmpct.nonce.to_le_bytes());
                            // Compute block hash as the hash of the header bytes
                            let block_hash = {
                                use sha2::{Digest, Sha256};
                                let h1 = Sha256::digest(&header_bytes2);
                                let h2 = Sha256::digest(h1);
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&h2);
                                arr
                            };
                            let req =
                                CompactBlockDecoder::build_getblocktxn(block_hash, missing_indexes);
                            let payload = serde_json::to_vec(&req).unwrap_or_default();
                            self.peer_manager
                                .send_to(peer_addr, NetMessage::new("getblocktxn", payload))
                                .await;
                            // Save the partial compact block so blocktxn can complete it.
                            self.pending_compact_blocks.insert(block_hash, cmpct);
                        }
                        Err(CompactBlockDecodeError::TooManyTransactions) => {
                            tracing::warn!(
                                "cmpctblock: rejecting block with too many transactions from {}",
                                peer_addr
                            );
                        }
                        Err(CompactBlockDecodeError::InvalidPrefilledIndex) => {
                            // Protocol violation: score the peer so repeat
                            // offenders get banned.
                            tracing::warn!(
                                "cmpctblock: invalid prefilled index from {}",
                                peer_addr
                            );
                            let _ = self
                                .peer_manager
                                .record_misbehaviour(
                                    peer_addr,
                                    vtorrent_p2p::ban_manager::Misbehaviour::MalformedMessage,
                                )
                                .await;
                        }
                    }
                }
            }

            "getblocktxn" => {
                if let Ok(req) = serde_json::from_slice::<GetBlockTxnMsg>(&msg.payload) {
                    let chain = self.chain.lock().await;
                    // Find the block by hash
                    let our_height = chain.best_height();
                    let mut found_txs: Option<Vec<Vec<u8>>> = None;
                    'outer: for h in 0..=our_height {
                        if let Some(block) = chain.get_block_at_height(h) {
                            if block.hash() == req.block_hash {
                                let mut txs = Vec::new();
                                for &idx in &req.indexes {
                                    let idx = idx as usize;
                                    if idx < block.transactions.len() {
                                        if let Ok(bytes) =
                                            serde_json::to_vec(&block.transactions[idx])
                                        {
                                            txs.push(bytes);
                                        }
                                    }
                                }
                                found_txs = Some(txs);
                                break 'outer;
                            }
                        }
                    }
                    if let Some(txs) = found_txs {
                        let resp = BlockTxnMsg {
                            block_hash: req.block_hash,
                            transactions: txs,
                        };
                        let payload = serde_json::to_vec(&resp).unwrap_or_default();
                        drop(chain);
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("blocktxn", payload))
                            .await;
                    }
                }
            }

            "blocktxn" => {
                // blocktxn arrives after we sent getblocktxn for a compact block.
                // Reconstruct the full block from the saved compact block + received txs.
                if let Ok(resp) = serde_json::from_slice::<BlockTxnMsg>(&msg.payload) {
                    let Some(pending) = self.pending_compact_blocks.remove(&resp.block_hash) else {
                        tracing::debug!(
                            "blocktxn: no pending compact block for {} from {}",
                            hex::encode(resp.block_hash),
                            peer_addr
                        );
                        return Ok(());
                    };
                    tracing::debug!(
                        "blocktxn: received {} txs for block {} from {}, completing reconstruction",
                        resp.transactions.len(),
                        hex::encode(resp.block_hash),
                        peer_addr
                    );

                    // Build SipHash keys from the compact block header.
                    let mut header_bytes = Vec::with_capacity(80);
                    header_bytes.extend_from_slice(&pending.version.to_le_bytes());
                    header_bytes.extend_from_slice(&pending.prev_block_hash);
                    header_bytes.extend_from_slice(&pending.merkle_root);
                    header_bytes.extend_from_slice(&pending.timestamp.to_le_bytes());
                    header_bytes.extend_from_slice(&pending.bits.to_le_bytes());
                    header_bytes.extend_from_slice(&pending.nonce.to_le_bytes());
                    let (k0, k1) = derive_siphash_keys(&header_bytes, pending.siphash_nonce);

                    // Build maps: mempool txs by short_id, and received txs by index.
                    let mempool_map = {
                        let mp = self.mempool.lock().await;
                        let entries = mp.get_entries();
                        let mut map = std::collections::HashMap::new();
                        for entry in entries {
                            let txid = entry.tx.txid();
                            let sid = short_txid(&txid, k0, k1);
                            if let Ok(bytes) = serde_json::to_vec(&entry.tx) {
                                map.insert(sid, bytes);
                            }
                        }
                        map
                    };
                    let mut received_map = std::collections::HashMap::new();
                    for (i, tx_bytes) in resp.transactions.iter().enumerate() {
                        received_map.insert(i, tx_bytes.clone());
                    }

                    // Decode: fill short_ids from mempool_map, missing from received_map.
                    match CompactBlockDecoder::decode_with_received(
                        &pending,
                        &mempool_map,
                        &received_map,
                    ) {
                        Ok(tx_bytes_list) => {
                            let mut txs: Vec<Transaction> = Vec::new();
                            let mut all_ok = true;
                            for bytes in &tx_bytes_list {
                                match serde_json::from_slice::<Transaction>(bytes) {
                                    Ok(tx) => txs.push(tx),
                                    Err(e) => {
                                        tracing::warn!(
                                            "blocktxn: failed to decode tx from {}: {}",
                                            peer_addr,
                                            e
                                        );
                                        all_ok = false;
                                        break;
                                    }
                                }
                            }
                            if all_ok {
                                let stake_modifier = {
                                    let chain = self.chain.lock().await;
                                    chain
                                        .get_block(&pending.prev_block_hash)
                                        .map(|p| {
                                            compute_stake_modifier(
                                                p.header.stake_modifier,
                                                &pending.prev_block_hash,
                                            )
                                        })
                                        .unwrap_or(0)
                                };
                                let block = Block {
                                    header: BlockHeader {
                                        version: pending.version,
                                        prev_block_hash: pending.prev_block_hash,
                                        merkle_root: pending.merkle_root,
                                        timestamp: pending.timestamp,
                                        bits: pending.bits,
                                        nonce: pending.nonce,
                                        stake_modifier,
                                    },
                                    transactions: txs,
                                };
                                let hash = block.hash();
                                let tx_count = block.transactions.len();
                                let timestamp = block.header.timestamp;
                                let size_bytes =
                                    serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                                let block_arc = std::sync::Arc::new(block);
                                let mut chain = self.chain.lock().await;
                                match chain.add_block((*block_arc).clone()) {
                                    Ok(acceptance) => {
                                        use crate::chain::BlockAcceptance;
                                        if let BlockAcceptance::MainChain {
                                            height,
                                            utxos_added,
                                            utxos_removed,
                                            claimed_addresses,
                                        } = acceptance
                                        {
                                            tracing::info!(
                                                "blocktxn: accepted block {} at height {}",
                                                hex::encode(hash),
                                                height
                                            );
                                            {
                                                let confirmed: Vec<[u8; 32]> = block_arc
                                                    .transactions
                                                    .iter()
                                                    .map(|tx| tx.txid())
                                                    .collect();
                                                let mut mp = self.mempool.lock().await;
                                                mp.handle_confirmed_block(
                                                    &confirmed,
                                                    &utxos_removed,
                                                );
                                            }
                                            self.emit(NodeEvent::NewBlock {
                                                height,
                                                hash,
                                                tx_count,
                                                timestamp,
                                                size_bytes,
                                                block: block_arc.clone(),
                                                utxos_added,
                                                utxos_removed,
                                                claimed_addresses,
                                            });
                                            for tx in block_arc.transactions.iter() {
                                                self.emit(NodeEvent::TxConfirmed {
                                                    txid: tx.txid(),
                                                    block_height: height,
                                                    block_hash: hash,
                                                });
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "cmpctblock: rejected completed block from {}: {}",
                                            peer_addr,
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "blocktxn: failed to complete block from {}: {:?}",
                                peer_addr,
                                e
                            );
                        }
                    }
                }
            }

            // ── Keepalive ─────────────────────────────────────────────────────
            "ping" => {
                // peer.rs already handles inbound ping→pong at the peer level;
                // this arm handles any ping that bubbles up (e.g. from test harness).
                if let Ok(ping) = serde_json::from_slice::<PingMsg>(&msg.payload) {
                    let payload =
                        serde_json::to_vec(&PingMsg { nonce: ping.nonce }).unwrap_or_default();
                    self.peer_manager
                        .send_to(peer_addr, NetMessage::new("pong", payload))
                        .await;
                }
            }

            "pong" => {
                // Validate the nonce matches what we sent
                if let Ok(pong) = serde_json::from_slice::<PingMsg>(&msg.payload) {
                    if let Some(&expected) = self.peer_ping_nonces.get(&peer_addr) {
                        if pong.nonce == expected {
                            self.peer_ping_nonces.remove(&peer_addr);
                            tracing::trace!(
                                "Pong from {} confirmed (nonce={})",
                                peer_addr,
                                pong.nonce
                            );
                        } else {
                            tracing::warn!(
                                "Pong nonce mismatch from {}: expected {} got {}",
                                peer_addr,
                                expected,
                                pong.nonce
                            );
                        }
                    }
                }
            }

            // ── Fee filter ────────────────────────────────────────────────────
            "feefilter" => {
                if let Ok(ff) = serde_json::from_slice::<FeeFilterMsg>(&msg.payload) {
                    self.peer_fee_filters.insert(peer_addr, ff.feerate);
                    tracing::debug!(
                        "feefilter: peer {} min fee rate = {} sat/kB",
                        peer_addr,
                        ff.feerate
                    );
                }
            }

            // ── Not-found ─────────────────────────────────────────────────────
            "notfound" => {
                if let Ok(nf) = serde_json::from_slice::<InvMsg>(&msg.payload) {
                    for item in &nf.items {
                        tracing::debug!(
                            "notfound: peer {} does not have {:?} {}",
                            peer_addr,
                            item.inv_type,
                            hex::encode(item.hash)
                        );
                    }
                }
            }

            // ── getdata: serve blocks and transactions to requesting peers ──────
            "getdata" => {
                if let Ok(req) =
                    serde_json::from_slice::<vtorrent_p2p::message::GetDataMsg>(&msg.payload)
                {
                    for item in &req.items {
                        match item.inv_type {
                            InvType::Block => {
                                let maybe_block = {
                                    let chain = self.chain.lock().await;
                                    chain.get_block(&item.hash).cloned()
                                };
                                if let Some(block) = maybe_block {
                                    match serde_json::to_vec(&block) {
                                        Ok(payload) => {
                                            self.peer_manager
                                                .send_to(
                                                    peer_addr,
                                                    NetMessage::new("block", payload),
                                                )
                                                .await;
                                            tracing::debug!(
                                                "getdata: served block {} to {}",
                                                hex::encode(item.hash),
                                                peer_addr
                                            );
                                        }
                                        Err(e) => tracing::warn!(
                                            "getdata: failed to serialize block {}: {}",
                                            hex::encode(item.hash),
                                            e
                                        ),
                                    }
                                } else {
                                    // Block not found — reply with notfound
                                    let nf = serde_json::to_vec(&InvMsg {
                                        items: vec![item.clone()],
                                    })
                                    .unwrap_or_default();
                                    self.peer_manager
                                        .send_to(peer_addr, NetMessage::new("notfound", nf))
                                        .await;
                                }
                            }
                            InvType::Transaction => {
                                let maybe_tx = {
                                    let mp = self.mempool.lock().await;
                                    mp.get_transaction(&item.hash).cloned()
                                };
                                if let Some(tx) = maybe_tx {
                                    match serde_json::to_vec(&tx) {
                                        Ok(payload) => {
                                            self.peer_manager
                                                .send_to(peer_addr, NetMessage::new("tx", payload))
                                                .await;
                                            tracing::debug!(
                                                "getdata: served tx {} to {}",
                                                hex::encode(item.hash),
                                                peer_addr
                                            );
                                        }
                                        Err(e) => tracing::warn!(
                                            "getdata: failed to serialize tx {}: {}",
                                            hex::encode(item.hash),
                                            e
                                        ),
                                    }
                                } else {
                                    let nf = serde_json::to_vec(&InvMsg {
                                        items: vec![item.clone()],
                                    })
                                    .unwrap_or_default();
                                    self.peer_manager
                                        .send_to(peer_addr, NetMessage::new("notfound", nf))
                                        .await;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            // ── Header sync (getheaders / headers) ────────────────────────────
            "getheaders" => {
                if let Ok(req) = serde_json::from_slice::<GetHeadersMsg>(&msg.payload) {
                    let chain = self.chain.lock().await;
                    let our_height = chain.best_height();

                    // Find the highest block in the locator that we know
                    let start_height = req
                        .block_locator_hashes
                        .iter()
                        .find_map(|hash| {
                            for h in (0..=our_height).rev() {
                                if let Some(b) = chain.get_block_at_height(h) {
                                    if b.hash() == *hash {
                                        return Some(h + 1);
                                    }
                                }
                            }
                            None
                        })
                        .unwrap_or(1);

                    let mut headers: Vec<HeaderEntry> = Vec::new();
                    for h in start_height..=our_height.min(start_height + HEADERS_PER_BATCH as u32)
                    {
                        if let Some(block) = chain.get_block_at_height(h) {
                            let hash = block.hash();
                            if req.hash_stop != [0u8; 32] && hash == req.hash_stop {
                                // Serialize header as bytes (bincode matches how hash() works)
                                let header_bytes =
                                    bincode::serialize(&block.header).unwrap_or_default();
                                headers.push(HeaderEntry {
                                    header: header_bytes,
                                    tx_count: 0,
                                });
                                break;
                            }
                            let header_bytes =
                                bincode::serialize(&block.header).unwrap_or_default();
                            headers.push(HeaderEntry {
                                header: header_bytes,
                                tx_count: 0,
                            });
                        }
                    }

                    if !headers.is_empty() {
                        let payload =
                            serde_json::to_vec(&HeadersMsg { headers }).unwrap_or_default();
                        drop(chain);
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("headers", payload))
                            .await;
                    }
                }
            }

            "headers" => {
                if let Ok(resp) = serde_json::from_slice::<HeadersMsg>(&msg.payload) {
                    let count = resp.headers.len();
                    if count == 0 {
                        return Ok(());
                    }
                    tracing::debug!("headers: received {} headers from {}", count, peer_addr);

                    // Deserialize each HeaderEntry's bytes into a BlockHeader to get the hash
                    let decoded: Vec<BlockHeader> = resp
                        .headers
                        .iter()
                        .filter_map(|h| bincode::deserialize::<BlockHeader>(&h.header).ok())
                        .collect();

                    // Request the blocks we don't have yet
                    let want: Vec<InvItem> = {
                        let chain = self.chain.lock().await;
                        decoded
                            .iter()
                            .map(|hdr| hdr.hash())
                            .filter(|hash| chain.get_block(hash).is_none())
                            .map(|hash| InvItem {
                                inv_type: InvType::Block,
                                hash,
                            })
                            .collect()
                    };

                    if !want.is_empty() {
                        let payload =
                            serde_json::to_vec(&vtorrent_p2p::message::GetDataMsg { items: want })
                                .unwrap_or_default();
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("getdata", payload))
                            .await;
                    }

                    // If we got a full batch, there may be more — send another getheaders
                    if count == HEADERS_PER_BATCH {
                        let last_hash = decoded.last().map(|hdr| hdr.hash()).unwrap_or([0u8; 32]);
                        let payload = serde_json::to_vec(&GetHeadersMsg {
                            version: 70001,
                            block_locator_hashes: vec![last_hash],
                            hash_stop: [0u8; 32],
                        })
                        .unwrap_or_default();
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("getheaders", payload))
                            .await;
                    }
                }
            }

            // ── DEX order gossip ─────────────────────────────────────────────
            "dexorder" => {
                if let Ok(ann) = serde_json::from_slice::<OrderAnnouncement>(&msg.payload) {
                    let order_id = ann.order_id;
                    if self.seen_orders.insert(order_id) {
                        if let Some(book) = &self.order_book {
                            book.write().await.add_order(ann.to_order());
                        }
                        // Re-broadcast to all peers except the sender.
                        let payload = serde_json::to_vec(&ann).unwrap_or_default();
                        for peer in self.peer_manager.connected_peers() {
                            if peer != peer_addr {
                                self.peer_manager
                                    .send_to(peer, NetMessage::new("dexorder", payload.clone()))
                                    .await;
                            }
                        }
                        tracing::debug!("DEX gossip: received order {}", hex::encode(order_id));
                    }
                } else {
                    self.peer_manager
                        .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage)
                        .await;
                }
            }

            cmd => {
                tracing::trace!("Unhandled message '{}' from {}", cmd, peer_addr);
                self.peer_manager
                    .record_misbehaviour(peer_addr, Misbehaviour::UnknownMessage)
                    .await;
            }
        }
        Ok(())
    }

    /// Attempt to produce a new PoS block.
    async fn attempt_stake(&mut self) -> Result<()> {
        let staking = self
            .staking
            .as_ref()
            .ok_or_else(|| NodeError::Chain("Staking not enabled".into()))?;

        let (best_height, best_hash, best_timestamp, best_stake_modifier, stake_utxos) = {
            let chain = self.chain.lock().await;
            let best_height = chain.best_height();
            let best_hash = chain.best_hash().unwrap_or([0u8; 32]);
            let best_block = chain.get_block_at_height(best_height);
            let best_timestamp = best_block.map(|b| b.header.timestamp).unwrap_or(0);
            let best_stake_modifier = best_block.map(|b| b.header.stake_modifier).unwrap_or(0);
            let utxos = chain.get_utxos_for_address(&staking.address);
            (
                best_height,
                best_hash,
                best_timestamp,
                best_stake_modifier,
                utxos,
            )
        };

        if stake_utxos.is_empty() {
            return Err(NodeError::Chain("No UTXOs available for staking".into()));
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        if now <= best_timestamp + TARGET_BLOCK_TIME as u32 {
            return Err(NodeError::Chain("Too soon to stake".into()));
        }

        // Only include pending txs whose inputs are still unspent in the
        // current UTXO set — mempool entries can go stale when a competing
        // block confirms the same inputs, and including them would make our
        // block invalid.
        let pending_txs = {
            let chain = self.chain.lock().await;
            let mempool = self.mempool.lock().await;
            mempool
                .get_transactions()
                .into_iter()
                .filter(|tx| chain.compute_tx_fee(tx).is_some())
                .collect()
        };

        if let Some(block) = staking.build_stake_block(
            best_hash,
            best_stake_modifier,
            best_height + 1,
            now,
            stake_utxos,
            pending_txs,
        ) {
            let block_hash = block.hash();
            let tx_count = block.transactions.len();
            let timestamp = block.header.timestamp;
            let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
            let block_arc = std::sync::Arc::new(block);
            let acceptance = {
                let mut chain = self.chain.lock().await;
                let result = chain.add_block((*block_arc).clone())?;
                tracing::info!(
                    "Staked new block {} at height {}",
                    hex::encode(block_hash),
                    chain.best_height()
                );
                result
            };

            // Emit NewBlock event (carries UTXO diff for BlockStore persistence)
            use crate::chain::BlockAcceptance;
            if let BlockAcceptance::MainChain {
                height,
                utxos_added,
                utxos_removed,
                claimed_addresses,
            } = acceptance
            {
                {
                    let confirmed: Vec<[u8; 32]> =
                        block_arc.transactions.iter().map(|tx| tx.txid()).collect();
                    let mut mp = self.mempool.lock().await;
                    mp.handle_confirmed_block(&confirmed, &utxos_removed);
                }
                self.emit(NodeEvent::NewBlock {
                    height,
                    hash: block_hash,
                    tx_count,
                    timestamp,
                    size_bytes,
                    block: block_arc.clone(),
                    utxos_added,
                    utxos_removed,
                    claimed_addresses,
                });
                for tx in block_arc.transactions.iter() {
                    self.emit(NodeEvent::TxConfirmed {
                        txid: tx.txid(),
                        block_height: height,
                        block_hash,
                    });
                }
                // Emit StakingReward event
                let reward_sats: u64 = block_arc
                    .transactions
                    .iter()
                    .filter(|tx| matches!(tx.tx_type, crate::block::TxType::Coinstake))
                    .flat_map(|tx| tx.outputs.iter())
                    .map(|o| o.value)
                    .sum();
                let staking_addr = staking.address.clone();
                self.emit(NodeEvent::StakingReward {
                    block_height: height,
                    reward_sats,
                    address: staking_addr,
                });
            }

            // Announce to peers
            let payload = serde_json::to_vec(&InvMsg {
                items: vec![InvItem {
                    inv_type: InvType::Block,
                    hash: block_hash,
                }],
            })
            .unwrap_or_default();
            self.peer_manager
                .broadcast(NetMessage::new("inv", payload))
                .await;
        }

        Ok(())
    }

    /// Request new blocks from peers if we are behind.
    ///
    /// Uses `getheaders` (up to 2000 headers per round) which is significantly
    /// faster than the legacy `getblocks` + `inv` approach during IBD.
    async fn request_blocks_from_peers(&mut self) {
        let our_height = {
            let chain = self.chain.lock().await;
            chain.best_height()
        };
        let network_height = self.peer_manager.network_best_height();

        if network_height > our_height {
            tracing::info!(
                "Syncing: our height {} < network height {}",
                our_height,
                network_height
            );

            // Build a block locator (exponentially stepped hashes from tip)
            let locator = {
                let chain = self.chain.lock().await;
                let tip = chain.best_height();
                let mut hashes = Vec::new();
                let mut step = 1u32;
                let mut h = tip;
                loop {
                    if let Some(block) = chain.get_block_at_height(h) {
                        hashes.push(block.hash());
                    }
                    if hashes.len() >= 10 {
                        step *= 2;
                    }
                    if h < step {
                        // Always include genesis
                        if let Some(genesis) = chain.get_block_at_height(0) {
                            if hashes.last().copied() != Some(genesis.hash()) {
                                hashes.push(genesis.hash());
                            }
                        }
                        break;
                    }
                    h -= step;
                }
                hashes
            };

            let payload = serde_json::to_vec(&GetHeadersMsg {
                version: 70001,
                block_locator_hashes: locator,
                hash_stop: [0u8; 32],
            })
            .unwrap_or_default();
            self.peer_manager
                .broadcast(NetMessage::new("getheaders", payload))
                .await;
        }
    }

    /// Maintain peer connections — reconnect if below target using PEX address book.
    async fn maintain_peers(&mut self) {
        let count = self.peer_manager.peer_count();
        if count < TARGET_OUTBOUND {
            let needed = TARGET_OUTBOUND - count;
            tracing::debug!(
                "Low peer count ({}/{}), trying {} PEX candidates",
                count,
                TARGET_OUTBOUND,
                needed
            );

            // First try PEX address book candidates
            let candidates = self.peer_manager.get_peer_candidates(needed * 2);
            let mut connected = 0;
            for addr in candidates {
                if connected >= needed {
                    break;
                }
                if let Err(e) = self.peer_manager.connect_addr(addr).await {
                    tracing::debug!("Could not connect to PEX candidate {}: {}", addr, e);
                } else {
                    connected += 1;
                }
            }

            // If still not enough, re-dial explicitly configured seeds
            if connected < needed && !self.config.extra_seeds.is_empty() {
                tracing::debug!(
                    "PEX insufficient, re-dialing {} explicit seeds",
                    self.config.extra_seeds.len()
                );
                for seed in self.config.extra_seeds.clone() {
                    if connected >= needed {
                        break;
                    }
                    if let Err(e) = self.peer_manager.connect(&seed).await {
                        tracing::debug!("Could not connect to seed {}: {}", seed, e);
                    } else {
                        connected += 1;
                    }
                }
            }

            // If still not enough, try DHT again
            if connected < needed && self.config.use_dht && !self.config.isolated {
                tracing::debug!("PEX insufficient, re-running DHT bootstrap");
                self.bootstrap_via_dht().await;
            }
        }
    }

    /// Send a ping to every connected peer to confirm liveness.
    ///
    /// Peers that have an outstanding unanswered ping from the *previous* cycle
    /// are disconnected (they failed to respond within 2 minutes).
    async fn send_keepalive_pings(&mut self) {
        let peers = self.peer_manager.connected_peers();
        let mut stale: Vec<std::net::SocketAddr> = Vec::new();

        for addr in &peers {
            if self.peer_ping_nonces.contains_key(addr) {
                // Peer did not respond to last ping — disconnect it
                tracing::warn!("Peer {} timed out (no pong), disconnecting", addr);
                stale.push(*addr);
            } else {
                // Send a fresh ping
                let nonce: u64 = rand::random();
                self.peer_ping_nonces.insert(*addr, nonce);
                let payload = serde_json::to_vec(&PingMsg { nonce }).unwrap_or_default();
                self.peer_manager
                    .send_to(*addr, NetMessage::new("ping", payload))
                    .await;
            }
        }

        for addr in stale {
            self.peer_manager.disconnect(addr).await;
            self.peer_fee_filters.remove(&addr);
            self.peer_ping_nonces.remove(&addr);
            self.compact_peers.remove(&addr);
        }
    }

    /// PEX maintenance: send getaddr to peers and self-announce.
    async fn do_pex_maintenance(&mut self) {
        let services = NODE_NETWORK | NODE_TORRENT;

        // Broadcast our own address to all peers
        if self.peer_manager.should_self_announce() {
            if let Some(announce_msg) = self.peer_manager.build_self_announce(services) {
                self.peer_manager.broadcast(announce_msg).await;
                self.peer_manager.record_self_announce();
                tracing::debug!("PEX: Broadcasted self-announcement");
            }
        }

        // Send getaddr to all connected peers
        if self.peer_manager.should_getaddr() {
            let getaddr = PeerManager::build_getaddr();
            self.peer_manager.broadcast(getaddr).await;
            self.peer_manager.record_getaddr();
            tracing::debug!("PEX: Sent getaddr to all peers");
        }
    }

    /// Deserialize a block from raw bytes.
    /// Currently uses JSON for simplicity; will be replaced with binary encoding.
    fn deserialize_block(&self, bytes: &[u8]) -> Result<Block> {
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("Block deserialization failed: {}", e)))
    }

    /// Deserialize a transaction from raw bytes.
    fn deserialize_tx(&self, bytes: &[u8]) -> Result<Transaction> {
        serde_json::from_slice(bytes)
            .map_err(|e| NodeError::Chain(format!("TX deserialization failed: {}", e)))
    }

    /// Start the overlay NAT traversal layer.
    ///
    /// Binds a UDP socket, discovers our external address via STUN, and begins
    /// accepting hole-punch requests from other nodes. Authenticated overlay
    /// payloads are decoded and routed through the same P2P message handler as
    /// TCP-delivered traffic.
    async fn start_overlay(&mut self) {
        let key_path = self.config.data_dir.join("overlay.key");
        let overlay_config = OverlayConfig {
            listen_port: 0, // OS-assigned port; avoids conflicts with P2P port
            key_file: Some(key_path),
            ..OverlayConfig::default()
        };

        match Overlay::start(overlay_config).await {
            Ok((overlay, mut events)) => {
                if let Some(ext) = overlay.external_addr {
                    tracing::info!("Overlay ready — external UDP address: {}", ext);
                    tracing::info!("Overlay node ID: {}", &overlay.keypair.node_id()[..16]);
                }

                // Publish our overlay endpoint to the PEX address book so peers
                // can learn it via addr messages.
                if let Some(ep) = overlay.our_endpoint() {
                    tracing::debug!("Overlay endpoint: {}", ep);
                }

                self.overlay = Some(overlay);

                let overlay_ingress_tx = self.overlay_tx.clone();

                // Convert authenticated overlay events into virtual P2P peer events.
                // The node event loop owns protocol state and handles them alongside
                // TCP peers, preserving a single validation and dispatch path.
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        let ingress = match event {
                            OverlayEvent::PeerConnected(endpoint) => {
                                tracing::info!(peer = %endpoint.node_id, "Overlay: peer connected");
                                OverlayIngress::PeerConnected {
                                    node_id: endpoint.node_id,
                                }
                            }
                            OverlayEvent::PeerDisconnected(node_id) => {
                                tracing::info!(peer = %node_id, "Overlay: peer disconnected");
                                OverlayIngress::PeerDisconnected { node_id }
                            }
                            OverlayEvent::ExternalAddrDiscovered(addr) => {
                                tracing::info!("Overlay: external address confirmed {}", addr);
                                continue;
                            }
                            OverlayEvent::DataReceived {
                                from_node_id,
                                payload,
                            } => match decode_overlay_message(&payload) {
                                Ok(msg) => OverlayIngress::Message {
                                    node_id: from_node_id,
                                    msg,
                                },
                                Err(e) => {
                                    tracing::debug!(
                                        "Discarding malformed overlay P2P envelope: {}",
                                        e
                                    );
                                    continue;
                                }
                            },
                        };
                        if overlay_ingress_tx.send(ingress).await.is_err() {
                            tracing::debug!("Overlay ingress channel closed");
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!("Overlay failed to start (non-fatal): {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_message_roundtrip_preserves_command_and_payload() {
        let message = NetMessage::new("ping", vec![1, 2, 3, 4]);
        let encoded = encode_overlay_message(&message);
        let decoded = decode_overlay_message(&encoded).expect("valid overlay envelope");
        assert_eq!(decoded.command_str(), "ping");
        assert_eq!(decoded.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn overlay_message_rejects_invalid_envelopes() {
        assert!(decode_overlay_message(&[0; 15]).is_err());

        let mut malformed = encode_overlay_message(&NetMessage::new("ping", vec![1]));
        malformed[12..16].copy_from_slice(&(2u32).to_le_bytes());
        assert!(decode_overlay_message(&malformed).is_err());
    }

    #[test]
    fn overlay_peer_address_is_stable_and_private() {
        let node_id = "11".repeat(32);
        let first = overlay_peer_addr(&node_id).expect("valid node id");
        let second = overlay_peer_addr(&node_id).expect("valid node id");
        assert_eq!(first, second);
        assert_eq!(first.ip().to_string().split('.').next(), Some("198"));
    }

    fn test_node() -> Node {
        Node::new(NodeConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            use_overlay: false,
            use_dht: false,
            ..NodeConfig::default()
        })
        .expect("node init failed")
    }

    #[tokio::test]
    async fn invalid_tx_from_peer_records_misbehaviour() {
        use crate::block::{Transaction, TxType};
        use vtorrent_p2p::ban_manager::Misbehaviour;

        let mut node = test_node();
        let peer_addr: std::net::SocketAddr = "127.0.0.1:12345".parse().unwrap();

        // An empty transaction fails consensus validation.
        let bad_tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let msg = NetMessage::new("tx", serde_json::to_vec(&bad_tx).unwrap());
        node.handle_message(peer_addr, msg).await.unwrap();

        assert_eq!(
            node.peer_manager
                .ban_manager
                .read()
                .await
                .score(peer_addr.ip()),
            Misbehaviour::InvalidTransaction.score()
        );
    }

    #[tokio::test]
    async fn malformed_message_records_misbehaviour() {
        use vtorrent_p2p::ban_manager::Misbehaviour;

        let mut node = test_node();
        let peer_addr: std::net::SocketAddr = "127.0.0.1:12346".parse().unwrap();

        // A "tx" message whose payload is not a valid transaction.
        let msg = NetMessage::new("tx", b"not-a-transaction".to_vec());
        node.handle_message(peer_addr, msg).await.unwrap();

        assert_eq!(
            node.peer_manager
                .ban_manager
                .read()
                .await
                .score(peer_addr.ip()),
            Misbehaviour::MalformedMessage.score()
        );
    }

    #[tokio::test]
    async fn message_flood_triggers_ban() {
        let mut node = test_node();
        let peer_addr: std::net::SocketAddr = "127.0.0.1:12347".parse().unwrap();

        // A legitimate, penalty-free message ("getaddr") sent faster than the
        // per-peer rate limit should trigger a ban.
        for _ in 0..=MAX_MSGS_PER_WINDOW {
            node.handle_message(peer_addr, NetMessage::new("getaddr", vec![]))
                .await
                .unwrap();
        }

        assert!(
            node.peer_manager.is_banned(peer_addr).await,
            "flooding peer should be banned"
        );
    }

    #[tokio::test]
    async fn test_dexorder_gossip_adds_to_book() {
        use crate::atomic_swap::{OrderAnnouncement, SwapOrder, SwapOrderBook};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mut node = test_node();
        let book = Arc::new(RwLock::new(SwapOrderBook::new()));
        node.set_order_book(book.clone());

        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            48 * 3600,
        );
        let ann = OrderAnnouncement::from_order(&order);
        let payload = serde_json::to_vec(&ann).unwrap();
        let peer_addr: std::net::SocketAddr = "127.0.0.1:12348".parse().unwrap();

        node.handle_message(peer_addr, NetMessage::new("dexorder", payload))
            .await
            .unwrap();

        assert_eq!(book.read().await.open_order_count(), 1);
    }
}
