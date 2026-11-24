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

/// Resolve the wallet address a torrent session will be paid at.
///
/// Receipts are signed with the unlocked hot wallet, so accepting an address
/// the hot key does not own would produce unpayable receipts. When the wallet
/// is locked there is no key to check against, so a caller-supplied address is
/// refused rather than trusted.
fn resolve_torrent_wallet_address(requested: &str, hot: Option<&str>) -> Result<String, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return hot.map(str::to_owned).ok_or_else(|| {
            "wallet is locked or no wallet is imported; unlock/import a wallet before adding a torrent"
                .to_string()
        });
    }
    match hot {
        Some(hot) if requested == hot => Ok(requested.to_owned()),
        Some(_) => Err(
            "wallet_address must match the unlocked hot wallet; unlock that wallet or omit wallet_address"
                .to_string(),
        ),
        None => Err(
            "wallet is locked or no wallet is imported; unlock/import the wallet that owns wallet_address so receipts can be signed"
                .to_string(),
        ),
    }
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
    // Receipts are signed with this wallet and addressed back to it. A locked
    // hot wallet has an address but no signing key; accepting the address would
    // produce receipts the engine can never sign. Require the live key here.
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::BadRequest(
            "wallet is locked or no wallet is imported; unlock the wallet before adding a torrent"
                .into(),
        ));
    }
    let requested_address = req.wallet_address.trim();
    let hot_address = state.wallet_change_address.read().await.clone();
    let wallet_address = resolve_torrent_wallet_address(requested_address, hot_address.as_deref())
        .map_err(RpcError::BadRequest)?;
    vtorrent_core::address::validate_p2pkh(&wallet_address)
        .map_err(|error| RpcError::BadRequest(format!("invalid wallet_address: {error}")))?;
    let live_wif = state.wallet_wif.read().await.clone();
    if live_wif.is_none() {
        return Err(RpcError::BadRequest(
            "wallet signing key is not loaded; unlock the wallet before adding a torrent".into(),
        ));
    }
    let session = TorrentSession::new(metainfo.clone(), wallet_address);
    let session_id = {
        let mut sessions = state.torrent_sessions.write().await;
        // The seeder registry is keyed by info-hash; two sessions for the same
        // torrent would alias one registry entry and make deletion ambiguous.
        let duplicate = sessions
            .list_sessions()
            .iter()
            .any(|existing| existing.metainfo.info_hash == metainfo.info_hash);
        if duplicate {
            return Err(RpcError::BadRequest("Torrent is already added".into()));
        }
        sessions.add_session(session).ok_or_else(|| {
            RpcError::BadRequest(format!(
                "Too many active torrent sessions (max {})",
                vtorrent_torrent::session::MAX_TORRENT_SESSIONS
            ))
        })?
    };

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
            // Read the metainfo *after* the engine: a magnet link starts with
            // an empty stub and is only filled in once BEP-9 metadata arrives.
            // Registering the captured stub would report a seed that serves
            // nothing (and a failed magnet has total_size 0, which would make
            // `is_complete` trivially true).
            let completed = {
                let guard = engine_sessions.read().await;
                guard
                    .get_session(&seeder_session_id)
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
                        seeder_session_id.clone(),
                        metainfo,
                        seeder_download_dir,
                        &engine_sessions,
                    )
                    .await;
                if registered {
                    // A DELETE can land between the completeness check and the
                    // registration. Undo the entry if the session is already gone.
                    if engine_sessions
                        .read()
                        .await
                        .get_session(&seeder_session_id)
                        .is_ok()
                    {
                        tracing::info!(
                            info_hash = %hex::encode(info_hash),
                            "Torrent complete; now serving it to inbound peers"
                        );
                    } else {
                        seeder
                            .unregister_session(&info_hash, &seeder_session_id)
                            .await;
                    }
                } else {
                    tracing::debug!(
                        info_hash = %hex::encode(info_hash),
                        "A newer session is already serving this torrent"
                    );
                }
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
    let removal_guard = state
        .torrent_removal_guard
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    // Snapshot the session without holding a lock across the guard's journal
    // and ledger I/O. The guard may block on disk; holding the sessions write
    // lock there would stall every other torrent operation.
    let snapshot = {
        let sessions = state.torrent_sessions.read().await;
        let session = sessions
            .get_session(&id)
            .map_err(|_| RpcError::NotFound(format!("Session {id} not found")))?;
        session.clone()
    };
    if snapshot.has_outstanding_incentives() {
        return Err(RpcError::BadRequest(
            "Session has unpaid or in-flight incentive payments; retry after settlement".into(),
        ));
    }
    let info_hash = snapshot.metainfo.info_hash;
    if let Some(guard) = removal_guard {
        let result = tokio::task::spawn_blocking(move || guard(&snapshot)).await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(reason)) => return Err(RpcError::BadRequest(reason)),
            Err(error) => {
                return Err(RpcError::Internal(format!(
                    "torrent removal guard failed: {error}"
                )))
            }
        }
    }
    // Re-check after the guard: a receipt may have arrived while the guard was
    // reading. The peer task updates the session account before journaling, so
    // this check sees any receipt that landed during the I/O.
    let mut sessions = state.torrent_sessions.write().await;
    let Some(session) = sessions.get_session(&id).ok() else {
        return Err(RpcError::NotFound(format!("Session {id} not found")));
    };
    if session.has_outstanding_incentives() {
        return Err(RpcError::BadRequest(
            "Session has unpaid or in-flight incentive payments; retry after settlement".into(),
        ));
    }
    sessions.remove_session(&id);
    drop(sessions);
    if let Some(cancel) = state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    // Stop serving it immediately, even if the engine task is still winding
    // down: the session it would read from is gone.
    if let Some(seeder) = state.torrent_seeder.read().await.clone() {
        // Only remove this session's entry; a concurrent re-add may have
        // registered a newer one for the same info-hash.
        seeder.unregister_session(&info_hash, &id).await;
    }
    Ok(Json(
        json!({ "success": true, "message": format!("Session {} removed", id) }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_torrent::incentive::MIN_PAYMENT_SATS;
    use vtorrent_torrent::metainfo::{Metainfo, TorrentFile};
    use vtorrent_torrent::session::TorrentSession;

    fn test_metainfo() -> Metainfo {
        Metainfo {
            info_hash: [0xAB; 20],
            name: "removal-guard-test.iso".into(),
            total_size: 1024 * 1024,
            piece_length: 262144,
            piece_count: 4,
            pieces: Vec::new(),
            announce: None,
            announce_list: Vec::new(),
            files: vec![TorrentFile {
                path: vec!["removal-guard-test.iso".into()],
                length: 1024 * 1024,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    #[test]
    fn torrent_wallet_address_must_match_the_unlocked_hot_wallet() {
        let hot = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        let other = "VH6w62jDRYpYHjR2eJjFzJxRC4MQvvs93a6";
        assert_eq!(resolve_torrent_wallet_address("", Some(hot)).unwrap(), hot);
        assert_eq!(resolve_torrent_wallet_address(hot, Some(hot)).unwrap(), hot);
        assert!(
            resolve_torrent_wallet_address(other, Some(hot)).is_err(),
            "a foreign address would produce receipts the hot key cannot sign"
        );
        assert!(
            resolve_torrent_wallet_address(other, None).is_err(),
            "ownership cannot be checked while the wallet is locked"
        );
        assert!(resolve_torrent_wallet_address("", None).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn receipt_during_the_guard_blocks_removal() {
        let state = Arc::new(AppState::new());
        let session =
            TorrentSession::new(test_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let id = session.id.clone();
        state.torrent_sessions.write().await.add_session(session);

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let started_tx = std::sync::Mutex::new(Some(started_tx));
        let release_rx = std::sync::Mutex::new(Some(release_rx));
        let guard: crate::state::TorrentRemovalGuard = Arc::new(move |_| {
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = release_rx.lock().unwrap().take() {
                let _ = rx.blocking_recv();
            }
            Ok(())
        });
        *state.torrent_removal_guard.lock().unwrap() = Some(guard);

        let removal = {
            let state = Arc::clone(&state);
            let id = id.clone();
            tokio::spawn(async move { remove_torrent(State(state), Path(id)).await })
        };
        started_rx.await.unwrap();

        // A receipt lands while the guard is running. The post-guard re-check
        // must see the new debt and refuse the removal.
        {
            let mut sessions = state.torrent_sessions.write().await;
            let session = sessions.get_session_mut(&id).unwrap();
            session.record_download("VH6w62jDRYpYHjR2eJjFzJxRC4MQvvs93a6", 1);
            session
                .incentive_accounts
                .get_mut("VH6w62jDRYpYHjR2eJjFzJxRC4MQvvs93a6")
                .unwrap()
                .unpaid_satoshis = MIN_PAYMENT_SATS;
        }
        release_tx.send(()).unwrap();

        let result = removal.await.unwrap();
        assert!(
            result.is_err(),
            "debt that arrived during the guard must block removal"
        );
        assert!(
            state.torrent_sessions.read().await.get_session(&id).is_ok(),
            "the session must survive the refused removal"
        );
    }

    #[tokio::test]
    async fn add_torrent_requires_an_unlocked_signing_wallet() {
        let state = Arc::new(AppState::new());
        let address = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        *state.wallet_change_address.write().await = Some(address.into());

        let result = add_torrent(
            State(Arc::clone(&state)),
            Json(AddTorrentRequest {
                source: "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&dn=test"
                    .into(),
                source_type: "magnet".into(),
                wallet_address: address.into(),
            }),
        )
        .await;

        assert!(
            result.is_err(),
            "a locked wallet has no signing key for incentive receipts"
        );
        assert!(state.torrent_sessions.read().await.is_empty());
    }

    #[tokio::test]
    async fn removal_is_refused_when_the_ledger_guard_reports_debt() {
        let state = Arc::new(AppState::new());
        let session =
            TorrentSession::new(test_metainfo(), "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".into());
        let id = session.id.clone();
        state.torrent_sessions.write().await.add_session(session);

        let guard: crate::state::TorrentRemovalGuard =
            Arc::new(|_| Err("outstanding ledger debt".into()));
        *state.torrent_removal_guard.lock().unwrap() = Some(guard);

        let result = remove_torrent(State(Arc::clone(&state)), Path(id.clone())).await;

        assert!(result.is_err(), "the ledger guard must block removal");
        assert!(
            state.torrent_sessions.read().await.get_session(&id).is_ok(),
            "the session must survive a refused removal"
        );
    }
}
