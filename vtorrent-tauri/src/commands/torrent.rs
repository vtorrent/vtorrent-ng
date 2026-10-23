use serde::Serialize;
use std::sync::Arc;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct TorrentResult {
    pub id: String,
    pub name: String,
    pub info_hash: String,
    pub state: String,
    pub progress: f64,
    pub size_bytes: u64,
    pub downloaded_bytes: u64,
    pub uploaded_bytes: u64,
    pub download_speed: u64,
    pub upload_speed: u64,
    pub peer_count: usize,
    pub vtr_earned_satoshis: u64,
    pub vtr_paid_satoshis: u64,
    pub vtr_pending_earned_satoshis: u64,
    pub vtr_pending_owed_satoshis: u64,
    pub vtr_settled_earned_satoshis: u64,
    pub vtr_settled_paid_satoshis: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentSettingsResult {
    pub download_dir: String,
    pub download_limit_bytes_per_sec: u64,
    pub upload_limit_bytes_per_sec: u64,
    pub seed_ratio_target: f64,
}

#[tauri::command]
pub async fn get_torrent_settings(
    state: tauri::State<'_, AppState>,
) -> Result<TorrentSettingsResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let download_dir = handle
        .rpc_state
        .download_dir
        .read()
        .await
        .to_string_lossy()
        .to_string();
    let download_limit_bytes_per_sec = handle.rpc_state.torrent_limits.download_rate();
    let upload_limit_bytes_per_sec = handle.rpc_state.torrent_limits.upload_rate();
    let seed_ratio_target = *handle.rpc_state.seed_ratio_target.read().await;
    Ok(TorrentSettingsResult {
        download_dir,
        download_limit_bytes_per_sec,
        upload_limit_bytes_per_sec,
        seed_ratio_target,
    })
}

#[tauri::command]
pub async fn set_torrent_settings(
    state: tauri::State<'_, AppState>,
    download_dir: String,
    download_limit_bytes_per_sec: u64,
    upload_limit_bytes_per_sec: u64,
    seed_ratio_target: f64,
) -> Result<TorrentSettingsResult> {
    let download_dir = download_dir.trim();
    if download_dir.is_empty() {
        return Err(TauriError::InvalidInput(
            "Download directory is required".into(),
        ));
    }
    std::fs::create_dir_all(download_dir).map_err(|e| TauriError::Io(e.to_string()))?;
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    *handle.rpc_state.download_dir.write().await = std::path::PathBuf::from(download_dir);
    handle
        .rpc_state
        .torrent_limits
        .set_rates(download_limit_bytes_per_sec, upload_limit_bytes_per_sec);
    *handle.rpc_state.seed_ratio_target.write().await = seed_ratio_target.clamp(0.0, 100.0);
    drop(guard);
    get_torrent_settings(state).await
}

#[derive(Debug, Serialize)]
pub struct AddTorrentResult {
    pub session_id: String,
    pub info_hash: String,
    pub name: String,
}

#[tauri::command]
pub async fn get_torrent_sessions(state: tauri::State<'_, AppState>) -> Result<Vec<TorrentResult>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let sessions = handle.rpc_state.torrent_sessions.read().await;
    Ok(sessions
        .list_sessions()
        .into_iter()
        .map(|s| TorrentResult {
            id: s.id.clone(),
            name: s.metainfo.name.clone(),
            info_hash: hex::encode(s.metainfo.info_hash),
            state: format!("{:?}", s.state),
            progress: s.progress(),
            size_bytes: s.metainfo.total_size,
            downloaded_bytes: s.bytes_downloaded,
            uploaded_bytes: s.bytes_uploaded,
            download_speed: s.download_speed,
            upload_speed: s.upload_speed,
            peer_count: s.peers.len(),
            vtr_earned_satoshis: s.incentive_summary().total_earned_satoshis,
            vtr_paid_satoshis: s.incentive_summary().total_paid_satoshis,
            vtr_pending_earned_satoshis: s.incentive_summary().pending_earned_satoshis,
            vtr_pending_owed_satoshis: s.incentive_summary().pending_owed_satoshis,
            vtr_settled_earned_satoshis: s.incentive_summary().total_earned_satoshis,
            vtr_settled_paid_satoshis: s.incentive_summary().total_paid_satoshis,
        })
        .collect())
}

#[tauri::command]
pub async fn add_torrent(
    state: tauri::State<'_, AppState>,
    source: String,
    source_type: String,
    wallet_address: String,
) -> Result<AddTorrentResult> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use vtorrent_torrent::metainfo::{MagnetLink, Metainfo};
    use vtorrent_torrent::session::TorrentSession;

    let metainfo = if source_type == "magnet" {
        let magnet = MagnetLink::parse(&source).map_err(|e| TauriError::Torrent(e.to_string()))?;
        Metainfo::from_magnet_link(&magnet)
    } else {
        let bytes = B64
            .decode(&source)
            .map_err(|e| TauriError::Torrent(e.to_string()))?;
        Metainfo::from_bytes(&bytes).map_err(|e| TauriError::Torrent(e.to_string()))?
    };

    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo, wallet_address);

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let session_id = handle
        .rpc_state
        .torrent_sessions
        .write()
        .await
        .add_session(session)
        .ok_or_else(|| {
            TauriError::InvalidInput(format!(
                "Too many active torrent sessions (max {})",
                vtorrent_torrent::session::MAX_TORRENT_SESSIONS
            ))
        })?;

    let cancel = tokio_util::sync::CancellationToken::new();
    handle
        .rpc_state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&handle.rpc_state.torrent_sessions);
    let download_dir = handle.rpc_state.download_dir.read().await.clone();
    let limits = Arc::clone(&handle.rpc_state.torrent_limits);
    let seed_ratio_target = Arc::clone(&handle.rpc_state.seed_ratio_target);
    let sid = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(
            sid,
            sessions,
            download_dir,
            limits,
            seed_ratio_target,
            cancel,
        )
        .await;
    });

    Ok(AddTorrentResult {
        session_id,
        info_hash,
        name,
    })
}

#[tauri::command]
pub async fn remove_torrent(state: tauri::State<'_, AppState>, id: String) -> Result<()> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    if let Some(cancel) = handle.rpc_state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    handle
        .rpc_state
        .torrent_sessions
        .write()
        .await
        .remove_session(&id)
        .ok_or_else(|| TauriError::Torrent(format!("Session {} not found", id)))?;
    Ok(())
}
