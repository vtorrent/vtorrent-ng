use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn list_torrent_sessions(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<TorrentSessionResponse>>> {
    let sessions = state.torrent_sessions.read().await;
    let result: Vec<TorrentSessionResponse> = sessions
        .list_sessions()
        .iter()
        .map(|s| {
            let summary = s.incentive_summary();
            TorrentSessionResponse {
                id: s.id.clone(),
                name: s.metainfo.name.clone(),
                info_hash: hex::encode(s.metainfo.info_hash),
                state: s.state.to_string(),
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
        .collect();

    Ok(Json(result))
}

pub async fn get_torrent_settings(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<TorrentSettingsResponse>> {
    let seeder = state.torrent_seeder.read().await.clone();
    let (seeding, seeded_torrents) = match seeder {
        Some(seeder) => (true, seeder.len().await),
        None => (false, 0),
    };
    Ok(Json(TorrentSettingsResponse {
        download_dir: state
            .download_dir
            .read()
            .await
            .to_string_lossy()
            .to_string(),
        download_limit_bytes_per_sec: state.torrent_limits.download_rate(),
        upload_limit_bytes_per_sec: state.torrent_limits.upload_rate(),
        seed_ratio_target: *state.seed_ratio_target.read().await,
        seeding,
        seeded_torrents,
    }))
}

pub async fn set_torrent_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TorrentSettingsRequest>,
) -> RpcResult<Json<TorrentSettingsResponse>> {
    let download_dir = req.download_dir.trim();
    if download_dir.is_empty() {
        return Err(RpcError::BadRequest(
            "Download directory is required".into(),
        ));
    }
    std::fs::create_dir_all(download_dir)
        .map_err(|e| RpcError::Internal(format!("Could not create download directory: {e}")))?;
    *state.download_dir.write().await = std::path::PathBuf::from(download_dir);
    state.torrent_limits.set_rates(
        req.download_limit_bytes_per_sec,
        req.upload_limit_bytes_per_sec,
    );
    *state.seed_ratio_target.write().await = req.seed_ratio_target.clamp(0.0, 100.0);
    get_torrent_settings(State(state)).await
}

pub async fn add_torrent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddTorrentRequest>,
) -> RpcResult<Json<AddTorrentResponse>> {
    use base64::Engine as _;
    use vtorrent_torrent::metainfo::{MagnetLink, Metainfo};
    use vtorrent_torrent::session::TorrentSession;

    let metainfo = if req.source_type == "magnet" {
        let magnet =
            MagnetLink::parse(&req.source).map_err(|e| RpcError::BadRequest(e.to_string()))?;
        Metainfo::from_magnet_link(&magnet)
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&req.source)
            .map_err(|_| RpcError::BadRequest("Invalid base64 torrent data".into()))?;
        Metainfo::from_bytes(&bytes).map_err(|e| RpcError::BadRequest(e.to_string()))?
    };

    let download_dir = state.download_dir.read().await.clone();
    if let Err(error) = std::fs::create_dir_all(&download_dir) {
        return Err(RpcError::Internal(format!(
            "Could not create download directory {}: {error}",
            download_dir.display()
        )));
    }

    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo.clone(), req.wallet_address);
    let session_id = state
        .torrent_sessions
        .write()
        .await
        .add_session(session)
        .ok_or_else(|| {
            RpcError::BadRequest(format!(
                "Too many active torrent sessions (max {})",
                vtorrent_torrent::session::MAX_TORRENT_SESSIONS
            ))
        })?;

    // Spawn the download engine for this session.
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&state.torrent_sessions);
    // If inbound seeding is enabled, publish this torrent as soon as the
    // engine completes. The engine flips the session to `Seeding` and returns,
    // so registration has to happen outside it.
    let seeder = state.torrent_seeder.read().await.clone();
    let seeder_info_hash = metainfo.info_hash;
    let seeder_metainfo = metainfo.clone();
    let seeder_session_id = session_id.clone();
    let seeder_download_dir = download_dir.clone();
    let limits = Arc::clone(&state.torrent_limits);
    let seed_ratio_target = Arc::clone(&state.seed_ratio_target);
    let local_wif = state.wallet_wif.read().await.clone();
    let sid = session_id.clone();
    let engine_sessions = Arc::clone(&sessions);
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(
            sid,
            engine_sessions.clone(),
            download_dir.clone(),
            limits,
            seed_ratio_target,
            cancel,
            local_wif,
        )
        .await;
        if let Some(seeder) = seeder {
            let complete = engine_sessions
                .read()
                .await
                .get_session(&seeder_session_id)
                .map(|session| session.is_complete())
                .unwrap_or(false);
            if complete {
                seeder
                    .register(seeder_session_id, seeder_metainfo, seeder_download_dir)
                    .await;
                tracing::info!(
                    info_hash = %hex::encode(seeder_info_hash),
                    "Torrent complete; now serving it to inbound peers"
                );
            }
        }
    });

    Ok(Json(AddTorrentResponse {
        session_id,
        info_hash,
        name,
    }))
}

pub async fn remove_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<Value>> {
    let mut sessions = state.torrent_sessions.write().await;
    let Some(session) = sessions.get_session(&id).ok() else {
        return Err(RpcError::NotFound(format!("Session {} not found", id)));
    };
    if session.has_outstanding_incentives() {
        return Err(RpcError::BadRequest(
            "Session has unpaid or in-flight incentive payments; retry after settlement".into(),
        ));
    }
    let info_hash = session.metainfo.info_hash;
    sessions.remove_session(&id);
    drop(sessions);
    if let Some(cancel) = state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    // Stop serving it immediately, even if the engine task is still winding
    // down: the session it would read from is gone.
    if let Some(seeder) = state.torrent_seeder.read().await.clone() {
        seeder.unregister(&info_hash).await;
    }
    Ok(Json(
        json!({ "success": true, "message": format!("Session {} removed", id) }),
    ))
}
