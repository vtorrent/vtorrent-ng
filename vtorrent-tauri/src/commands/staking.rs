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
    pub expected_reward_per_day: u64,
    pub last_stake_time: Option<u64>,
    pub blocks_staked: u64,
}

#[tauri::command]
pub async fn start_staking(
    state: State<'_, AppState>,
    address: String,
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
    let wif = handle.rpc_state.wallet_wif.read().await.clone();

    if let Some(tx) = &handle.rpc_state.staking_control {
        let _ = tx
            .send(vtorrent_node::staking::StakingCommand::Start {
                address: address.clone(),
                wif,
            })
            .await;
    }
    *handle.rpc_state.staking_enabled.write().await = true;
    *handle.rpc_state.staking_address.write().await = Some(address.clone());

    tracing::info!("Staking started for address: {}", address);

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    let chain = handle.rpc_state.chain.lock().await;
    let staking_utxos = chain.get_utxos_for_address(&address);
    let total_staking: u64 = staking_utxos.iter().map(|u| u.value).sum();
    let eligible_count = staking_utxos.len();
    let expected_reward_per_day = if total_staking > 0 {
        ((total_staking as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0) as u64
    } else {
        0
    };
    Ok(StakingStatusResult {
        enabled: true,
        staking_address: Some(address),
        eligible_utxos: eligible_count,
        total_staking_satoshis: total_staking,
        expected_reward_per_day,
        last_stake_time: None,
        blocks_staked,
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

    tracing::info!("Staking stopped");

    let blocks_staked = *handle.rpc_state.blocks_staked.read().await;
    Ok(StakingStatusResult {
        enabled: false,
        staking_address: None,
        eligible_utxos: 0,
        total_staking_satoshis: 0,
        expected_reward_per_day: 0,
        last_stake_time: None,
        blocks_staked,
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

    let (eligible_utxos, total_staking_satoshis, expected_reward_per_day) = if enabled {
        if let Some(ref addr) = staking_address {
            let chain = handle.rpc_state.chain.lock().await;
            let staking_utxos = chain.get_utxos_for_address(addr);
            let total: u64 = staking_utxos.iter().map(|u| u.value).sum();
            let count = staking_utxos.len();
            let daily = if total > 0 {
                ((total as f64 * vtorrent_node::consensus::POS_ANNUAL_RATE) / 365.0) as u64
            } else {
                0
            };
            (count, total, daily)
        } else {
            (0, 0, 0)
        }
    } else {
        (0, 0, 0)
    };

    Ok(StakingStatusResult {
        enabled,
        staking_address,
        eligible_utxos,
        total_staking_satoshis,
        expected_reward_per_day,
        last_stake_time: None,
        blocks_staked,
    })
}
