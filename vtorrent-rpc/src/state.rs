use crate::ws::EventBroadcaster;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use vtorrent_node::atomic_swap::SwapOrderBook;
use vtorrent_node::block::Transaction;
use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;
use vtorrent_node::staking::StakingEngine;
use vtorrent_spv::SpvChain;
use vtorrent_torrent::session::SessionManager;
use vtorrent_wallet::encryption::EncryptedWallet;
use vtorrent_wallet::otp::TotpSecret;

/// Snapshot of a connected peer's metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    /// Socket address of the peer (e.g. `"1.2.3.4:22526"`).
    pub addr: String,
    /// Peer's self-reported user-agent string.
    pub user_agent: String,
    /// Peer's advertised service flags.
    pub services: u64,
    /// Peer's best known block height.
    pub best_height: u32,
}

/// Shared application state for the RPC API server.
///
/// `chain` and `mempool` use `Arc<Mutex<...>>` so they can be shared directly
/// with the P2P node (which also uses `tokio::sync::Mutex`). All other fields
/// use `Arc<RwLock<...>>` since they are RPC-only and benefit from concurrent
/// read access.
#[derive(Clone)]
pub struct AppState {
    /// The blockchain state — shared with the P2P node.
    pub chain: Arc<Mutex<Chain>>,
    /// The transaction mempool — shared with the P2P node.
    pub mempool: Arc<Mutex<Mempool>>,
    /// The PoS staking engine.
    pub staking: Arc<RwLock<StakingEngine>>,
    /// The DEX order book.
    pub order_book: Arc<RwLock<SwapOrderBook>>,
    /// The torrent session manager.
    pub torrent_sessions: Arc<RwLock<SessionManager>>,
    /// SPV header chain for light-client verification.
    pub spv_chain: Arc<RwLock<SpvChain>>,
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
    /// WebSocket event broadcaster.
    pub events: EventBroadcaster,
    /// Hot wallet WIF private key (in-memory only, never persisted to disk).
    ///
    /// Populated by `POST /api/v1/wallet/unlock` after the passphrase (and TOTP,
    /// if enabled) have been verified against `wallet_encrypted`. Cleared on
    /// wallet lock or daemon restart.
    pub wallet_wif: Arc<RwLock<Option<String>>>,
    /// The imported hot-wallet WIF encrypted with the wallet passphrase
    /// (Argon2id + ChaCha20-Poly1305). Set on import; used to verify the
    /// passphrase on unlock and send. Never persisted to disk.
    pub wallet_encrypted: Arc<RwLock<Option<EncryptedWallet>>>,
    /// TOTP secret for the hot wallet's 2FA (optional, set on import).
    pub wallet_totp_secret: Arc<RwLock<Option<TotpSecret>>>,
    /// Derived change address for the hot wallet (from `wallet_wif`).
    pub wallet_change_address: Arc<RwLock<Option<String>>>,
    /// Best block height reported by any connected peer (used for sync % calculation).
    pub best_peer_height: Arc<RwLock<u64>>,
    /// Channel for submitting locally-created transactions into the node's event loop.
    /// `None` when running in standalone/test mode (no live P2P node).
    pub tx_submit: Option<mpsc::Sender<Transaction>>,
    /// Live list of connected peers — updated by the daemon event bridge.
    pub peer_list: Arc<RwLock<Vec<PeerInfo>>>,
    /// Optional RPC API key. When set, sensitive endpoints require the
    /// `X-API-Key` header to match (constant-time compared).
    pub rpc_api_key: Option<String>,
}

impl AppState {
    /// Create an AppState that shares the live chain and mempool Arcs from the
    /// P2P node.  This is the constructor used by `vtorrent-daemon` so that RPC
    /// responses always reflect the current chain state.
    pub fn new_with_shared(chain: Arc<Mutex<Chain>>, mempool: Arc<Mutex<Mempool>>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        AppState {
            chain,
            mempool,
            staking: Arc::new(RwLock::new(StakingEngine::new(String::new()))),
            order_book: Arc::new(RwLock::new(SwapOrderBook::new())),
            torrent_sessions: Arc::new(RwLock::new(SessionManager::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            blocks_staked: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            rpc_api_key: None,
        }
    }

    /// Create a new AppState with a fresh chain and empty components.
    /// Used by standalone RPC server instances and tests.
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        AppState {
            chain: Arc::new(Mutex::new(
                Chain::new().expect("failed to initialize chain"),
            )),
            mempool: Arc::new(Mutex::new(Mempool::new(10_000))),
            staking: Arc::new(RwLock::new(StakingEngine::new(String::new()))),
            order_book: Arc::new(RwLock::new(SwapOrderBook::new())),
            torrent_sessions: Arc::new(RwLock::new(SessionManager::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            blocks_staked: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            rpc_api_key: None,
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
