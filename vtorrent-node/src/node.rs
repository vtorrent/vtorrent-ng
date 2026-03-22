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

use vtorrent_p2p::{
    message::{
        GetBlocksMsg, InvItem, InvMsg, InvType, NetMessage,
    },
    peer::PeerEvent,
    peer_manager::{PeerManager, DEFAULT_PORT, DNS_SEEDS, TARGET_OUTBOUND},
};

use crate::{
    block::{Block, Transaction},
    chain::Chain,
    consensus::TARGET_BLOCK_TIME,
    error::{NodeError, Result},
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
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            listen_addr: format!("0.0.0.0:{}", DEFAULT_PORT),
            staking_enabled: false,
            staking_address: None,
            max_mempool: 10_000,
            extra_seeds: Vec::new(),
        }
    }
}

/// The vTorrent node.
pub struct Node {
    chain: Arc<Mutex<Chain>>,
    mempool: Arc<Mutex<Mempool>>,
    peer_manager: PeerManager,
    staking: Option<StakingEngine>,
    config: NodeConfig,
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
        })
    }

    /// Start the node — connects to peers and begins the event loop.
    pub async fn start(&mut self) -> Result<()> {
        tracing::info!("Starting vTorrent node on {}", self.config.listen_addr);

        // Start P2P listener
        self.peer_manager.start().await
            .map_err(|e| NodeError::Chain(format!("P2P start failed: {}", e)))?;

        // Connect to DNS seeds
        self.connect_to_seeds().await;

        // Periodic timers
        let mut sync_ticker = interval(Duration::from_secs(30));
        let mut stake_ticker = interval(Duration::from_secs(TARGET_BLOCK_TIME as u64));
        let mut peer_ticker = interval(Duration::from_secs(60));

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
                        "Peers: {} | Height: {} | Mempool: {} txs",
                        self.peer_manager.peer_count(),
                        height,
                        mp_size
                    );
                }
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
            }

            PeerEvent::Message { peer_addr, msg } => {
                self.handle_message(peer_addr, msg).await?;
            }

            PeerEvent::Disconnected { peer_addr } => {
                tracing::info!("Peer {} disconnected", peer_addr);
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
                            Ok(()) => {
                                let hash = block.hash();
                                tracing::info!(
                                    "Accepted block {} at height {}",
                                    hex::encode(hash),
                                    chain.best_height()
                                );
                                // Relay to other peers
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
                                tracing::debug!("Accepted tx {}", hex::encode(txid));
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
            {
                let mut chain = self.chain.lock().await;
                chain.add_block(block)?;
                tracing::info!(
                    "Staked new block {} at height {}",
                    hex::encode(block_hash),
                    chain.best_height()
                );
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

    /// Connect to DNS seed nodes.
    async fn connect_to_seeds(&mut self) {
        let mut seeds: Vec<String> = DNS_SEEDS.iter()
            .map(|s| format!("{}:{}", s, DEFAULT_PORT))
            .collect();
        seeds.extend(self.config.extra_seeds.clone());

        for seed in seeds {
            tracing::info!("Connecting to seed: {}", seed);
            if let Err(e) = self.peer_manager.connect(&seed).await {
                tracing::debug!("Could not connect to {}: {}", seed, e);
            }
        }
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

    /// Maintain peer connections — reconnect if below target.
    async fn maintain_peers(&mut self) {
        let count = self.peer_manager.peer_count();
        if count < TARGET_OUTBOUND {
            tracing::debug!("Low peer count ({}), reconnecting to seeds", count);
            self.connect_to_seeds().await;
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
}
