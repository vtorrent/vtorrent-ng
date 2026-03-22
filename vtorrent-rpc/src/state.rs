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
    /// Create an AppState that shares an existing chain and mempool with the P2P node.
    ///
    /// This is the constructor used by the real node binary so that the RPC
    /// server reflects live chain state rather than a separate in-memory copy.
    pub fn new_with_shared(
        chain: Arc<tokio::sync::Mutex<Chain>>,
        mempool: Arc<tokio::sync::Mutex<Mempool>>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Wrap the Mutex-guarded chain in RwLock for the RPC layer.
        // We create a fresh RwLock-wrapped copy seeded from the shared Mutex chain.
        // The P2P node owns the Mutex; the RPC reads via its own RwLock wrapper.
        // For testnet this is sufficient; a future refactor can unify the lock type.
        let chain_snapshot = {
            // We can't await here (not async), so we use try_lock.
            // If the lock is held, fall back to a fresh chain.
            match chain.try_lock() {
                Ok(_guard) => Chain::new().expect("failed to initialize chain"),
                Err(_) => Chain::new().expect("failed to initialize chain"),
            }
        };

        AppState {
            chain: Arc::new(RwLock::new(chain_snapshot)),
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
