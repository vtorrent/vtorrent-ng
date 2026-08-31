// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! vTorrent Node — main event loop.

/// vTorrent Node — main event loop.
///
/// Wires together:
/// - Chain state (vtorrent-node)
/// - P2P peer manager (vtorrent-p2p)
/// - PoS staking engine (vtorrent-node::staking)
/// - Mempool (vtorrent-node)
pub mod bootstrap;
pub mod chain;
pub(crate) mod handler;
pub mod overlay;
pub mod p2p;
pub(crate) mod peering;
pub(crate) mod staking_loop;

pub use chain::handle_block;
pub use p2p::handle_peer_event;

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{interval, Duration};

use std::path::PathBuf;

use vtorrent_core::time::now_secs;
use vtorrent_onion::TransportConfig;
use vtorrent_overlay::{Overlay, OverlayConfig, OverlayEvent};

use vtorrent_p2p::{
    compact::CompactBlockPeerState,
    message::{encode_v2, is_v2_peer, CmpctBlockMsg, InvItem, InvMsg, InvType, NetMessage},
    peer_manager::{PeerManager, DEFAULT_PORT, TARGET_OUTBOUND},
};

use crate::{
    atomic_swap::{OrderAnnouncement, SwapOrderBook},
    block::{Block, Transaction},
    chain::{Chain, Utxo},
    consensus::TARGET_BLOCK_TIME,
    error::{NodeError, Result},
    events::{EventSender, NodeEvent},
    mempool::Mempool,
    staking::{StakingCommand, StakingEngine},
};

/// Authenticated overlay notifications queued for the node event loop.
pub(crate) enum OverlayIngress {
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
    /// This node's public `ip:port`. Learned addresses matching it are
    /// filtered so the node never dials or gossips itself.
    pub public_addr: Option<std::net::SocketAddr>,
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
    /// When `true` and `regtest` is also true, lower min/max stake age
    /// for rapid regtest soak testing (60s min, 1h max).
    pub regtest_fast_stake: bool,
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
            public_addr: None,
            data_dir: default_data_dir(),
            use_overlay: true,
            testnet: false,
            regtest: false,
            regtest_fast_stake: false,
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
    pub(crate) chain: Arc<Mutex<Chain>>,
    pub(crate) mempool: Arc<Mutex<Mempool>>,
    pub(crate) peer_manager: PeerManager,
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
    pub(crate) compact_peers:
        std::collections::HashMap<std::net::SocketAddr, CompactBlockPeerState>,
    /// Partial compact blocks awaiting `blocktxn` responses (BIP-152).
    /// Keyed by block hash; populated when `cmpctblock` reports missing txs.
    pub(crate) pending_compact_blocks: HashMap<[u8; 32], CmpctBlockMsg>,
    /// Per-peer minimum fee rate (from feefilter messages), satoshis per 1000 bytes.
    pub(crate) peer_fee_filters: std::collections::HashMap<std::net::SocketAddr, u64>,
    /// Per-peer last-seen ping nonce (for pong matching).
    pub(crate) peer_ping_nonces: std::collections::HashMap<std::net::SocketAddr, u64>,
    /// Per-peer message counts for flood rate limiting: (count, window start).
    pub(crate) peer_msg_counts: std::collections::HashMap<std::net::SocketAddr, (u64, u64)>,
    /// Per-peer advertised protocol version (for V2 bincode sniffing).
    /// V2 peers (`PROTOCOL_VERSION = 2`, bincode) vs legacy (`70001`, JSON).
    /// Unknown commands are ignored to allow rolling upgrades.
    pub(crate) peer_versions: std::collections::HashMap<std::net::SocketAddr, u32>,
    /// Shared DEX order book (set by the daemon; used for gossip).
    pub(crate) order_book: Option<Arc<RwLock<SwapOrderBook>>>,
    /// Order IDs already seen via gossip, for deduplication.
    pub(crate) seen_orders: HashSet<[u8; 32]>,
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

use overlay::*;

impl Node {
    /// Create a new node.
    pub fn new(config: NodeConfig) -> Result<Self> {
        let chain = Chain::new()?;
        let best_height = chain.best_height();
        let mempool = Mempool::new(config.max_mempool);
        let mut peer_manager = PeerManager::with_transport_config(
            best_height,
            &config.listen_addr,
            config.testnet,
            config.transport.clone(),
        );
        if let Some(public) = config.public_addr {
            peer_manager.set_public_addr(public);
        }
        let (tx_submit_tx, tx_submit_rx) = mpsc::channel(256);
        let (overlay_tx, overlay_rx) = mpsc::channel(256);

        let staking = if config.staking_enabled {
            config.staking_address.as_ref().map(|addr| {
                let fast = config.regtest && config.regtest_fast_stake;
                match &config.staking_wif {
                    Some(wif) if fast => StakingEngine::with_wif_fast(addr.clone(), wif.clone()),
                    Some(wif) => StakingEngine::with_wif(addr.clone(), wif.clone()),
                    None if fast => StakingEngine::new_fast(addr.clone()),
                    None => StakingEngine::new(addr.clone()),
                }
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
            peer_versions: std::collections::HashMap::new(),
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
        let mut peer_manager = PeerManager::with_transport_config(
            best_height,
            &config.listen_addr,
            config.testnet,
            config.transport.clone(),
        );
        if let Some(public) = config.public_addr {
            peer_manager.set_public_addr(public);
        }
        let (tx_submit_tx, tx_submit_rx) = mpsc::channel(256);
        let (overlay_tx, overlay_rx) = mpsc::channel(256);

        let staking = if config.staking_enabled {
            config.staking_address.as_ref().map(|addr| {
                let fast = config.regtest && config.regtest_fast_stake;
                match &config.staking_wif {
                    Some(wif) if fast => StakingEngine::with_wif_fast(addr.clone(), wif.clone()),
                    Some(wif) => StakingEngine::with_wif(addr.clone(), wif.clone()),
                    None if fast => StakingEngine::new_fast(addr.clone()),
                    None => StakingEngine::new(addr.clone()),
                }
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
            peer_versions: std::collections::HashMap::new(),
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
        let payload = if self.peer_versions.values().any(|v| is_v2_peer(*v)) {
            encode_v2(&ann).unwrap_or_else(|e| {
                tracing::warn!("Failed to bincode order announcement: {}", e);
                Vec::new()
            })
        } else {
            match serde_json::to_vec(&ann) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Failed to serialize order announcement: {}", e);
                    return;
                }
            }
        };
        // Empty payload means serialization failed — skip broadcast
        if payload.is_empty() {
            return;
        }
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
    pub(crate) fn emit(&self, event: NodeEvent) {
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
                    // Prune expired bans and decay misbehaviour scores.
                    // Without this the ban manager's maps grow without bound
                    // (remote-triggerable memory exhaustion via IP rotation).
                    self.peer_manager.prune_bans().await;
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
                    let size_bytes = tx.serialized_size();
                    // Route through admit_with_chain_fee like the P2P path:
                    // add_transaction would trust tx.fee_sats() (which assumes
                    // every input is worth 100k sats, fabricating fees) and
                    // skip script verification entirely.
                    let admission_fee = {
                        let mut mp = self.mempool.lock().await;
                        if mp.get_transaction(&txid).is_some() {
                            None
                        } else {
                            let chain = self.chain.lock().await;
                            match mp.admit_with_chain_fee(&chain, tx) {
                                Ok(fee) => Some(fee),
                                Err(e) => {
                                    tracing::warn!(
                                        txid = %hex::encode(txid),
                                        "Local tx submission rejected: {}",
                                        e
                                    );
                                    None
                                }
                            }
                        }
                    };
                    if let Some(fee_sats) = admission_fee {
                        self.emit(NodeEvent::TxUnconfirmed { txid, fee_sats, size_bytes });
                        let inv_msg = InvMsg {
                            items: vec![InvItem {
                                inv_type: InvType::Transaction,
                                hash: txid,
                            }],
                        };
                        let payload = if self.peer_versions.values().any(|v| is_v2_peer(*v)) {
                            encode_v2(&inv_msg).unwrap_or_else(|e| {
                                tracing::warn!("Failed to bincode inv: {}", e);
                                Vec::new()
                            })
                        } else {
                            match serde_json::to_vec(&inv_msg) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!("Failed to serialize inv message: {}", e);
                                    Vec::new()
                                }
                            }
                        };
                        if !payload.is_empty() {
                            self.peer_manager.broadcast(
                                NetMessage::new("inv", payload)
                            ).await;
                            tracing::info!(
                                "Local tx {} announced to peers",
                                hex::encode(txid)
                            );
                        }
                    }
                }
                // Locally-minted blocks from the regtest faucet
                Some(block) = self.block_submit_rx.recv() => {
                    let block_hash = block.hash();
                    // Faucet blocks are minted directly into the chain via
                    // Chain::mint_to_address. Emit the NewBlock event so the
                    // daemon's event bridge persists them to the block store —
                    // without this, faucet blocks exist only in memory and the
                    // store replay fails on restart (height gap → truncation).
                    {
                        let chain = self.chain.lock().await;
                        let height = chain.block_height(&block_hash).unwrap_or(0);
                        let chain_ref = &*chain;
                        let utxos_added: Vec<crate::chain::Utxo> = block
                            .transactions
                            .iter()
                            .flat_map(|tx| {
                                let txid = tx.txid();
                                tx.outputs.iter().enumerate().filter_map(move |(vout, _)| {
                                    let vout = vout as u32;
                                    chain_ref.get_utxo(&txid, vout).map(|u| Utxo {
                                        txid,
                                        vout,
                                        value: u.value,
                                        script_pubkey: u.script_pubkey.clone(),
                                        height,
                                        timestamp: u.timestamp,
                                    })
                                })
                            })
                            .collect();
                        let utxos_removed: Vec<([u8; 32], u32)> = block
                            .transactions
                            .iter()
                            .flat_map(|tx| {
                                tx.inputs
                                    .iter()
                                    .map(|i| (i.prev_txid, i.prev_vout))
                            })
                            .collect();
                        let claimed = block
                            .transactions
                            .iter()
                            .filter_map(|tx| tx.claim_address.clone())
                            .collect();
                        self.emit(NodeEvent::NewBlock {
                            height,
                            hash: block_hash,
                            tx_count: block.transactions.len(),
                            timestamp: block.header.timestamp,
                            size_bytes: serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0),
                            block: std::sync::Arc::new(block.clone()),
                            utxos_added,
                            utxos_removed,
                            claimed_addresses: claimed,
                        });
                    }
                    let inv_msg = InvMsg {
                        items: vec![InvItem {
                            inv_type: InvType::Block,
                            hash: block_hash,
                        }],
                    };
                    let payload = if self.peer_versions.values().any(|v| is_v2_peer(*v)) {
                        encode_v2(&inv_msg).unwrap_or_else(|e| {
                            tracing::warn!("Failed to bincode block inv: {}", e);
                            Vec::new()
                        })
                    } else {
                        match serde_json::to_vec(&inv_msg) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("Failed to serialize block inv: {}", e);
                                continue;
                            }
                        }
                    };
                    if payload.is_empty() {
                        continue;
                    }
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
                            let fast = self.config.regtest && self.config.regtest_fast_stake;
                            self.staking = Some(match (wif, fast) {
                                (Some(w), true) => StakingEngine::with_wif_fast(address, w),
                                (Some(w), false) => StakingEngine::with_wif(address, w),
                                (None, true) => StakingEngine::new_fast(address),
                                (None, false) => StakingEngine::new(address),
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
        bootstrap::bootstrap_via_dht(self).await;
    }

    /// Announce ourselves on the DHT so other nodes can find us.
    async fn dht_announce(&self) {
        bootstrap::dht_announce(self).await;
    }

    /// Connect to explicitly configured extra seed nodes.
    async fn connect_to_extra_seeds(&mut self) {
        bootstrap::connect_to_extra_seeds(self).await;
    }

    /// Bootstrap from the GitHub-hosted peer list (Stage 3 fallback).
    async fn bootstrap_via_github(&mut self) {
        bootstrap::bootstrap_via_github(self).await;
    }

    /// Connect to legacy DNS seed nodes (fallback only).
    async fn connect_to_dns_seeds(&mut self) {
        bootstrap::connect_to_dns_seeds(self).await;
    }

    /// Handle a raw network message from a peer.
    ///
    /// Thin wrapper: rate limiting and command dispatch live in
    /// `handler::dispatch_message` so the routing table is testable
    /// independently of the connection loop.
    async fn handle_message(
        &mut self,
        peer_addr: std::net::SocketAddr,
        msg: NetMessage,
    ) -> Result<()> {
        handler::dispatch_message(self, peer_addr, msg).await
    }

    /// Request new blocks from peers if we are behind.
    ///
    /// Uses `getheaders` (up to 2000 headers per round) which is significantly
    /// Send a ping to every connected peer to confirm liveness.
    ///
    /// Peers that have an outstanding unanswered ping from the *previous* cycle
    /// Deserialize a block from raw bytes.
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
