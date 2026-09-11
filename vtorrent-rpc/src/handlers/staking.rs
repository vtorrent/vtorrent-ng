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

    // The hot wallet holds a single key: refuse to stake to a foreign
    // address whose kernels our signatures could never satisfy.
    if let Some(owned) = state.wallet_change_address.read().await.clone() {
        if req.address != owned {
            return Err(RpcError::BadRequest(
                "Staking address is not owned by the hot wallet".into(),
            ));
        }
    }

    // Sign the coinstake with the unlocked hot-wallet key. If the requested
    // staking address is not owned by the hot wallet, coinstake signatures
    // will be rejected by the chain.
    let wif = state.wallet_wif.read().await.clone();

    match &state.staking_control {
        Some(tx) => {
            tx.send(vtorrent_node::staking::StakingCommand::Start {
                address: req.address.clone(),
                wif,
            })
            .await
            .map_err(|_| {
                RpcError::Internal("Staking engine unavailable — restart the node".into())
            })?;
        }
        None => {
            return Err(RpcError::Internal(
                "Staking engine unavailable — restart the node".into(),
            ));
        }
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

pub(crate) fn clamp_rewards_limit(limit: Option<u64>) -> u64 {
    match limit {
        None => 20,
        Some(n) => n.clamp(1, 100),
    }
}

pub(crate) fn p2pkh_script_to_address(script: &[u8]) -> Option<String> {
    if script.len() != 25
        || script[0] != 0x76
        || script[1] != 0xa9
        || script[2] != 0x14
        || script[23] != 0x88
        || script[24] != 0xac
    {
        return None;
    }
    let addr = vtorrent_core::address::Address::from_hash160(
        &script[3..23],
        vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX,
    )
    .ok()?;
    Some(addr.to_string())
}

pub(crate) fn coinstake_reward(tx: &vtorrent_node::block::Transaction) -> Option<u64> {
    if tx.tx_type != vtorrent_node::block::TxType::Coinstake {
        return None;
    }
    Some(tx.outputs.iter().map(|o| o.value).sum())
}

pub(crate) fn reward_matches_filter(staker_address: Option<&str>, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => staker_address == Some(f),
    }
}

// Bound total blocks scanned per request; response may truncate under a non-matching filter.
pub(crate) const MAX_SCAN_BLOCKS: u32 = 5_000;

pub async fn get_staking_rewards(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<StakingRewardsQuery>,
) -> RpcResult<Json<StakingRewardsResponse>> {
    let limit = clamp_rewards_limit(query.limit);
    let chain = state.chain.lock().await;
    let tip = chain.best_height();
    let mut rewards = Vec::new();
    let mut height = tip;
    let mut scanned: u32 = 0;
    while rewards.len() < limit as usize {
        if scanned >= MAX_SCAN_BLOCKS {
            break;
        }
        let Some(block) = chain.get_block_at_height(height) else {
            break;
        };
        scanned += 1;
        if let Some(coinstake) = block
            .transactions
            .iter()
            .find(|tx| tx.tx_type == vtorrent_node::block::TxType::Coinstake)
        {
            if let Some(reward_sats) = coinstake_reward(coinstake) {
                let staker_address = coinstake
                    .outputs
                    .iter()
                    .filter_map(|o| p2pkh_script_to_address(&o.script_pubkey))
                    .next();
                if reward_matches_filter(staker_address.as_deref(), query.address.as_deref()) {
                    let hash = chain
                        .block_hash_at_height(height)
                        .map(hex::encode)
                        .unwrap_or_default();
                    rewards.push(StakingRewardItem {
                        height: height as u64,
                        timestamp: block.header.timestamp,
                        block_hash: hash,
                        reward_sats,
                        staker_address,
                    });
                }
            }
        }
        if height == 0 {
            break;
        }
        height -= 1;
    }
    Ok(Json(StakingRewardsResponse {
        tip_height: tip as u64,
        rewards,
    }))
}

#[cfg(test)]
mod rewards_tests {
    use super::*;

    fn p2pkh_script(hash: &[u8; 20]) -> Vec<u8> {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(hash);
        s.push(0x88);
        s.push(0xac);
        s
    }

    #[test]
    fn test_reward_matches_filter() {
        assert!(reward_matches_filter(Some("Vabc"), None));
        assert!(reward_matches_filter(None, None));
        assert!(reward_matches_filter(Some("Vabc"), Some("Vabc")));
        assert!(!reward_matches_filter(Some("Vabc"), Some("Vother")));
        assert!(!reward_matches_filter(None, Some("Vabc")));
    }

    #[test]
    fn test_clamp_rewards_limit() {
        assert_eq!(clamp_rewards_limit(None), 20);
        assert_eq!(clamp_rewards_limit(Some(0)), 1);
        assert_eq!(clamp_rewards_limit(Some(5)), 5);
        assert_eq!(clamp_rewards_limit(Some(101)), 100);
    }

    #[test]
    fn test_p2pkh_script_to_address_roundtrip() {
        let hash = [0x11u8; 20];
        let addr = p2pkh_script_to_address(&p2pkh_script(&hash)).unwrap();
        assert!(addr.starts_with('V'));
        assert!(p2pkh_script_to_address(&[0x00, 0x01]).is_none());
        assert!(p2pkh_script_to_address(&[0x00; 25]).is_none());
    }

    #[test]
    fn test_coinstake_reward_sums_outputs() {
        let tx = vtorrent_node::block::Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::Coinstake,
            inputs: vec![],
            outputs: vec![
                vtorrent_node::block::TxOutput {
                    value: 0,
                    script_pubkey: vec![],
                },
                vtorrent_node::block::TxOutput {
                    value: 50_000,
                    script_pubkey: vec![0x76],
                },
            ],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(coinstake_reward(&tx), Some(50_000));
        let std_tx = vtorrent_node::block::Transaction {
            version: 1,
            tx_type: vtorrent_node::block::TxType::Standard,
            inputs: vec![],
            outputs: vec![vtorrent_node::block::TxOutput {
                value: 50_000,
                script_pubkey: vec![0x76],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(coinstake_reward(&std_tx), None);
    }
}
