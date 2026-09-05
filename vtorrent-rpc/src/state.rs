use crate::ratelimit::SharedRateLimiter;
use crate::ws::EventBroadcaster;
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
    /// Cancellation tokens for active torrent engine tasks, keyed by session id.
    pub torrent_cancels:
        Arc<RwLock<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>,
    /// SPV header chain for light-client verification.
    pub spv_chain: Arc<RwLock<SpvChain>>,
    pub stake_proofs: Arc<RwLock<std::collections::HashMap<[u8; 32], StakeProof>>>,
    /// Bitcoin SPV wallet (optional — created when a seed is available).
    pub btc_wallet: Arc<RwLock<Option<BtcWallet>>>,
    /// Bitcoin network the wallet operates on (mainnet or regtest).
    pub btc_network: Arc<RwLock<bitcoin::Network>>,
    /// Optional explicit Bitcoin peer as `host:port` (regtest). When set, BTC
    /// sync and broadcast use this peer instead of DNS seeds. Stored as a
    /// hostname so container restarts with new IPs are re-resolved per cycle.
    pub btc_peer: Arc<RwLock<Option<String>>>,
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
    /// File path recording the staking intent (`{"address": ...}`). When set,
    /// staking auto-resumes whenever the wallet is unlocked.
    pub staking_state_path: Option<std::path::PathBuf>,
    /// TOTP secret for the hot wallet's 2FA (optional, set on import).
    pub wallet_totp_secret: Arc<RwLock<Option<TotpSecret>>>,
    /// Derived change address for the hot wallet (from `wallet_wif`).
    pub wallet_change_address: Arc<RwLock<Option<String>>>,
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
            torrent_cancels: Arc::new(RwLock::new(std::collections::HashMap::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            stake_proofs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            btc_wallet: Arc::new(RwLock::new(None)),
            btc_network: Arc::new(RwLock::new(bitcoin::Network::Bitcoin)),
            btc_peer: Arc::new(RwLock::new(None)),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            blocks_staked: Arc::new(RwLock::new(0)),
            last_stake_time: Arc::new(RwLock::new(0)),
            rewards_earned_sats: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            block_submit: None,
            staking_control: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            wallet_path: None,
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
            torrent_cancels: Arc::new(RwLock::new(std::collections::HashMap::new())),
            spv_chain: Arc::new(RwLock::new(SpvChain::new())),
            stake_proofs: Arc::new(RwLock::new(std::collections::HashMap::new())),
            btc_wallet: Arc::new(RwLock::new(None)),
            btc_network: Arc::new(RwLock::new(bitcoin::Network::Bitcoin)),
            btc_peer: Arc::new(RwLock::new(None)),
            start_time: now,
            peer_count: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
            wallet_unlock_expiry: Arc::new(RwLock::new(None)),
            staking_enabled: Arc::new(RwLock::new(false)),
            staking_address: Arc::new(RwLock::new(None)),
            blocks_staked: Arc::new(RwLock::new(0)),
            last_stake_time: Arc::new(RwLock::new(0)),
            rewards_earned_sats: Arc::new(RwLock::new(0)),
            events: EventBroadcaster::new(1024),
            wallet_wif: Arc::new(RwLock::new(None)),
            wallet_encrypted: Arc::new(RwLock::new(None)),
            wallet_totp_secret: Arc::new(RwLock::new(None)),
            wallet_change_address: Arc::new(RwLock::new(None)),
            best_peer_height: Arc::new(RwLock::new(0)),
            tx_submit: None,
            block_submit: None,
            staking_control: None,
            peer_list: Arc::new(RwLock::new(Vec::new())),
            wallet_path: None,
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
