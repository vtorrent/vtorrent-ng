use crate::ratelimit::SharedRateLimiter;
use crate::ws::EventBroadcaster;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use vtorrent_btc::wallet::BtcWallet;
use vtorrent_node::atomic_swap::{SwapOrderBook, SwapState};
use vtorrent_node::block::Transaction;
use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;
use vtorrent_node::staking::StakingCommand;
use vtorrent_spv::stake::StakeProof;
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

/// Runtime health counters for the Bitcoin SPV sync loop.
#[derive(Default)]
pub struct BtcSyncHealth {
    pub best_height: AtomicU64,
    pub last_scanned_height: AtomicU64,
    pub synced: AtomicBool,
    pub peer_groups: AtomicU64,
    pub scan_in_progress: AtomicBool,
    pub scan_duration_ms: AtomicU64,
    pub last_success_unix: AtomicU64,
    pub auth_failures: AtomicU64,
    pub scan_errors: AtomicU64,
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
    /// The DEX order book.
    pub order_book: Arc<RwLock<SwapOrderBook>>,
    /// Active cross-chain swaps keyed by hex order_id.
    pub swaps: Arc<RwLock<std::collections::HashMap<String, SwapState>>>,
    /// The torrent session manager.
    pub torrent_sessions: Arc<RwLock<SessionManager>>,
    /// Directory where downloaded torrent data is written.
    pub download_dir: Arc<RwLock<std::path::PathBuf>>,
    /// Shared torrent upload/download rate limiter.
    pub torrent_limits: Arc<vtorrent_torrent::limits::TorrentRateLimiter>,
    /// Stop seeding once uploaded/downloaded reaches this ratio (0 = disabled).
    pub seed_ratio_target: Arc<RwLock<f64>>,
    /// Cancellation tokens for active torrent engine tasks, keyed by session id.
    pub torrent_cancels:
        Arc<RwLock<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>,
    /// SPV header chain for light-client verification.
    pub spv_chain: Arc<RwLock<SpvChain>>,
    pub stake_proofs: Arc<RwLock<std::collections::HashMap<[u8; 32], StakeProof>>>,
    /// Bitcoin SPV wallet (optional — created when a seed is available).
    pub btc_wallet: Arc<RwLock<Option<BtcWallet>>>,
    pub btc_swap_scan_lock: Arc<Mutex<()>>,
    /// Bitcoin network the wallet operates on (mainnet or regtest).
    pub btc_network: Arc<RwLock<bitcoin::Network>>,
    /// Optional explicit Bitcoin peer as `host:port` (regtest). When set, BTC
    /// sync and broadcast use this peer instead of DNS seeds. Stored as a
    /// hostname so container restarts with new IPs are re-resolved per cycle.
    pub btc_peer: Arc<RwLock<Option<String>>>,
    /// Working compact-filter peers learned from successful connections.
    pub btc_peer_cache: Arc<RwLock<Vec<SocketAddr>>>,
    /// Optional path used to persist `btc_peer_cache` across restarts.
    pub btc_peer_cache_path: Option<PathBuf>,
    /// Runtime health counters for the BTC SPV sync loop.
    pub btc_health: Arc<BtcSyncHealth>,
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
    /// Whether the active staking key was imported for cold-stake delegation.
    pub staking_key_imported: Arc<RwLock<bool>>,
    /// Total blocks staked this session.
    pub blocks_staked: Arc<RwLock<u64>>,
    /// Unix timestamp of the most recent staked block (0 = never staked).
    pub last_stake_time: Arc<RwLock<u32>>,
    /// Total staking rewards earned this session (satoshis).
    pub rewards_earned_sats: Arc<RwLock<u64>>,
    /// WebSocket event broadcaster.
    pub events: EventBroadcaster,
    /// Hot wallet WIF private key (in-memory only, never persisted to disk).
    ///
    /// Populated by `POST /api/v1/wallet/unlock` after the passphrase (and TOTP,
    /// if enabled) have been verified against `wallet_encrypted`. Cleared on
    /// wallet lock or daemon restart.
    pub wallet_wif: Arc<RwLock<Option<zeroize::Zeroizing<String>>>>,
    /// The imported hot-wallet WIF encrypted with the wallet passphrase
    /// (Argon2id + ChaCha20-Poly1305). Set on import (and restored from disk
    /// on startup when `wallet_path` is configured); used to verify the
    /// passphrase on unlock and send.
    pub wallet_encrypted: Arc<RwLock<Option<EncryptedWallet>>>,
    /// File path where the encrypted hot wallet is persisted. `None` disables
    /// persistence (standalone/test instances).
    pub wallet_path: Option<std::path::PathBuf>,
    /// Per-wallet encrypted swap recovery records; disabled only in ephemeral instances.
    pub swap_recovery_dir: Option<std::path::PathBuf>,
    pub swap_recovery_lock: Arc<Mutex<()>>,
    /// File path recording the staking intent (`{"address": ...}`). When set,
    /// staking auto-resumes whenever the wallet is unlocked.
    pub staking_state_path: Option<std::path::PathBuf>,
    /// TOTP secret for the hot wallet's 2FA (optional, set on import).
    pub wallet_totp_secret: Arc<RwLock<Option<TotpSecret>>>,
    /// Highest TOTP time-step already accepted for this wallet.
    ///
    /// TOTP codes are valid for a ±1-step window, so without this an observed
    /// code could be replayed within ~90 seconds. Any step at or below this is
    /// rejected.
    pub wallet_totp_last_step: Arc<RwLock<Option<u64>>>,
    /// Derived change address for the hot wallet (from `wallet_wif`).
    pub wallet_change_address: Arc<RwLock<Option<String>>>,
    /// P2CS scripts generated by this process. `fund_cold_stake` only accepts
    /// a script in this set, so a caller cannot redirect funds to an arbitrary
    /// cold key without first generating it through `create_cold_stake`.
    pub cold_stake_scripts: Arc<RwLock<std::collections::HashSet<Vec<u8>>>>,
    /// Single-use, short-lived WebSocket tokens (token -> expiry Unix seconds).
    /// Keeps the long-lived API key out of WebSocket URLs and headers.
    pub ws_tokens: Arc<RwLock<std::collections::HashMap<String, u64>>>,
    /// Best block height reported by any connected peer (used for sync % calculation).
    pub best_peer_height: Arc<RwLock<u64>>,
    /// Channel for submitting locally-created transactions into the node's event loop.
    /// `None` when running in standalone/test mode (no live P2P node).
    pub tx_submit: Option<mpsc::Sender<Transaction>>,
    /// Channel for submitting locally-minted blocks (regtest faucet) into the
    /// node's event loop so they are announced to peers. `None` in standalone mode.
    pub block_submit: Option<mpsc::Sender<vtorrent_node::block::Block>>,
    /// Channel for enabling/disabling staking at runtime. `None` in standalone mode.
    pub staking_control: Option<mpsc::Sender<StakingCommand>>,
    /// Shared ban manager for operator unbans. `None` in standalone mode.
    pub ban_manager: Option<Arc<tokio::sync::RwLock<vtorrent_p2p::ban_manager::BanManager>>>,
    /// Live list of connected peers — updated by the daemon event bridge.
    pub peer_list: Arc<RwLock<Vec<PeerInfo>>>,
    /// Optional RPC API key. When set, sensitive endpoints require the
    /// `X-API-Key` header to match (constant-time compared).
    pub rpc_api_key: Option<String>,
    /// Regtest mode: enables the faucet endpoint and relaxed staking.
    pub regtest: bool,
    /// Human-readable network label (e.g. "vtorrent-mainnet", "vtorrent-testnet",
    /// "vtorrent-regtest"). Set by the daemon from its config.
    pub network: String,
    /// Regtest mock time (Unix timestamp). When set, time-dependent checks
    /// (e.g. HTLC expiry) use this instead of the wall clock. `None` = real time.
    pub mock_time: Arc<RwLock<Option<u64>>>,
    /// Per-IP rate limiter (sliding window, 100 req/min).
    pub rate_limiter: SharedRateLimiter,
    /// Faucet per-address cooldown (address → last claim time). Prevents
    /// draining the regtest faucet by repeated requests from the same address.
    pub faucet_cooldowns:
        Arc<tokio::sync::RwLock<std::collections::HashMap<String, std::time::Instant>>>,
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
            order_book: Arc::new(RwLock::new(SwapOrderBook::new())),
            swaps: Arc::new(RwLock::new(std::collections::HashMap::new())),
            torrent_sessions: Arc::new(RwLock::new(SessionManager::new())),
            download_dir: Arc::new(RwLock::new(std::path::PathBuf::from("downloads"))),
            torrent_limits: Arc::new(vtorrent_torrent::limits::TorrentRateLimiter::new(0, 0)),
            seed_ratio_target: Arc::new(RwLock::new(0.0)),
            torrent_cancels: Arc::new(RwLock::new(std::collections::HashMap::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            stake_proofs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            btc_wallet: Arc::new(RwLock::new(None)),
            btc_swap_scan_lock: Arc::new(Mutex::new(())),
            btc_network: Arc::new(RwLock::new(bitcoin::Network::Bitcoin)),
            btc_peer: Arc::new(RwLock::new(None)),
            btc_peer_cache: Arc::new(RwLock::new(Vec::new())),
            btc_peer_cache_path: None,
            btc_health: Arc::new(BtcSyncHealth::default()),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            staking_key_imported: Arc::new(RwLock::new(false)),
            blocks_staked: Arc::new(RwLock::new(0)),
            last_stake_time: Arc::new(RwLock::new(0)),
            rewards_earned_sats: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_totp_last_step: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            cold_stake_scripts: Arc::new(RwLock::new(std::collections::HashSet::new())),
            ws_tokens: Arc::new(RwLock::new(std::collections::HashMap::new())),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            block_submit: None,
            staking_control: None,
            ban_manager: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            wallet_path: None,
            swap_recovery_dir: None,
            swap_recovery_lock: Arc::new(Mutex::new(())),
            staking_state_path: None,
            rpc_api_key: None,
            regtest: false,
            network: "vtorrent-mainnet".to_string(),
            mock_time: Arc::new(RwLock::new(None)),
            rate_limiter: crate::ratelimit::new_shared_limiter(),
            faucet_cooldowns: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
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
            order_book: Arc::new(RwLock::new(SwapOrderBook::new())),
            swaps: Arc::new(RwLock::new(std::collections::HashMap::new())),
            torrent_sessions: Arc::new(RwLock::new(SessionManager::new())),
            download_dir: Arc::new(RwLock::new(std::path::PathBuf::from("downloads"))),
            torrent_limits: Arc::new(vtorrent_torrent::limits::TorrentRateLimiter::new(0, 0)),
            seed_ratio_target: Arc::new(RwLock::new(0.0)),
            torrent_cancels: Arc::new(RwLock::new(std::collections::HashMap::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            stake_proofs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            btc_wallet: Arc::new(RwLock::new(None)),
            btc_swap_scan_lock: Arc::new(Mutex::new(())),
            btc_network: Arc::new(RwLock::new(bitcoin::Network::Bitcoin)),
            btc_peer: Arc::new(RwLock::new(None)),
            btc_peer_cache: Arc::new(RwLock::new(Vec::new())),
            btc_peer_cache_path: None,
            btc_health: Arc::new(BtcSyncHealth::default()),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            staking_key_imported: Arc::new(RwLock::new(false)),
            blocks_staked: Arc::new(RwLock::new(0)),
            last_stake_time: Arc::new(RwLock::new(0)),
            rewards_earned_sats: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_totp_last_step: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            cold_stake_scripts: Arc::new(RwLock::new(std::collections::HashSet::new())),
            ws_tokens: Arc::new(RwLock::new(std::collections::HashMap::new())),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            block_submit: None,
            staking_control: None,
            ban_manager: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            wallet_path: None,
            swap_recovery_dir: None,
            swap_recovery_lock: Arc::new(Mutex::new(())),
            staking_state_path: None,
            rpc_api_key: None,
            regtest: false,
            network: "vtorrent-mainnet".to_string(),
            mock_time: Arc::new(RwLock::new(None)),
            rate_limiter: crate::ratelimit::new_shared_limiter(),
            faucet_cooldowns: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Check if the wallet is currently unlocked.
    ///
    /// If the unlock has expired, the hot key is cleared from memory so it
    /// cannot linger indefinitely.
    pub async fn is_wallet_unlocked(&self) -> bool {
        let expiry = self.wallet_unlock_expiry.read().await;
        match *expiry {
            None => false,
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if exp != 0 && now >= exp {
                    drop(expiry);
                    self.lock_wallet().await;
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Lock the wallet, clearing the hot key and change address from memory.
    pub async fn lock_wallet(&self) {
        *self.wallet_wif.write().await = None;
        *self.wallet_change_address.write().await = None;
        *self.wallet_unlock_expiry.write().await = None;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
