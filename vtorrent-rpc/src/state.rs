use std::sync::Arc;
use tokio::sync::RwLock;
use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;
use vtorrent_node::staking::StakingEngine;
use vtorrent_node::atomic_swap::SwapOrderBook;
use vtorrent_torrent::session::SessionManager;

/// Shared application state for the RPC API server.
///
/// All state is wrapped in Arc<RwLock<...>> for safe concurrent access
/// across multiple Axum handler tasks.
#[derive(Clone)]
pub struct AppState {
    /// The blockchain state.
    pub chain: Arc<RwLock<Chain>>,
    /// The transaction mempool.
    pub mempool: Arc<RwLock<Mempool>>,
    /// The PoS staking engine.
    pub staking: Arc<RwLock<StakingEngine>>,
    /// The DEX order book.
    pub order_book: Arc<RwLock<SwapOrderBook>>,
    /// The torrent session manager.
    pub torrent_sessions: Arc<RwLock<SessionManager>>,
    /// Node start time (Unix timestamp).
    pub start_time: u64,
    /// Number of connected P2P peers.
    pub peer_count: Arc<RwLock<usize>>,
    /// Whether the node is currently syncing.
    pub syncing: Arc<RwLock<bool>>,
    /// Wallet unlock expiry (Unix timestamp, None = locked).
    pub wallet_unlock_expiry: Arc<RwLock<Option<u64>>>,
    /// Staking enabled flag.
    pub staking_enabled: Arc<RwLock<bool>>,
    /// Staking address.
    pub staking_address: Arc<RwLock<Option<String>>>,
    /// Total blocks staked this session.
    pub blocks_staked: Arc<RwLock<u64>>,
}

impl AppState {
    /// Create a new AppState with a fresh chain and empty components.
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        AppState {
            chain: Arc::new(RwLock::new(Chain::new().expect("failed to initialize chain"))),
            mempool: Arc::new(RwLock::new(Mempool::new(10_000))),
            staking: Arc::new(RwLock::new(StakingEngine::new(String::new()))),
            order_book: Arc::new(RwLock::new(SwapOrderBook::new())),
            torrent_sessions: Arc::new(RwLock::new(SessionManager::new())),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            blocks_staked: Arc::new(RwLock::new(0)),
        }
    }

    /// Check if the wallet is currently unlocked.
    pub async fn is_wallet_unlocked(&self) -> bool {
        let expiry = self.wallet_unlock_expiry.read().await;
        match *expiry {
            None => false,
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                exp == 0 || now < exp
            }
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
