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
            vtr_earned_satoshis: s
                .incentive_accounts
                .values()
                .map(|a| a.total_earned_satoshis)
                .sum(),
            vtr_paid_satoshis: 0,
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
        .add_session(session);

    let cancel = tokio_util::sync::CancellationToken::new();
    handle
        .rpc_state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&handle.rpc_state.torrent_sessions);
    let download_dir = handle.rpc_state.download_dir.read().await.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(sid, sessions, download_dir, cancel).await;
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
