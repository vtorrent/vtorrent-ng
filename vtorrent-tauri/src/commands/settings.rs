//! Node settings and redacted diagnostics for the desktop app.

use serde::Serialize;
use tauri::State;

use crate::error::{Result, TauriError};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub network: String,
    pub app_data_dir: Option<String>,
    pub node_data_dir: Option<String>,
    pub btc_network: String,
    pub btc_peer: Option<String>,
    pub wallet_address: Option<String>,
}

async fn app_info(state: &AppState) -> AppInfo {
    let app_data_dir = state
        .app_data_dir
        .lock()
        .unwrap()
        .clone()
        .map(|path| path.to_string_lossy().to_string());
    let guard = state.node.lock().await;
    if let Some(handle) = guard.as_ref() {
        let node_data_dir = handle
            .mempool_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(|path| path.to_string_lossy().to_string());
        let btc_network = format!("{:?}", *handle.rpc_state.btc_network.read().await);
        let btc_peer = handle.rpc_state.btc_peer.read().await.clone();
        let wallet_address = handle.rpc_state.wallet_change_address.read().await.clone();
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            network: handle.network.clone(),
            app_data_dir,
            node_data_dir,
            btc_network,
            btc_peer,
            wallet_address,
        }
    } else {
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            network: "not running".to_string(),
            app_data_dir,
            node_data_dir: None,
            btc_network: "unknown".to_string(),
            btc_peer: None,
            wallet_address: None,
        }
    }
}

fn redact_path(path: Option<String>) -> Option<String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    path.map(|value| match home {
        Some(home) if value.starts_with(&home) => value.replacen(&home, "~", 1),
        _ => value,
    })
}

fn redact_address(address: Option<String>) -> Option<String> {
    address.map(|value| {
        if value.len() <= 12 {
            value
        } else {
            format!("{}…{}", &value[..6], &value[value.len() - 4..])
        }
    })
}

#[tauri::command]
pub async fn get_app_info(state: State<'_, AppState>) -> Result<AppInfo> {
    Ok(app_info(state.inner()).await)
}

#[tauri::command]
pub async fn set_btc_peer(state: State<'_, AppState>, peer: Option<String>) -> Result<()> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let peer = peer
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    *handle.rpc_state.btc_peer.write().await = peer;
    Ok(())
}

#[tauri::command]
pub async fn get_diagnostics(state: State<'_, AppState>) -> Result<String> {
    let info = app_info(state.inner()).await;
    let guard = state.node.lock().await;
    let (height, best_hash, peers, syncing, mempool, uptime, btc_height, btc_synced) =
        if let Some(handle) = guard.as_ref() {
            let chain = handle.rpc_state.chain.lock().await;
            let height = chain.best_height();
            let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();
            drop(chain);
            let peers = *handle.rpc_state.peer_count.read().await;
            let syncing = *handle.rpc_state.syncing.read().await;
            let mempool = handle.rpc_state.mempool.lock().await.size();
            let uptime = handle.start_time.elapsed().as_secs();
            let btc = handle.rpc_state.btc_wallet.read().await;
            let (btc_height, btc_synced) = btc
                .as_ref()
                .map(|wallet| (wallet.best_height(), wallet.synced()))
                .unwrap_or((0, false));
            (
                height, best_hash, peers, syncing, mempool, uptime, btc_height, btc_synced,
            )
        } else {
            (0, String::new(), 0, false, 0, 0, 0, false)
        };

    let diagnostics = serde_json::json!({
        "version": info.version,
        "network": info.network,
        "blockHeight": height,
        "bestBlockHash": best_hash,
        "peers": peers,
        "syncing": syncing,
        "mempoolSize": mempool,
        "uptimeSecs": uptime,
        "btcNetwork": info.btc_network,
        "btcPeer": info.btc_peer,
        "btcHeight": btc_height,
        "btcSynced": btc_synced,
        "walletAddress": redact_address(info.wallet_address),
        "appDataDir": redact_path(info.app_data_dir),
        "nodeDataDir": redact_path(info.node_data_dir),
    });
    serde_json::to_string_pretty(&diagnostics).map_err(|error| TauriError::Io(error.to_string()))
}

#[tauri::command]
pub fn restart_app(app: tauri::AppHandle) {
    app.restart();
}
