use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

async fn persist_staking_intent(state: &AppState, address: Option<&str>) -> RpcResult<()> {
    let Some(path) = &state.staking_state_path else {
        return Ok(());
    };
    let path = path.clone();
    let address = address.map(|s| s.to_string());
    // File I/O on the async runtime stalls all RPC processing on a slow
    // disk; run on the blocking pool instead.
    tokio::task::spawn_blocking(move || match address {
        Some(addr) => {
            let blob = serde_json::json!({ "enabled": true, "address": addr });
            let bytes = serde_json::to_vec_pretty(&blob).map_err(|e| {
                RpcError::Internal(format!("Staking intent serialize failed: {}", e))
            })?;
            // Atomic write: a crash mid-write would otherwise leave a
            // truncated staking.json that fails to parse on resume.
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, &bytes)
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(|e| {
                    RpcError::Internal(format!(
                        "Could not persist staking intent to {}: {}",
                        path.display(),
                        e
                    ))
                })
        }
        None => {
            // Removal is best-effort: a missing file is the desired state.
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(RpcError::Internal(format!(
                        "Could not remove staking intent {}: {}",
                        path.display(),
                        e
                    )));
                }
            }
            Ok(())
        }
    })
    .await
    .map_err(|e| RpcError::Internal(format!("Staking persist task panicked: {}", e)))?
}

pub async fn get_staking_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<StakingStatusResponse>> {
    let enabled = *state.staking_enabled.read().await;
    let staking_address = state.staking_address.read().await.clone();
    let blocks_staked = *state.blocks_staked.read().await;
    let last_stake_time_raw = *state.last_stake_time.read().await;
    let chain = state.chain.lock().await;

    // Sum only the staking address's UTXOs, not the entire network UTXO set.
    let staking_utxos: Vec<vtorrent_node::chain::Utxo> = staking_address
        .as_ref()
        .map(|addr| chain.get_utxos_for_address(addr))
        .unwrap_or_default();
    let total_staking: u64 = staking_utxos.iter().map(|u| u.value).sum();
    let eligible_utxos = staking_utxos.len();

    let expected_per_day = if enabled {
        total_staking as f64 * 0.05 / 365.0
    } else {
        0.0
    };

    Ok(Json(StakingStatusResponse {
        enabled,
        staking_address,
        eligible_utxos,
        total_staking_satoshis: total_staking,
        expected_reward_per_day: expected_per_day,
        last_stake_time: if last_stake_time_raw == 0 {
            None
        } else {
            Some(last_stake_time_raw)
        },
        blocks_staked,
    }))
}

pub async fn start_staking(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StakingStartRequest>,
) -> RpcResult<Json<Value>> {
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.address.is_empty() {
        return Err(RpcError::BadRequest("Staking address is required".into()));
    }

    // Sign the coinstake with the unlocked hot-wallet key. If the requested
    // staking address is not owned by the hot wallet, coinstake signatures
    // will be rejected by the chain.
    let wif = state.wallet_wif.read().await.clone();

    if let Some(tx) = &state.staking_control {
        let _ = tx
            .send(vtorrent_node::staking::StakingCommand::Start {
                address: req.address.clone(),
                wif,
            })
            .await;
    }

    *state.staking_enabled.write().await = true;
    *state.staking_address.write().await = Some(req.address.clone());
    persist_staking_intent(&state, Some(&req.address)).await?;

    Ok(Json(json!({
        "success": true,
        "message": format!("Staking started for address {}", req.address)
    })))
}

pub async fn stop_staking(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    if let Some(tx) = &state.staking_control {
        let _ = tx.send(vtorrent_node::staking::StakingCommand::Stop).await;
    }
    *state.staking_enabled.write().await = false;
    *state.staking_address.write().await = None;
    persist_staking_intent(&state, None).await?;
    Ok(Json(
        json!({ "success": true, "message": "Staking stopped" }),
    ))
}
