use serde::Serialize;
use std::sync::Arc;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct NodeInfoResult {
    pub running: bool,
    pub version: String,
    pub network: String,
    pub block_height: u64,
    pub best_hash: String,
    pub connections: usize,
    pub syncing: bool,
    pub sync_percent: f64,
    pub mempool_size: usize,
    pub uptime_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct TxResult {
    pub txid: String,
    pub block_height: u32,
    pub confirmations: u32,
    pub timestamp: u32,
    pub tx_type: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: u64,
    pub display: String,
}

#[tauri::command]
pub async fn start_node(state: tauri::State<'_, AppState>) -> Result<NodeInfoResult> {
    use crate::state::NodeHandle;
    use vtorrent_node::node::{Node, NodeConfig};
    use vtorrent_rpc::state::AppState as RpcAppState;

    {
        let guard = state.node.lock().await;
        if guard.is_some() {
            drop(guard);
            return get_node_info(state).await;
        }
    }

    let config = NodeConfig::default();
    let mut node = Node::new(config).map_err(|e| TauriError::NodeError(e.to_string()))?;

    let mut rpc_state = RpcAppState::new_with_shared(node.chain_arc(), node.mempool_arc());

    let btc_seed = {
        use sha2::{Digest, Sha512};
        let wallet_guard = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
        wallet_guard.as_ref().and_then(|wallet| {
            wallet
                .mnemonic()
                .and_then(|m| vtorrent_wallet::hd::Mnemonic::from_phrase(m).ok())
                .and_then(|m| m.to_seed().ok())
                .or_else(|| {
                    let wif = wallet.get_default_wif()?;
                    let mut hasher = Sha512::new();
                    hasher.update(b"vtorrent-ng-btc-seed-v1");
                    hasher.update(wif.as_bytes());
                    Some(hasher.finalize().into())
                })
        })
    };
    if let Some(seed) = btc_seed {
        let utxo_path = {
            let wp = state
                .wallet_path
                .lock()
                .map_err(|_| TauriError::WalletLocked)?;
            wp.as_ref()
                .map(|p| {
                    p.parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join("btc_utxos.json")
                })
                .unwrap_or_else(|| std::env::temp_dir().join("vtorrent_btc_utxos.json"))
        };
        match vtorrent_btc::wallet::BtcWallet::with_persistence(
            seed,
            bitcoin::Network::Bitcoin,
            utxo_path.clone(),
        ) {
            Ok(mut wallet) => {
                wallet.set_last_scanned_height(wallet.best_height());
                *rpc_state.btc_wallet.write().await = Some(wallet);
                tracing::info!(
                    "BTC wallet loaded with UTXO persistence: {}",
                    utxo_path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load BTC UTXOs from {}: {}, using fresh wallet",
                    utxo_path.display(),
                    e
                );
                *rpc_state.btc_wallet.write().await =
                    Some(vtorrent_btc::wallet::BtcWallet::new(seed));
            }
        }
    }

    let (event_tx, mut node_rx) = vtorrent_node::events::channel(1024);
    node.set_event_sender(event_tx);

    rpc_state.staking_control = Some(node.staking_control());

    let handle = NodeHandle {
        rpc_state: rpc_state.clone(),
        start_time: std::time::Instant::now(),
    };
    *state.node.lock().await = Some(handle);

    {
        let blocks_staked = Arc::clone(&rpc_state.blocks_staked);
        let last_stake_time = Arc::clone(&rpc_state.last_stake_time);
        let rewards_earned = Arc::clone(&rpc_state.rewards_earned_sats);
        tokio::spawn(async move {
            loop {
                match node_rx.recv().await {
                    Ok(event) => {
                        if let vtorrent_node::events::NodeEvent::StakingReward {
                            reward_sats, ..
                        } = &*event
                        {
                            *blocks_staked.write().await += 1;
                            *last_stake_time.write().await = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                as u32;
                            let current = *rewards_earned.read().await;
                            *rewards_earned.write().await = current.saturating_add(*reward_sats);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    tokio::spawn(async move {
        if let Err(e) = node.start().await {
            tracing::error!("Node stopped with error: {}", e);
        }
    });

    tracing::info!("vTorrent node started in background");
    get_node_info(state).await
}

#[tauri::command]
pub async fn get_node_info(state: tauri::State<'_, AppState>) -> Result<NodeInfoResult> {
    let guard = state.node.lock().await;
    match &*guard {
        None => Ok(NodeInfoResult {
            running: false,
            version: env!("CARGO_PKG_VERSION").into(),
            network: "vtorrent-mainnet".into(),
            block_height: 0,
            best_hash: String::new(),
            connections: 0,
            syncing: false,
            sync_percent: 0.0,
            mempool_size: 0,
            uptime_secs: 0,
        }),
        Some(handle) => {
            let rpc = &handle.rpc_state;
            let chain = rpc.chain.lock().await;
            let mempool = rpc.mempool.lock().await;
            let peer_count = *rpc.peer_count.read().await;
            let syncing = *rpc.syncing.read().await;
            let uptime = handle.start_time.elapsed().as_secs();
            let height = chain.best_height();
            let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();
            let mempool_size = mempool.size();
            drop(chain);
            drop(mempool);
            // Progress is measured against the best height advertised by our
            // peers. Using our own height as the denominator always yields
            // 100% — the bug this replaces.
            let best_peer_height = *rpc.best_peer_height.read().await;
            let sync_pct = if syncing {
                let target = best_peer_height.max(1) as f64;
                ((height as f64 / target) * 100.0).min(100.0)
            } else {
                100.0
            };
            Ok(NodeInfoResult {
                running: true,
                version: env!("CARGO_PKG_VERSION").into(),
                block_height: height as u64,
                best_hash,
                connections: peer_count,
                syncing,
                sync_percent: sync_pct,
                mempool_size,
                uptime_secs: uptime,
                network: rpc.network.clone(),
            })
        }
    }
}

#[tauri::command]
pub async fn get_transactions(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<TxResult>> {
    let addresses: Vec<String> = state
        .wallet
        .lock()
        .map_err(|_| TauriError::WalletLocked)?
        .as_ref()
        .ok_or(TauriError::WalletNotInitialized)?
        .list_addresses()
        .into_iter()
        .map(|(address, _, _, _)| address)
        .collect();
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let chain = handle.rpc_state.chain.lock().await;
    let limit = limit.unwrap_or(50);
    let txs = chain.get_recent_transactions_for_addresses(&addresses, limit);
    let best = chain.best_height();
    Ok(txs
        .into_iter()
        .map(|(txid, height, ts, dir, amount, fee)| {
            let display = format!(
                "{} {} VTR",
                match dir.as_str() {
                    "receive" => "Received",
                    "stake" => "Staked",
                    _ => "Sent",
                },
                amount as f64 / 100_000_000.0
            );
            TxResult {
                txid,
                block_height: height,
                confirmations: best.saturating_sub(height).saturating_add(1),
                timestamp: ts,
                tx_type: dir,
                amount_satoshis: amount,
                fee_satoshis: fee,
                display,
            }
        })
        .collect())
}
