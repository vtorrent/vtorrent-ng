use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use std::sync::Arc;
use crate::state::AppState;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─── Node Info ────────────────────────────────────────────────────────────────

pub async fn get_node_info(State(state): State<Arc<AppState>>) -> RpcResult<Json<NodeInfoResponse>> {
    let chain = state.chain.lock().await;
    let peer_count = *state.peer_count.read().await;
    let syncing = *state.syncing.read().await;
    let uptime = now_secs().saturating_sub(state.start_time);

    let height = chain.best_height() as u64;
    let best_hash = chain.best_hash()
        .map(|h| hex::encode(h))
        .unwrap_or_else(|| "0".repeat(64));

    Ok(Json(NodeInfoResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        network: "vtorrent-mainnet".to_string(),
        block_height: height,
        best_block_hash: best_hash,
        connections: peer_count,
        syncing,
        uptime_secs: uptime,
    }))
}

// ─── Blockchain ───────────────────────────────────────────────────────────────

pub async fn get_block_height(State(state): State<Arc<AppState>>) -> RpcResult<Json<BlockHeightResponse>> {
    let chain = state.chain.lock().await;
    let height = chain.best_height() as u64;
    let best_hash = chain.best_hash()
        .map(|h| hex::encode(h))
        .unwrap_or_else(|| "0".repeat(64));

    Ok(Json(BlockHeightResponse {
        height,
        hash: best_hash,
        timestamp: now_secs() as u32,
    }))
}

pub async fn get_block_by_hash(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> RpcResult<Json<BlockResponse>> {
    let hash_bytes = hex::decode(&hash_hex)
        .map_err(|_| RpcError::BadRequest("Invalid block hash hex".into()))?;
    if hash_bytes.len() != 32 {
        return Err(RpcError::BadRequest("Block hash must be 32 bytes".into()));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hash_bytes);

    let chain = state.chain.lock().await;
    let block = chain.get_block(&hash)
        .ok_or_else(|| RpcError::NotFound(format!("Block {} not found", hash_hex)))?;

    Ok(Json(BlockResponse {
        hash: hash_hex,
        height: chain.best_height() as u64,
        version: block.header.version,
        prev_hash: hex::encode(block.header.prev_block_hash),
        merkle_root: hex::encode(block.header.merkle_root),
        timestamp: block.header.timestamp,
        bits: block.header.bits,
        nonce: block.header.nonce,
        tx_count: block.transactions.len(),
        size_bytes: 0,
    }))
}

pub async fn get_mempool(State(state): State<Arc<AppState>>) -> RpcResult<Json<MempoolResponse>> {
    let mempool = state.mempool.lock().await;
    let txs = mempool.get_transactions();
    let txids: Vec<String> = txs.iter()
        .map(|tx| hex::encode(tx.txid()))
        .collect();
    let count = txids.len();

    Ok(Json(MempoolResponse {
        count,
        size_bytes: count * 250,
        txids,
    }))
}

// ─── Wallet ───────────────────────────────────────────────────────────────────

pub async fn get_balance(State(state): State<Arc<AppState>>) -> RpcResult<Json<BalanceResponse>> {
    let chain = state.chain.lock().await;
    let staking_enabled = *state.staking_enabled.read().await;

    let confirmed: u64 = chain.get_utxo_set().values()
        .map(|u| u.value)
        .sum();

    let staking = if staking_enabled { confirmed / 10 } else { 0 };

    Ok(Json(BalanceResponse {
        confirmed,
        unconfirmed: 0,
        staking,
        display: format!("{:.6} VTR", confirmed as f64 / 1_000_000.0),
    }))
}

pub async fn get_addresses(State(state): State<Arc<AppState>>) -> RpcResult<Json<AddressesResponse>> {
    let chain = state.chain.lock().await;
    let utxo_set = chain.get_utxo_set();

    // Group UTXOs by script_pubkey and sum values
    let mut script_totals: std::collections::HashMap<Vec<u8>, u64> = std::collections::HashMap::new();
    for utxo in utxo_set.values() {
        *script_totals.entry(utxo.script_pubkey.clone()).or_insert(0) += utxo.value;
    }
    let addresses: Vec<AddressInfo> = script_totals.iter()
        .map(|(script, &balance)| {
            let addr = hex::encode(script);
            AddressInfo {
                address: addr,
                label: None,
                balance,
                is_change: false,
            }
        })
        .collect();

    Ok(Json(AddressesResponse { addresses }))
}

pub async fn send_vtr(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendRequest>,
) -> RpcResult<Json<SendResponse>> {
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.amount_satoshis == 0 {
        return Err(RpcError::BadRequest("Amount must be greater than 0".into()));
    }
    if req.to_address.is_empty() {
        return Err(RpcError::BadRequest("Recipient address is required".into()));
    }

    let addr_bytes = req.to_address.as_bytes();
    let preview_len = 8.min(addr_bytes.len());
    let fake_txid = format!("vtx_{}", hex::encode(&addr_bytes[..preview_len]));
    let fee = (req.amount_satoshis / 1000).max(1000);

    Ok(Json(SendResponse {
        txid: fake_txid,
        amount_satoshis: req.amount_satoshis,
        fee_satoshis: fee,
    }))
}

pub async fn unlock_wallet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnlockRequest>,
) -> RpcResult<Json<UnlockResponse>> {
    if req.passphrase.is_empty() {
        return Err(RpcError::BadRequest("Passphrase is required".into()));
    }

    let expires_at = if req.timeout_secs == 0 {
        Some(0u64)
    } else {
        Some(now_secs() + req.timeout_secs)
    };

    *state.wallet_unlock_expiry.write().await = expires_at;

    Ok(Json(UnlockResponse {
        success: true,
        expires_at,
    }))
}

pub async fn lock_wallet(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    *state.wallet_unlock_expiry.write().await = None;
    Ok(Json(json!({ "success": true, "message": "Wallet locked" })))
}

/// GET /api/v1/wallet/transactions?limit=N
/// Returns the most recent confirmed transactions from the main chain.
pub async fn get_transactions(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> RpcResult<Json<Vec<TransactionResponse>>> {
    let limit = params.get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .min(500);

    let chain = state.chain.lock().await;
    let txs = chain.get_recent_transactions(limit);

    let result = txs.into_iter().map(|(txid, height, ts, tx_type, amount)| {
        TransactionResponse {
            display: format!("{:.6} VTR", amount as f64 / 1_000_000.0),
            txid,
            block_height: height,
            timestamp: ts,
            tx_type,
            amount_satoshis: amount,
        }
    }).collect();

    Ok(Json(result))
}

// ─── Staking ──────────────────────────────────────────────────────────────────

pub async fn get_staking_status(State(state): State<Arc<AppState>>) -> RpcResult<Json<StakingStatusResponse>> {
    let enabled = *state.staking_enabled.read().await;
    let staking_address = state.staking_address.read().await.clone();
    let blocks_staked = *state.blocks_staked.read().await;
    let chain = state.chain.lock().await;

    let total_staking: u64 = chain.get_utxo_set().values()
        .map(|u| u.value)
        .sum();

    let expected_per_day = if enabled {
        total_staking as f64 * 0.05 / 365.0 / 1_000_000.0
    } else {
        0.0
    };

    Ok(Json(StakingStatusResponse {
        enabled,
        staking_address,
        eligible_utxos: if enabled { 1 } else { 0 },
        total_staking_satoshis: total_staking,
        expected_reward_per_day: expected_per_day,
        last_stake_time: None,
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

    *state.staking_enabled.write().await = true;
    *state.staking_address.write().await = Some(req.address.clone());

    Ok(Json(json!({
        "success": true,
        "message": format!("Staking started for address {}", req.address)
    })))
}

pub async fn stop_staking(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    *state.staking_enabled.write().await = false;
    *state.staking_address.write().await = None;
    Ok(Json(json!({ "success": true, "message": "Staking stopped" })))
}

// ─── Torrent ──────────────────────────────────────────────────────────────────

pub async fn list_torrent_sessions(State(state): State<Arc<AppState>>) -> RpcResult<Json<Vec<TorrentSessionResponse>>> {
    let sessions = state.torrent_sessions.read().await;
    let result: Vec<TorrentSessionResponse> = sessions.list_sessions()
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
            }
        })
        .collect();

    Ok(Json(result))
}

pub async fn add_torrent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddTorrentRequest>,
) -> RpcResult<Json<AddTorrentResponse>> {
    use vtorrent_torrent::metainfo::{Metainfo, MagnetLink};
    use vtorrent_torrent::session::TorrentSession;

    let metainfo = if req.source_type == "magnet" {
        let magnet = MagnetLink::parse(&req.source)
            .map_err(|e| RpcError::BadRequest(e.to_string()))?;
        Metainfo::from_magnet_link(&magnet)
    } else {
        let bytes = base64::decode(&req.source)
            .map_err(|_| RpcError::BadRequest("Invalid base64 torrent data".into()))?;
        Metainfo::from_bytes(&bytes)
            .map_err(|e| RpcError::BadRequest(e.to_string()))?
    };

    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo, req.wallet_address);
    let session_id = state.torrent_sessions.write().await.add_session(session);

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
    let removed = state.torrent_sessions.write().await.remove_session(&id);
    if removed.is_none() {
        return Err(RpcError::NotFound(format!("Session {} not found", id)));
    }
    Ok(Json(json!({ "success": true, "message": format!("Session {} removed", id) })))
}

// ─── DEX ──────────────────────────────────────────────────────────────────────

pub async fn get_dex_orders(State(state): State<Arc<AppState>>) -> RpcResult<Json<Vec<DexOrderResponse>>> {
    let order_book = state.order_book.read().await;
    let orders: Vec<DexOrderResponse> = order_book.list_open_orders()
        .iter()
        .map(|o| DexOrderResponse {
            id: hex::encode(o.order_id),
            maker_address: o.maker_address.clone(),
            offer_amount_satoshis: o.vtr_amount,
            offer_asset: "VTR".to_string(),
            request_amount_satoshis: o.target_amount,
            request_asset: o.target_asset.clone(),
            rate: o.rate(),
            status: format!("{:?}", o.status),
            created_at: now_secs(),
            expires_at: o.expiry as u64,
        })
        .collect();

    Ok(Json(orders))
}

pub async fn place_dex_order(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlaceOrderRequest>,
) -> RpcResult<Json<PlaceOrderResponse>> {
    use vtorrent_node::atomic_swap::{AtomicSwap, SwapOrder, DEFAULT_HTLC_LOCKTIME};

    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }

    let swap = AtomicSwap::new();
    let hash_lock = hex::encode(swap.hash_lock);
    let htlc_address = format!("htlc_{}", &hash_lock[..16]);

    // SwapOrder::new(maker_address, vtr_amount, target_asset, target_amount, locktime_seconds)
    let locktime = if req.expiry_secs > 0 && req.expiry_secs <= u32::MAX as u64 {
        req.expiry_secs as u32
    } else {
        DEFAULT_HTLC_LOCKTIME
    };

    let order = SwapOrder::new(
        req.maker_address,
        req.offer_amount_satoshis,
        req.request_asset,
        req.request_amount_satoshis,
        locktime,
    );
    let order_id = hex::encode(order.order_id);
    state.order_book.write().await.add_order(order);

    Ok(Json(PlaceOrderResponse {
        order_id,
        htlc_address,
        hash_lock,
    }))
}

pub async fn cancel_dex_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<Value>> {
    let cancelled = state.order_book.write().await.cancel_order(&id);
    if !cancelled {
        return Err(RpcError::NotFound(format!("Order {} not found", id)));
    }
    Ok(Json(json!({ "success": true, "message": format!("Order {} cancelled", id) })))
}

// ─── Legacy Claim ─────────────────────────────────────────────────────────────

pub async fn check_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ClaimCheckRequest>,
) -> RpcResult<Json<ClaimCheckResponse>> {
    use vtorrent_node::genesis::get_legacy_balance;

    let chain = state.chain.lock().await;
    let claimable = get_legacy_balance(&req.legacy_address);
    let already_claimed = chain.is_claimed(&req.legacy_address);

    Ok(Json(ClaimCheckResponse {
        address: req.legacy_address.clone(),
        claimable_satoshis: claimable,
        display: format!("{:.6} VTR", claimable as f64 / 1_000_000.0),
        already_claimed,
    }))
}

pub async fn submit_claim(
    State(_state): State<Arc<AppState>>,
    Json(req): Json<ClaimSubmitRequest>,
) -> RpcResult<Json<ClaimSubmitResponse>> {
    if req.wif_private_key.is_empty() {
        return Err(RpcError::BadRequest("WIF private key is required".into()));
    }
    if req.recipient_address.is_empty() {
        return Err(RpcError::BadRequest("Recipient address is required".into()));
    }

    let addr_bytes = req.recipient_address.as_bytes();
    let preview_len = 8.min(addr_bytes.len());
    let fake_txid = format!("claim_{}", hex::encode(&addr_bytes[..preview_len]));

    Ok(Json(ClaimSubmitResponse {
        txid: fake_txid,
        claimed_satoshis: 0,
        recipient_address: req.recipient_address,
    }))
}
