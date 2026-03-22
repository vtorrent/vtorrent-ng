/// Application state managed by Tauri's state system.
///
/// Holds both the wallet state and the embedded node state.
/// The node runs as a background tokio task; the RPC AppState is shared
/// between the node task and the Tauri command handlers so that all data
/// (chain, mempool, DEX, torrents, staking) is always consistent.

use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
use vtorrent_wallet::wallet::Wallet;
use vtorrent_rpc::state::AppState as RpcAppState;

/// Shared handle to the running node's RPC state.
///
/// Created when `start_node` is called and stored in `AppState`.
/// The `rpc_state` field is the same `Arc`-wrapped state used by the
/// embedded RPC server, so all Tauri commands see live data.
pub struct NodeHandle {
    /// The full RPC AppState — contains chain, mempool, DEX, torrents, staking.
    pub rpc_state: RpcAppState,
}

/// Global application state managed by Tauri.
pub struct AppState {
    /// The active wallet, if one has been loaded and unlocked.
    /// Uses `std::sync::Mutex` because wallet operations are synchronous.
    pub wallet: Mutex<Option<Wallet>>,
    /// Path to the wallet file on disk.
    pub wallet_path: Mutex<Option<std::path::PathBuf>>,
    /// The embedded node handle — `None` until `start_node` is called.
    /// Uses `tokio::sync::Mutex` so it can be held across `.await` points
    /// in async Tauri commands.
    pub node: TokioMutex<Option<NodeHandle>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            wallet: Mutex::new(None),
            wallet_path: Mutex::new(None),
            node: TokioMutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
