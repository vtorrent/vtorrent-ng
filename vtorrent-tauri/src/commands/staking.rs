use tauri::State;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

#[derive(Debug, serde::Serialize)]
pub struct StakingStatusResult {
    pub enabled: bool,
    pub staking_address: Option<String>,
    pub eligible_utxos: usize,
    pub total_staking_satoshis: u64,
    pub eligible_staking_satoshis: u64,
    pub immature_utxos: usize,
    pub immature_staking_satoshis: u64,
    pub expired_utxos: usize,
    pub expired_staking_satoshis: u64,
    pub next_maturity_at: Option<u32>,
    pub next_maturity_in_seconds: Option<u64>,
    pub min_stake_age_seconds: u64,
    pub max_stake_age_seconds: u64,
    pub expected_reward_per_day: u64,
    pub last_stake_time: Option<u64>,
    pub blocks_staked: u64,
    pub owned: bool,
    pub wallet_unlocked: bool,
    pub warning: Option<String>,
}

#[tauri::command]
pub async fn start_staking(
    state: State<'_, AppState>,
    address: String,
    staking_wif: Option<String>,
) -> Result<StakingStatusResult> {
    if address.is_empty() {
        return Err(TauriError::NodeError("Staking address is required".into()));
    }
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    if !handle.rpc_state.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    let imported = staking_wif.is_some();
    let wif = match staking_wif {
        Some(wif) => Some(zeroize::Zeroizing::new(wif)),
        None => handle.rpc_state.wallet_wif.read().await.clone(),
    };

    if let Some(tx) = &handle.rpc_state.staking_control {
        // Only report staking as enabled if the command actually reached the
        // staking loop; otherwise the UI shows it active while nothing runs.
        tx.send(vtorrent_node::staking::StakingCommand::Start {
            address: address.clone(),
            wif,
        })
        .await
        .map_err(|_| TauriError::NodeError("Staking loop is not accepting commands".into()))?;
    } else {
        return Err(TauriError::NodeError(
            "Staking control channel is not available".into(),
        ));
    }
    *handle.rpc_state.staking_enabled.write().await = true;
    *handle.rpc_state.staking_address.write().await = Some(address.clone());
    *handle.rpc_state.staking_key_imported.write().await = imported;
    vtorrent_rpc::handlers::staking::persist_staking_intent(
        &handle.rpc_state,
        Some(&address),
        imported,
    )
    .await?;

    tracing::info!("Staking started for address: {}", address);

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    let wallet_address = handle.rpc_state.wallet_change_address.read().await.clone();
    let owned = wallet_address.as_deref() == Some(address.as_str()) || imported;
    let warning = if owned {
        None
    } else {
        Some(
            "Staking address is not owned by the hot wallet — kernels it cannot sign will be rejected by the chain"
                .to_string(),
        )
    };
    let chain = handle.rpc_state.chain.lock().await;
    let now = vtorrent_core::time::now_secs() as u32;
    let snapshot = vtorrent_node::staking::staking_snapshot(&chain, &address, now);
    let expected_reward_per_day = if snapshot.eligible_satoshis > 0 {
        ((snapshot.eligible_satoshis as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0)
            as u64
    } else {
        0
    };
    let last_stake_time = {
        let value = *handle.rpc_state.last_stake_time.read().await;
        (value != 0).then_some(value as u64)
    };
    Ok(StakingStatusResult {
        enabled: true,
        staking_address: Some(address),
        eligible_utxos: snapshot.eligible_utxos,
        total_staking_satoshis: snapshot.total_satoshis,
        eligible_staking_satoshis: snapshot.eligible_satoshis,
        immature_utxos: snapshot.immature_utxos,
        immature_staking_satoshis: snapshot.immature_satoshis,
        expired_utxos: snapshot.expired_utxos,
        expired_staking_satoshis: snapshot.expired_satoshis,
        next_maturity_at: snapshot.next_maturity_at,
        next_maturity_in_seconds: snapshot
            .next_maturity_at
            .map(|at| at.saturating_sub(now) as u64),
        min_stake_age_seconds: snapshot.min_stake_age,
        max_stake_age_seconds: snapshot.max_stake_age,
        expected_reward_per_day,
        last_stake_time,
        blocks_staked,
        owned,
        wallet_unlocked: true,
        warning,
    })
}

#[tauri::command]
pub async fn stop_staking(state: State<'_, AppState>) -> Result<StakingStatusResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    if let Some(tx) = &handle.rpc_state.staking_control {
        let _ = tx.send(vtorrent_node::staking::StakingCommand::Stop).await;
    }
    *handle.rpc_state.staking_enabled.write().await = false;
    *handle.rpc_state.staking_address.write().await = None;
    *handle.rpc_state.staking_key_imported.write().await = false;
    vtorrent_rpc::handlers::staking::persist_staking_intent(&handle.rpc_state, None, false).await?;

    tracing::info!("Staking stopped");

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    let wallet_unlocked = handle.rpc_state.is_wallet_unlocked().await;
    let (min_stake_age, max_stake_age) = {
        let chain = handle.rpc_state.chain.lock().await;
        let params = chain.params();
        (params.min_stake_age, params.max_stake_age)
    };
    Ok(StakingStatusResult {
        enabled: false,
        staking_address: None,
        eligible_utxos: 0,
        total_staking_satoshis: 0,
        eligible_staking_satoshis: 0,
        immature_utxos: 0,
        immature_staking_satoshis: 0,
        expired_utxos: 0,
        expired_staking_satoshis: 0,
        next_maturity_at: None,
        next_maturity_in_seconds: None,
        min_stake_age_seconds: min_stake_age,
        max_stake_age_seconds: max_stake_age,
        expected_reward_per_day: 0,
        last_stake_time: None,
        blocks_staked,
        owned: false,
        wallet_unlocked,
        warning: None,
    })
}

#[tauri::command]
pub async fn get_staking_status(state: State<'_, AppState>) -> Result<StakingStatusResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    let enabled = *handle.rpc_state.staking_enabled.read().await;
    let staking_address = handle.rpc_state.staking_address.read().await.clone();
    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    let wallet_address = handle.rpc_state.wallet_change_address.read().await.clone();
    let imported = *handle.rpc_state.staking_key_imported.read().await;
    let owned = staking_address
        .as_deref()
        .is_some_and(|address| wallet_address.as_deref() == Some(address))
        || imported;
    let warning = if enabled && !owned {
        Some(
            "Staking address is not owned by the hot wallet — kernels it cannot sign will be rejected by the chain"
                .to_string(),
        )
    } else {
        None
    };

    let wallet_unlocked = handle.rpc_state.is_wallet_unlocked().await;
    let snapshot_address = staking_address.as_deref().or(wallet_address.as_deref());
    let now = vtorrent_core::time::now_secs() as u32;
    let snapshot = if let Some(address) = snapshot_address {
        let chain = handle.rpc_state.chain.lock().await;
        vtorrent_node::staking::staking_snapshot(&chain, address, now)
    } else {
        vtorrent_node::staking::StakingSnapshot::default()
    };
    let expected_reward_per_day = if enabled && snapshot.eligible_satoshis > 0 {
        ((snapshot.eligible_satoshis as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0)
            as u64
    } else {
        0
    };
    let last_stake_time = {
        let value = *handle.rpc_state.last_stake_time.read().await;
        (value != 0).then_some(value as u64)
    };

    Ok(StakingStatusResult {
        enabled,
        staking_address,
        eligible_utxos: snapshot.eligible_utxos,
        total_staking_satoshis: snapshot.total_satoshis,
        eligible_staking_satoshis: snapshot.eligible_satoshis,
        immature_utxos: snapshot.immature_utxos,
        immature_staking_satoshis: snapshot.immature_satoshis,
        expired_utxos: snapshot.expired_utxos,
        expired_staking_satoshis: snapshot.expired_satoshis,
        next_maturity_at: snapshot.next_maturity_at,
        next_maturity_in_seconds: snapshot
            .next_maturity_at
            .map(|at| at.saturating_sub(now) as u64),
        min_stake_age_seconds: snapshot.min_stake_age,
        max_stake_age_seconds: snapshot.max_stake_age,
        expected_reward_per_day,
        last_stake_time,
        blocks_staked,
        owned,
        wallet_unlocked,
        warning,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StakingRewardEntry {
    pub height: u64,
    pub timestamp: u32,
    pub block_hash: String,
    pub reward_sats: u64,
    pub staker_address: Option<String>,
}

// Bound total blocks scanned per request; response may truncate under a non-matching filter.
const MAX_SCAN_BLOCKS: u32 = 5_000;

#[tauri::command]
pub async fn get_staking_rewards(
    state: State<'_, AppState>,
    limit: Option<u64>,
    address: Option<String>,
) -> Result<Vec<StakingRewardEntry>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let chain = handle.rpc_state.chain.lock().await;
    let mut out = Vec::new();
    let mut height = chain.best_height();
    let mut scanned: u32 = 0;
    while out.len() < limit as usize {
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
            // Reward = total outputs minus the staked principal (a coinstake
            // returns `stake + reward`; summing outputs overstates it).
            let reward_sats: u64 = chain.coinstake_reward(coinstake).unwrap_or(0);
            let staker_address = coinstake
                .outputs
                .iter()
                .filter_map(|o| p2pkh_to_address(&o.script_pubkey))
                .next();
            if address
                .as_ref()
                .is_none_or(|a| staker_address.as_ref() == Some(a))
            {
                out.push(StakingRewardEntry {
                    height: height as u64,
                    timestamp: block.header.timestamp,
                    block_hash: chain
                        .block_hash_at_height(height)
                        .map(hex::encode)
                        .unwrap_or_default(),
                    reward_sats,
                    staker_address,
                });
            }
        }
        if height == 0 {
            break;
        }
        height -= 1;
    }
    Ok(out)
}

fn p2pkh_to_address(script: &[u8]) -> Option<String> {
    if script.len() != 25
        || script[0] != 0x76
        || script[1] != 0xa9
        || script[2] != 0x14
        || script[23] != 0x88
        || script[24] != 0xac
    {
        return None;
    }
    vtorrent_core::address::Address::from_hash160(
        &script[3..23],
        vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX,
    )
    .ok()
    .map(|a| a.to_string())
}
