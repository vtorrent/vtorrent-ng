/// vTorrent Node — main event loop.
///
/// Wires together:
/// - Chain state (vtorrent-node)
/// - P2P peer manager (vtorrent-p2p)
/// - PoS staking engine (vtorrent-node::staking)
/// - Mempool (vtorrent-node)

use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration};

use std::path::PathBuf;

use vtorrent_overlay::{
    Overlay, OverlayConfig, OverlayEvent,
};

use vtorrent_p2p::{
    compact::{CompactBlockDecoder, CompactBlockEncoder, CompactBlockPeerState, derive_siphash_keys, short_txid},
    dht::{discover_peers_via_doh, discover_peers_via_github, DhtBootstrap},
    message::{
        AddrMsg, BlockTxnMsg, CmpctBlockMsg, GetBlocksMsg, GetBlockTxnMsg,
        InvItem, InvMsg, InvType, NetMessage, SendCmpctMsg,
        NODE_NETWORK, NODE_TORRENT,
    },
    peer::PeerEvent,
    peer_manager::{PeerManager, DEFAULT_PORT, TARGET_OUTBOUND},
};

use crate::{
    block::{Block, BlockHeader, Transaction},
    chain::Chain,
    consensus::TARGET_BLOCK_TIME,
    error::{NodeError, Result},
    events::{EventSender, NodeEvent},
    mempool::Mempool,
    staking::StakingEngine,
};

/// Node configuration.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// P2P listen address.
    pub listen_addr: String,
    /// Whether to enable staking.
    pub staking_enabled: bool,
    /// The staking address (must have UTXOs).
    pub staking_address: Option<String>,
    /// Maximum mempool size.
    pub max_mempool: usize,
    /// Additional seed nodes to connect to.
    pub extra_seeds: Vec<String>,
    /// Whether to use DHT bootstrap for peer discovery.
    pub use_dht: bool,
    /// Node data directory (for peer cache, chain data, etc.).
    /// Defaults to `~/.vtorrent` on all platforms.
    pub data_dir: PathBuf,
    /// Whether to start the overlay NAT traversal layer.
    pub use_overlay: bool,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen_addr: format!("0.0.0.0:{}", DEFAULT_PORT),
            staking_enabled: false,
            staking_address: None,
            max_mempool: 10_000,
            extra_seeds: Vec::new(),
            use_dht: true,
            data_dir: default_data_dir(),
            use_overlay: true,
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
    /// Optional event sender — when set, the node emits live events to subscribers.
    event_tx: Option<EventSender>,
    /// Per-peer compact block relay state (BIP-152).
    compact_peers: std::collections::HashMap<std::net::SocketAddr, CompactBlockPeerState>,
}

impl Node {
    /// Create a new node.
    pub fn new(config: NodeConfig) -> Result<Self> {
        let chain = Chain::new()?;
        let best_height = chain.best_height();
        let mempool = Mempool::new(config.max_mempool);
        let peer_manager = PeerManager::new(best_height, &config.listen_addr);

        let staking = if config.staking_enabled {
            config.staking_address.as_ref().map(|addr| {
                StakingEngine::new(addr.clone())
            })
        } else {
            None
        };

        Ok(Self {
            chain: Arc::new(Mutex::new(chain)),
            mempool: Arc::new(Mutex::new(mempool)),
            peer_manager,
            staking,
            config,
            overlay: None,
            event_tx: None,
            compact_peers: std::collections::HashMap::new(),
        })
    }

    /// Create a new node with a pre-loaded chain (e.g. loaded from BlockStore on disk).
    ///
    /// This is used by `vtorrent-daemon` when resuming from a persisted chain state
    /// rather than starting from genesis.
    pub fn new_with_chain(config: NodeConfig, chain: Chain) -> Result<Self> {
        let best_height = chain.best_height();
        let mempool = Mempool::new(config.max_mempool);
        let peer_manager = PeerManager::new(best_height, &config.listen_addr);

        let staking = if config.staking_enabled {
            config.staking_address.as_ref().map(|addr| {
                StakingEngine::new(addr.clone())
            })
        } else {
            None
        };

        Ok(Self {
            chain: Arc::new(Mutex::new(chain)),
            mempool: Arc::new(Mutex::new(mempool)),
            peer_manager,
            staking,
            config,
            overlay: None,
            event_tx: None,
            compact_peers: std::collections::HashMap::new(),
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
            tracing::warn!("Could not create data dir {:?}: {}", self.config.data_dir, e);
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
        self.peer_manager.start().await
            .map_err(|e| NodeError::Chain(format!("P2P start failed: {}", e)))?;

        // ── Stage 0.5: Start the overlay NAT traversal layer ─────────────────
        if self.config.use_overlay {
            self.start_overlay().await;
        }

        // ── Stage 1: DHT + Cloudflare DoH in parallel (decentralized) ────────
        if self.config.use_dht {
            self.bootstrap_via_dht().await;
        }

        // ── Stage 2: Explicitly configured extra seeds ────────────────────────
        if !self.config.extra_seeds.is_empty() {
            self.connect_to_extra_seeds().await;
        }

        // ── Stage 3: GitHub-hosted peer list (if still no peers) ─────────────
        if self.peer_manager.peer_count() == 0 {
            tracing::info!("No peers yet — trying GitHub bootstrap peer list...");
            self.bootstrap_via_github().await;
        }

        // ── Stage 4: Legacy DNS seeds (absolute last resort) ──────────────────
        if self.peer_manager.peer_count() == 0 {
            tracing::warn!("No peers found via any decentralized source, trying legacy DNS seeds");
            self.connect_to_dns_seeds().await;
        }

        // Periodic timers
        let mut sync_ticker = interval(Duration::from_secs(30));
        let mut stake_ticker = interval(Duration::from_secs(TARGET_BLOCK_TIME as u64));
        let mut peer_ticker = interval(Duration::from_secs(60));
        let mut pex_ticker = interval(Duration::from_secs(600));   // PEX getaddr every 10 min
        let mut dht_ticker = interval(Duration::from_secs(1800));  // DHT re-announce every 30 min

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

                // PEX: periodically send getaddr and self-announce
                _ = pex_ticker.tick() => {
                    self.do_pex_maintenance().await;
                }

                // DHT: periodically re-announce ourselves
                _ = dht_ticker.tick() => {
                    if self.config.use_dht {
                        self.dht_announce().await;
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

        let port = self.config.listen_addr
            .split(':')
            .last()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        // Spawn both bootstrap methods concurrently
        let dht_task = tokio::task::spawn_blocking(move || {
            let dht = DhtBootstrap::new();
            dht.discover_peers()
        });

        let doh_task = tokio::task::spawn_blocking(move || {
            discover_peers_via_doh(port)
        });

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
        let port = self.config.listen_addr
            .split(':')
            .last()
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
        let port = self.config.listen_addr
            .split(':')
            .last()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);

        let peers = tokio::task::spawn_blocking(move || {
            discover_peers_via_github(port)
        }).await.unwrap_or_default();

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
        let seeds: Vec<String> = DNS_SEEDS.iter()
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
                    peer_addr, version.user_agent, version.start_height
                );
                self.emit(NodeEvent::PeerConnected {
                    addr: peer_addr,
                    user_agent: version.user_agent.clone(),
                    version: version.version,
                    height: version.start_height,
                });
                // Negotiate compact block relay (BIP-152)
                // We use low-bandwidth mode (0) by default; high-bandwidth (1) is for the 3 fastest peers
                let sendcmpct_payload = serde_json::to_vec(&SendCmpctMsg {
                    high_bandwidth: false,
                    version: 1,
                }).unwrap_or_default();
                self.peer_manager.send_to(
                    peer_addr,
                    NetMessage::new("sendcmpct", sendcmpct_payload)
                ).await;
                // Track compact block state for this peer
                self.compact_peers.insert(peer_addr, CompactBlockPeerState::default());
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
                    }).unwrap_or_default();
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
                // Clean up compact block state for this peer
                self.compact_peers.remove(&peer_addr);
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
        match msg.command_str() {
            // ── PEX: Peer Exchange ────────────────────────────────────────────
            "addr" => {
                if let Ok(addr_msg) = serde_json::from_slice::<AddrMsg>(&msg.payload) {
                    let count = addr_msg.addrs.len();
                    self.peer_manager.handle_addr_msg(&addr_msg);
                    tracing::debug!("PEX: Received {} addresses from {}", count, peer_addr);
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
                        let payload = serde_json::to_vec(&vtorrent_p2p::message::GetDataMsg {
                            items: want,
                        }).unwrap_or_default();
                        self.peer_manager.broadcast(NetMessage::new("getdata", payload)).await;
                    }
                }
            }

            "block" => {
                // Block payload is raw bytes — deserialize and add to chain
                match self.deserialize_block(&msg.payload) {
                    Ok(block) => {
                        let mut chain = self.chain.lock().await;
                        match chain.add_block(block.clone()) {
                            Ok(acceptance) => {
                                use crate::chain::BlockAcceptance;
                                let hash = block.hash();
                                let should_relay = match &acceptance {
                                    BlockAcceptance::MainChain { height, utxos_added, utxos_removed, claimed_addresses } => {
                                        tracing::info!("Accepted block {} at height {}", hex::encode(hash), height);
                                        // Emit new_block event (carries full block + UTXO diff for BlockStore persistence)
                                        let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                                        self.emit(NodeEvent::NewBlock {
                                            height: *height,
                                            hash,
                                            tx_count: block.transactions.len(),
                                            timestamp: block.header.timestamp,
                                            size_bytes,
                                            block: std::sync::Arc::new(block.clone()),
                                            utxos_added: utxos_added.clone(),
                                            utxos_removed: utxos_removed.clone(),
                                            claimed_addresses: claimed_addresses.clone(),
                                        });
                                        // Emit tx_confirmed for each transaction in the block
                                        for tx in &block.transactions {
                                            self.emit(NodeEvent::TxConfirmed {
                                                txid: tx.txid(),
                                                block_height: *height,
                                                block_hash: hash,
                                            });
                                        }
                                        true
                                    }
                                    BlockAcceptance::Reorg { old_tip, new_tip, depth } => {
                                        tracing::warn!("Reorg depth {}: {} -> {}", depth, hex::encode(old_tip), hex::encode(new_tip));
                                        self.emit(NodeEvent::Reorg {
                                            old_tip: *old_tip,
                                            new_tip: *new_tip,
                                            depth: *depth,
                                        });
                                        true
                                    }
                                    BlockAcceptance::Fork { fork_tip } => {
                                        tracing::debug!("Fork block {} stored", hex::encode(fork_tip));
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
                                    }).unwrap_or_default();
                                    drop(chain);
                                    self.peer_manager.broadcast(
                                        NetMessage::new("inv", payload)
                                    ).await;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Rejected block from {}: {}", peer_addr, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to deserialize block from {}: {}", peer_addr, e);
                    }
                }
            }

            "tx" => {
                match self.deserialize_tx(&msg.payload) {
                    Ok(tx) => {
                        let mut mp = self.mempool.lock().await;
                        match mp.add_transaction(tx.clone()) {
                            Ok(()) => {
                                let txid = tx.txid();
                                let fee_sats = tx.fee_sats();
                                tracing::debug!("Accepted tx {}", hex::encode(txid));
                                self.emit(NodeEvent::TxUnconfirmed {
                                    txid,
                                    fee_sats,
                                });
                                let payload = serde_json::to_vec(&InvMsg {
                                    items: vec![InvItem {
                                        inv_type: InvType::Transaction,
                                        hash: txid,
                                    }],
                                }).unwrap_or_default();
                                drop(mp);
                                self.peer_manager.broadcast(
                                    NetMessage::new("inv", payload)
                                ).await;
                            }
                            Err(e) => {
                                tracing::debug!("Rejected tx: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to deserialize tx from {}: {}", peer_addr, e);
                    }
                }
            }

            "getblocks" => {
                if let Ok(req) = serde_json::from_slice::<GetBlocksMsg>(&msg.payload) {
                    let chain = self.chain.lock().await;
                    let our_height = chain.best_height();

                    // Find the peer's best known block height
                    let start_height = req.block_locator_hashes.iter()
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
                        self.peer_manager.broadcast(NetMessage::new("inv", payload)).await;
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
                        peer_addr, msg_data.high_bandwidth, msg_data.version
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
                                        tracing::warn!("cmpctblock: failed to decode tx from {}: {}", peer_addr, e);
                                        all_ok = false;
                                        break;
                                    }
                                }
                            }
                            if all_ok {
                                let block = Block {
                                    header: BlockHeader {
                                        version: cmpct.version,
                                        prev_block_hash: cmpct.prev_block_hash,
                                        merkle_root: cmpct.merkle_root,
                                        timestamp: cmpct.timestamp,
                                        bits: cmpct.bits,
                                        nonce: cmpct.nonce,
                                        stake_modifier: 0, // compact blocks don't carry stake_modifier; resolved from chain
                                    },
                                    transactions: txs,
                                };
                                let mut chain = self.chain.lock().await;
                                match chain.add_block(block.clone()) {
                                    Ok(acceptance) => {
                                        use crate::chain::BlockAcceptance;
                                        let hash = block.hash();
                                        if let BlockAcceptance::MainChain { height, utxos_added, utxos_removed, claimed_addresses } = &acceptance {
                                            tracing::info!(
                                                "cmpctblock: accepted block {} at height {}",
                                                hex::encode(hash), height
                                            );
                                            let size_bytes = serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0);
                                            self.emit(NodeEvent::NewBlock {
                                                height: *height,
                                                hash,
                                                tx_count: block.transactions.len(),
                                                timestamp: block.header.timestamp,
                                                size_bytes,
                                                block: std::sync::Arc::new(block.clone()),
                                                utxos_added: utxos_added.clone(),
                                                utxos_removed: utxos_removed.clone(),
                                                claimed_addresses: claimed_addresses.clone(),
                                            });
                                            for tx in &block.transactions {
                                                self.emit(NodeEvent::TxConfirmed {
                                                    txid: tx.txid(),
                                                    block_height: *height,
                                                    block_hash: hash,
                                                });
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("cmpctblock: rejected block from {}: {}", peer_addr, e);
                                    }
                                }
                            }
                        }
                        Err(missing_indexes) => {
                            // Some transactions are missing from our mempool — request them
                            tracing::debug!(
                                "cmpctblock: {} missing txs from {}, sending getblocktxn",
                                missing_indexes.len(), peer_addr
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
                                use sha2::{Sha256, Digest};
                                let h1 = Sha256::digest(&header_bytes2);
                                let h2 = Sha256::digest(h1);
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&h2);
                                arr
                            };
                            let req = CompactBlockDecoder::build_getblocktxn(block_hash, missing_indexes);
                            let payload = serde_json::to_vec(&req).unwrap_or_default();
                            self.peer_manager.send_to(
                                peer_addr,
                                NetMessage::new("getblocktxn", payload)
                            ).await;
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
                                        if let Ok(bytes) = serde_json::to_vec(&block.transactions[idx]) {
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
                        self.peer_manager.send_to(
                            peer_addr,
                            NetMessage::new("blocktxn", payload)
                        ).await;
                    }
                }
            }

            "blocktxn" => {
                // blocktxn arrives after we sent getblocktxn for a compact block
                // For now, log it — a full implementation would need to store the
                // partial compact block state and complete it here.
                // This is handled by a pending-block cache (future improvement).
                if let Ok(resp) = serde_json::from_slice::<BlockTxnMsg>(&msg.payload) {
                    tracing::debug!(
                        "blocktxn: received {} txs for block {} from {}",
                        resp.transactions.len(),
                        hex::encode(resp.block_hash),
                        peer_addr
                    );
                }
            }

            cmd => {
                tracing::trace!("Unhandled message '{}' from {}", cmd, peer_addr);
            }
        }
        Ok(())
    }

    /// Attempt to produce a new PoS block.
    async fn attempt_stake(&mut self) -> Result<()> {
        let staking = self.staking.as_ref()
            .ok_or_else(|| NodeError::Chain("Staking not enabled".into()))?;

        let (best_height, best_hash, best_timestamp, stake_utxos) = {
            let chain = self.chain.lock().await;
            let best_height = chain.best_height();
            let best_hash = chain.best_hash().unwrap_or([0u8; 32]);
            let best_timestamp = chain.get_block_at_height(best_height)
                .map(|b| b.header.timestamp)
                .unwrap_or(0);
            let utxos = chain.get_utxos_for_address(&staking.address);
            (best_height, best_hash, best_timestamp, utxos)
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

        let pending_txs = {
            let mempool = self.mempool.lock().await;
            mempool.get_transactions()
        };

        if let Some(block) = staking.build_stake_block(
            best_hash,
            best_height + 1,
            now,
            stake_utxos,
            pending_txs,
        ) {
            let block_hash = block.hash();
            let block_arc = std::sync::Arc::new(block.clone());
            let acceptance = {
                let mut chain = self.chain.lock().await;
                let result = chain.add_block(block.clone()).map_err(|e| e)?;
                tracing::info!(
                    "Staked new block {} at height {}",
                    hex::encode(block_hash),
                    chain.best_height()
                );
                result
            };

            // Emit NewBlock event (carries UTXO diff for BlockStore persistence)
            use crate::chain::BlockAcceptance;
            if let BlockAcceptance::MainChain { height, utxos_added, utxos_removed, claimed_addresses } = &acceptance {
                let size_bytes = serde_json::to_vec(&*block_arc).map(|v| v.len()).unwrap_or(0);
                self.emit(NodeEvent::NewBlock {
                    height: *height,
                    hash: block_hash,
                    tx_count: block_arc.transactions.len(),
                    timestamp: block_arc.header.timestamp,
                    size_bytes,
                    block: block_arc.clone(),
                    utxos_added: utxos_added.clone(),
                    utxos_removed: utxos_removed.clone(),
                    claimed_addresses: claimed_addresses.clone(),
                });
                for tx in &block_arc.transactions {
                    self.emit(NodeEvent::TxConfirmed {
                        txid: tx.txid(),
                        block_height: *height,
                        block_hash,
                    });
                }
                // Emit StakingReward event
                let reward_sats: u64 = block_arc.transactions.iter()
                    .filter(|tx| matches!(tx.tx_type, crate::block::TxType::Coinstake))
                    .flat_map(|tx| tx.outputs.iter())
                    .map(|o| o.value)
                    .sum();
                let staking_addr = staking.address.clone();
                self.emit(NodeEvent::StakingReward {
                    block_height: *height,
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
            }).unwrap_or_default();
            self.peer_manager.broadcast(NetMessage::new("inv", payload)).await;
        }

        Ok(())
    }

    /// Request new blocks from peers if we are behind.
    async fn request_blocks_from_peers(&mut self) {
        let our_height = {
            let chain = self.chain.lock().await;
            chain.best_height()
        };
        let network_height = self.peer_manager.network_best_height();

        if network_height > our_height {
            tracing::info!(
                "Syncing: our height {} < network height {}",
                our_height, network_height
            );
            let best_hash = {
                let chain = self.chain.lock().await;
                chain.best_hash().unwrap_or([0u8; 32])
            };
            let payload = serde_json::to_vec(&GetBlocksMsg {
                version: 70001,
                block_locator_hashes: vec![best_hash],
                hash_stop: [0u8; 32],
            }).unwrap_or_default();
            self.peer_manager.broadcast(NetMessage::new("getblocks", payload)).await;
        }
    }

    /// Maintain peer connections — reconnect if below target using PEX address book.
    async fn maintain_peers(&mut self) {
        let count = self.peer_manager.peer_count();
        if count < TARGET_OUTBOUND {
            let needed = TARGET_OUTBOUND - count;
            tracing::debug!(
                "Low peer count ({}/{}), trying {} PEX candidates",
                count, TARGET_OUTBOUND, needed
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

            // If still not enough, try DHT again
            if connected < needed && self.config.use_dht {
                tracing::debug!("PEX insufficient, re-running DHT bootstrap");
                self.bootstrap_via_dht().await;
            }
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
    /// accepting hole-punch requests from other nodes. The overlay runs as a
    /// background task and emits events that are logged but not yet wired into
    /// the P2P layer (that wiring is the next integration step).
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
                    tracing::info!(
                        "Overlay node ID: {}",
                        &overlay.keypair.node_id()[..16]
                    );
                }

                // Publish our overlay endpoint to the PEX address book so peers
                // can learn it via addr messages.
                if let Some(ep) = overlay.our_endpoint() {
                    tracing::debug!("Overlay endpoint: {}", ep);
                }

                self.overlay = Some(overlay);

                // Clone the event sender so the overlay background task can emit
                // NodeEvents for overlay peer connect/disconnect.
                let overlay_event_tx = self.event_tx.clone();

                // Spawn background task to handle overlay events and forward them
                // as NodeEvents to the RPC WebSocket broadcaster.
                tokio::spawn(async move {
                    while let Some(event) = events.recv().await {
                        match event {
                            OverlayEvent::PeerConnected(ep) => {
                                let ep_str = ep.to_string();
                                tracing::info!("Overlay: peer connected {}", ep_str);
                                if let Some(ref tx) = overlay_event_tx {
                                    // Parse the endpoint string as a SocketAddr; use a
                                    // dummy loopback address if the overlay endpoint is
                                    // not a plain IP:port string.
                                    let addr: std::net::SocketAddr = ep_str
                                        .parse()
                                        .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
                                    let _ = tx.send(std::sync::Arc::new(NodeEvent::PeerConnected {
                                        addr,
                                        user_agent: "overlay".to_string(),
                                        version: 0,
                                        height: 0,
                                    }));
                                }
                            }
                            OverlayEvent::PeerDisconnected(id) => {
                                let short_id = &id[..8.min(id.len())];
                                tracing::info!("Overlay: peer disconnected {}", short_id);
                                if let Some(ref tx) = overlay_event_tx {
                                    let addr: std::net::SocketAddr = id
                                        .parse()
                                        .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap());
                                    let _ = tx.send(std::sync::Arc::new(NodeEvent::PeerDisconnected {
                                        addr,
                                    }));
                                }
                            }
                            OverlayEvent::ExternalAddrDiscovered(addr) => {
                                tracing::info!("Overlay: external address confirmed {}", addr);
                            }
                            OverlayEvent::DataReceived { from_node_id, payload } => {
                                tracing::debug!(
                                    "Overlay: {} bytes from {}",
                                    payload.len(),
                                    &from_node_id[..8.min(from_node_id.len())]
                                );
                            }
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
