use serde::Serialize;
use std::sync::Arc;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};
use vtorrent_torrent::session::TorrentSession;

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
    /// Whether this node accepts inbound torrent connections.
    pub seeding: bool,
    /// Completed torrents currently served to inbound peers.
    pub seeded_torrents: usize,
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
    let seeder = handle.rpc_state.torrent_seeder.read().await.clone();
    let (seeding, seeded_torrents) = match seeder {
        Some(seeder) => (true, seeder.len().await),
        None => (false, 0),
    };
    Ok(TorrentSettingsResult {
        download_dir,
        download_limit_bytes_per_sec,
        upload_limit_bytes_per_sec,
        seed_ratio_target,
        seeding,
        seeded_torrents,
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
        .map(|s| {
            let summary = s.incentive_summary();
            TorrentResult {
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
                vtr_earned_satoshis: summary.total_earned_satoshis,
                vtr_paid_satoshis: summary.total_paid_satoshis,
                vtr_pending_earned_satoshis: summary.pending_earned_satoshis,
                vtr_pending_owed_satoshis: summary.pending_owed_satoshis,
                vtr_settled_earned_satoshis: summary.total_earned_satoshis,
                vtr_settled_paid_satoshis: summary.total_paid_satoshis,
            }
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

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let requested_address = wallet_address.trim();
    let wallet_address = if requested_address.is_empty() {
        handle
            .rpc_state
            .wallet_change_address
            .read()
            .await
            .clone()
            .unwrap_or_default()
    } else {
        requested_address.to_owned()
    };
    vtorrent_core::address::validate_p2pkh(&wallet_address)
        .map_err(|error| TauriError::InvalidInput(format!("invalid wallet_address: {error}")))?;
    let session = TorrentSession::new(metainfo, wallet_address);
    let session_id = {
        let mut sessions = handle.rpc_state.torrent_sessions.write().await;
        let duplicate = sessions
            .list_sessions()
            .iter()
            .any(|existing| hex::encode(existing.metainfo.info_hash) == info_hash);
        if duplicate {
            return Err(TauriError::InvalidInput("Torrent is already added".into()));
        }
        sessions.add_session(session).ok_or_else(|| {
            TauriError::InvalidInput(format!(
                "Too many active torrent sessions (max {})",
                vtorrent_torrent::session::MAX_TORRENT_SESSIONS
            ))
        })?
    };

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
    let local_wif = handle.rpc_state.wallet_wif.read().await.clone();
    let sid = session_id.clone();
    // Same registration as the daemon: a completed torrent becomes servable
    // when the operator enabled inbound seeding for this node.
    let seeder = handle.rpc_state.torrent_seeder.read().await.clone();
    let seeder_download_dir = download_dir.clone();
    let engine_sessions = Arc::clone(&sessions);
    let engine_session_id = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(
            sid.clone(),
            engine_sessions.clone(),
            download_dir,
            limits,
            seed_ratio_target,
            cancel,
            local_wif,
        )
        .await;
        if let Some(seeder) = seeder {
            // A magnet link starts with a stub metainfo and is only filled in
            // by the engine's BEP-9 fetch. Register the session's current
            // metainfo, not the copy captured before the engine ran.
            let completed = {
                let guard = engine_sessions.read().await;
                guard
                    .get_session(&engine_session_id)
                    .ok()
                    .and_then(|session| {
                        let metainfo = &session.metainfo;
                        let usable = session.is_complete()
                            && metainfo.total_size > 0
                            && metainfo.piece_count > 0
                            && metainfo.pieces.len() == metainfo.piece_count as usize;
                        usable.then(|| metainfo.clone())
                    })
            };
            if let Some(metainfo) = completed {
                let info_hash = metainfo.info_hash;
                let registered = seeder
                    .register_if_live(
                        engine_session_id.clone(),
                        metainfo,
                        seeder_download_dir,
                        &engine_sessions,
                    )
                    .await;
                if registered
                    && engine_sessions
                        .read()
                        .await
                        .get_session(&engine_session_id)
                        .is_err()
                {
                    seeder
                        .unregister_session(&info_hash, &engine_session_id)
                        .await;
                }
            }
        }
    });

    Ok(AddTorrentResult {
        session_id,
        info_hash,
        name,
    })
}

fn ensure_torrent_removable(session: &TorrentSession) -> Result<()> {
    if session.has_outstanding_incentives() {
        return Err(TauriError::Torrent(
            "Session has unpaid or in-flight incentive payments; settle them before removal".into(),
        ));
    }
    Ok(())
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
    let info_hash = {
        let mut sessions = handle.rpc_state.torrent_sessions.write().await;
        let session = sessions
            .get_session(&id)
            .map_err(|_| TauriError::Torrent(format!("Session {} not found", id)))?;
        // The desktop node has no settlement/payment loop. Refuse removal
        // rather than silently discarding a receipt-backed debt; the user
        // must settle or explicitly write it off first.
        ensure_torrent_removable(session)?;
        let info_hash = session.metainfo.info_hash;
        sessions
            .remove_session(&id)
            .ok_or_else(|| TauriError::Torrent(format!("Session {} not found", id)))?;
        info_hash
    };
    // Stop serving immediately, exactly like the RPC path.
    if let Some(seeder) = handle.rpc_state.torrent_seeder.read().await.clone() {
        seeder.unregister_session(&info_hash, &id).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_torrent::incentive::MIN_PAYMENT_SATS;
    use vtorrent_torrent::metainfo::{Metainfo, TorrentFile};

    fn test_metainfo() -> Metainfo {
        Metainfo {
            info_hash: [0xCD; 20],
            name: "desktop-removal-guard.iso".into(),
            total_size: 1024 * 1024,
            piece_length: 262144,
            piece_count: 4,
            pieces: Vec::new(),
            announce: None,
            announce_list: Vec::new(),
            files: vec![TorrentFile {
                path: vec!["desktop-removal-guard.iso".into()],
                length: 1024 * 1024,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    #[test]
    fn a_payable_account_blocks_removal() {
        let mut session =
            TorrentSession::new(test_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        session.record_download("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6", 1);
        session
            .incentive_accounts
            .get_mut("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6")
            .unwrap()
            .unpaid_satoshis = MIN_PAYMENT_SATS;
        assert!(ensure_torrent_removable(&session).is_err());
    }

    #[test]
    fn dust_only_account_is_removable() {
        let mut session =
            TorrentSession::new(test_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        session.record_download("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6", 1);
        session
            .incentive_accounts
            .get_mut("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6")
            .unwrap()
            .unpaid_satoshis = MIN_PAYMENT_SATS - 1;
        assert!(ensure_torrent_removable(&session).is_ok());
    }
}
