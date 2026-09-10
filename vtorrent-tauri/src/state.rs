/// Application state managed by Tauri's state system.
///
/// Holds both the wallet state and the embedded node state.
/// The node runs as a background tokio task; the RPC AppState is shared
/// between the node task and the Tauri command handlers so that all data
/// (chain, mempool, DEX, torrents, staking) is always consistent.
use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
use vtorrent_rpc::state::AppState as RpcAppState;
use vtorrent_wallet::wallet::Wallet;

/// Shared handle to the running node's RPC state.
///
/// Created when `start_node` is called and stored in `AppState`.
/// The `rpc_state` field is the same `Arc`-wrapped state used by the
/// embedded RPC server, so all Tauri commands see live data.
pub struct NodeHandle {
    /// The full RPC AppState — contains chain, mempool, DEX, torrents, staking.
    pub rpc_state: RpcAppState,
    /// Canonical network name (`vtorrent-mainnet` / `vtorrent-testnet`).
    pub network: String,
    /// When the node was started (for uptime calculation).
    pub start_time: std::time::Instant,
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
    pub async fn sync_swap_wallet(&self, rpc: &RpcAppState) -> crate::error::Result<()> {
        let (wif, address) = {
            let guard = self
                .wallet
                .lock()
                .map_err(|_| crate::error::TauriError::WalletLocked)?;
            let wallet = guard
                .as_ref()
                .ok_or(crate::error::TauriError::WalletLocked)?;
            let wif = zeroize::Zeroizing::new(
                wallet
                    .get_default_wif()
                    .ok_or(crate::error::TauriError::WalletLocked)?
                    .to_owned(),
            );
            let address = wallet
                .default_address()
                .ok_or(crate::error::TauriError::WalletLocked)?
                .to_string();
            (wif, address)
        };
        let same_key = rpc
            .wallet_wif
            .read()
            .await
            .as_ref()
            .is_some_and(|current| current.as_str() == wif.as_str());
        if same_key {
            *rpc.wallet_unlock_expiry.write().await = Some(0);
            return Ok(());
        }
        if let Err(error) = vtorrent_rpc::swap_recovery::restore_with_wif(rpc, &wif).await {
            rpc.lock_wallet().await;
            return Err(error.into());
        }
        *rpc.wallet_wif.write().await = Some(wif);
        *rpc.wallet_change_address.write().await = Some(address);
        *rpc.wallet_unlock_expiry.write().await = Some(0);
        Ok(())
    }

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
