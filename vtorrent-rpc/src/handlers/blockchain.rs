//! Blockchain query endpoints: node info, blocks, transactions, mempool, fees.

use axum::{
    extract::{Path, State},
    Json,
};
use std::sync::Arc;

use super::{block_response, now_secs, parse_hash32, transaction_lookup_response};
use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn get_node_info(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<NodeInfoResponse>> {
    let chain = state.chain.lock().await;
    let peer_count = *state.peer_count.read().await;
    let syncing = *state.syncing.read().await;
    let uptime = now_secs().saturating_sub(state.start_time);

    let height = chain.best_height() as u64;
    let _best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));

    // Compute sync percentage from best known peer height.
    let sync_percent = if syncing {
        let peer_height = *state.best_peer_height.read().await;
        if peer_height > 0 {
            ((height as f64 / peer_height as f64) * 100.0).min(99.9)
        } else {
            0.0
        }
    } else {
        100.0
    };

    // Get mempool size without holding the chain lock.
    drop(chain);
    let mempool_size = {
        let mempool = state.mempool.lock().await;
        mempool.size()
    };
    // Re-acquire chain for best_hash (already computed above, just use height).
    let chain = state.chain.lock().await;
    let height = chain.best_height() as u64;
    let best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));

    Ok(Json(NodeInfoResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        network: state.network.clone(),
        block_height: height,
        best_block_hash: best_hash,
        connections: peer_count,
        syncing,
        sync_percent,
        mempool_size,
        uptime_secs: uptime,
    }))
}

// ─── Blockchain ───────────────────────────────────────────────────────────────

pub async fn get_block_height(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BlockHeightResponse>> {
    let chain = state.chain.lock().await;
    let height = chain.best_height() as u64;
    let best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));
    let timestamp = chain
        .get_block_at_height(chain.best_height())
        .map(|b| b.header.timestamp)
        .unwrap_or_else(|| now_secs() as u32);

    Ok(Json(BlockHeightResponse {
        height,
        hash: best_hash,
        timestamp,
    }))
}

pub async fn get_block_by_hash(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> RpcResult<Json<BlockResponse>> {
    let hash = parse_hash32(&hash_hex, "block hash")?;
    let chain = state.chain.lock().await;
    let height = chain
        .block_height(&hash)
        .ok_or_else(|| RpcError::NotFound(format!("Block not found at hash {}", hash_hex)))?;
    let block = chain.get_block(&hash).ok_or_else(|| {
        RpcError::Internal(format!(
            "Block at height {} indexed but block data missing — store may be corrupt",
            height
        ))
    })?;
    Ok(Json(block_response(hash, height, block)))
}

/// Get an active-chain block by height.
pub async fn get_block_by_height(
    State(state): State<Arc<AppState>>,
    Path(height): Path<u32>,
) -> RpcResult<Json<BlockResponse>> {
    let chain = state.chain.lock().await;
    let hash = chain.block_hash_at_height(height).ok_or_else(|| {
        RpcError::NotFound(format!(
            "Block not found at height {} (chain tip is at {})",
            height,
            chain.best_height()
        ))
    })?;
    let block = chain.get_block_at_height(height).ok_or_else(|| {
        RpcError::Internal(format!(
            "Block at height {} has hash but block data missing — store may be corrupt",
            height
        ))
    })?;
    Ok(Json(block_response(hash, height, block)))
}

/// Get a transaction by txid, searching the active chain first and then mempool.
pub async fn get_transaction_by_id(
    State(state): State<Arc<AppState>>,
    Path(txid_hex): Path<String>,
) -> RpcResult<Json<TransactionLookupResponse>> {
    let txid = parse_hash32(&txid_hex, "transaction ID")?;

    {
        let chain = state.chain.lock().await;
        if let Some((tx, block_hash, height)) = chain.get_transaction(&txid) {
            return Ok(Json(transaction_lookup_response(
                txid,
                tx,
                Some(block_hash),
                Some(height),
            )));
        }
    }

    let mempool = state.mempool.lock().await;
    let tx = mempool.get_transaction(&txid).ok_or_else(|| {
        RpcError::NotFound(format!(
            "Transaction {} not found in chain or mempool",
            txid_hex
        ))
    })?;
    Ok(Json(transaction_lookup_response(txid, tx, None, None)))
}

/// Submit a raw, signed transaction to the local mempool and live P2P node.
pub async fn broadcast_transaction(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BroadcastTransactionRequest>,
) -> RpcResult<Json<BroadcastTransactionResponse>> {
    const MAX_RAW_TX_BYTES: usize = 1_000_000;

    if req.raw_tx.is_empty() {
        return Err(RpcError::BadRequest(
            "raw_tx is required — provide a hex-encoded signed transaction".into(),
        ));
    }
    let raw = hex::decode(&req.raw_tx).map_err(|_| {
        RpcError::BadRequest(format!(
            "raw_tx must be valid hexadecimal, got {} chars starting with \"{}\"",
            req.raw_tx.len(),
            &req.raw_tx[..req.raw_tx.len().min(16)]
        ))
    })?;
    if raw.len() > MAX_RAW_TX_BYTES {
        return Err(RpcError::BadRequest(format!(
            "raw_tx is {} bytes, exceeds the {} byte limit",
            raw.len(),
            MAX_RAW_TX_BYTES
        )));
    }
    let tx: vtorrent_node::block::Transaction = bincode::deserialize(&raw).map_err(|e| {
        RpcError::BadRequest(format!("raw_tx is not a valid vTorrent transaction: {}", e))
    })?;
    let txid = tx.txid();

    {
        // Verify the fee from the live UTXO set — same rule the P2P relay
        // path applies — so self-reported fee estimates cannot buy priority.
        // Chain is locked BEFORE mempool to match the node loop's lock order
        // (chain → mempool) and avoid ABBA deadlock with block processing.
        let chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx.clone())
            .map_err(|e| {
                RpcError::BadRequest(format!(
                    "Mempool rejected transaction {}: {}",
                    hex::encode(txid),
                    e
                ))
            })?;
    }

    let relayed = match &state.tx_submit {
        Some(sender) => sender.try_send(tx).is_ok(),
        None => false,
    };
    tracing::info!(txid = %hex::encode(txid), relayed, "Raw transaction accepted for broadcast");

    Ok(Json(BroadcastTransactionResponse {
        txid: hex::encode(txid),
        accepted: true,
        relayed,
    }))
}

pub async fn get_mempool(State(state): State<Arc<AppState>>) -> RpcResult<Json<MempoolResponse>> {
    let mempool = state.mempool.lock().await;
    let txs = mempool.get_transactions();
    let txids: Vec<String> = txs.iter().map(|tx| hex::encode(tx.txid())).collect();
    let count = txids.len();
    // Sum the actual serialized size of each transaction, not an estimate.
    let size_bytes: usize = txs
        .iter()
        .map(|tx| serde_json::to_vec(tx).map(|v| v.len()).unwrap_or(0))
        .sum();

    Ok(Json(MempoolResponse {
        count,
        size_bytes,
        txids,
    }))
}

/// Return the current fee recommendations derived from the local mempool.
pub async fn get_fee_estimate(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<FeeEstimateResponse>> {
    let mempool = state.mempool.lock().await;
    Ok(Json(FeeEstimateResponse {
        recommended_sat_per_byte: mempool.recommended_fee_rate(),
        minimum_sat_per_byte: mempool.min_fee_rate(),
        median_sat_per_byte: mempool.median_fee_rate(),
        mempool_transactions: mempool.size(),
    }))
}
