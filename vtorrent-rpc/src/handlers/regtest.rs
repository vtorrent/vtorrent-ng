//! Regtest-only endpoints: faucet and debug helpers.

use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn faucet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FaucetRequest>,
) -> RpcResult<Json<FaucetResponse>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Faucet is only available in regtest mode".into(),
        ));
    }
    if req.address.trim().is_empty() {
        return Err(RpcError::BadRequest(
            "Address is required — provide a valid VTR address".into(),
        ));
    }

    // Per-address cooldown: 10 seconds between claims from the same address.
    const FAUCET_COOLDOWN_SECS: u64 = 10;
    {
        let mut cooldowns = state.faucet_cooldowns.write().await;
        if let Some(last) = cooldowns.get(&req.address) {
            let elapsed = last.elapsed().as_secs();
            if elapsed < FAUCET_COOLDOWN_SECS {
                return Err(RpcError::BadRequest(format!(
                    "Faucet cooldown: wait {} more seconds before claiming again",
                    FAUCET_COOLDOWN_SECS - elapsed,
                )));
            }
        }
        cooldowns.insert(req.address.clone(), std::time::Instant::now());
    }

    let amount = req
        .amount_satoshis
        .unwrap_or(100 * vtorrent_node::consensus::COIN);
    if amount == 0 {
        return Err(RpcError::BadRequest(
            "Amount must be non-zero — provide a positive satoshi amount".into(),
        ));
    }

    let (txid, height, block) = {
        let mut chain = state.chain.lock().await;
        let txid = chain.mint_to_address(&req.address, amount).map_err(|e| {
            RpcError::Internal(format!(
                "Failed to mint {} sats to {}: {}",
                amount,
                &req.address[..req.address.len().min(64)],
                e
            ))
        })?;
        let height = chain.best_height();
        let block = chain
            .get_block_at_height(height)
            .cloned()
            .ok_or_else(|| RpcError::Internal(format!("Minted block at height {} not found immediately after mint — chain state inconsistent", height)))?;
        (txid, height, block)
    };

    // Announce the minted block to peers so a multi-node regtest network stays
    // in sync (the faucet mints directly into the chain, bypassing the node's
    // normal block-production path).
    if let Some(sender) = &state.block_submit {
        let _ = sender.try_send(block);
    }

    Ok(Json(FaucetResponse {
        address: req.address,
        amount_satoshis: amount,
        txid: hex::encode(txid),
        block_height: height as u64,
    }))
}

/// GET /api/v1/debug/order/:id/preimage
///
/// Returns the secret preimage for an order (regtest only). In a real swap the
/// taker learns the preimage when the maker claims BTC on-chain; this endpoint
/// exists purely to exercise the VTR claim leg in local regtest testing.
pub async fn debug_order_preimage(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Preimage debug endpoint is only available in regtest mode".into(),
        ));
    }
    let order_book = state.order_book.read().await;
    let order = order_book.get_order(&order_id).ok_or_else(|| {
        RpcError::NotFound(format!(
            "Order {} not found — check the order_id and ensure the order exists",
            order_id
        ))
    })?;
    let preimage = order.preimage.ok_or_else(|| {
        RpcError::BadRequest(format!(
            "Order {} has no preimage — only maker-created orders have preimages",
            order_id
        ))
    })?;
    Ok(Json(json!({
        "order_id": order_id,
        "preimage": hex::encode(preimage),
    })))
}

/// POST /api/v1/debug/mocktime
///
/// Sets the regtest mock clock (regtest only). Time-dependent checks (e.g.
/// HTLC expiry) use this instead of the wall clock, enabling refund-path
/// testing without waiting for real time to pass. A `null` timestamp resets
/// to the wall clock.
pub async fn debug_mocktime(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Mocktime is only available in regtest mode".into(),
        ));
    }
    let ts = req.get("timestamp").cloned();
    let new_time =
        match ts {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => n.as_u64(),
            _ => return Err(RpcError::BadRequest(
                "timestamp must be a number (unix seconds) or null to reset — got unexpected type"
                    .into(),
            )),
        };
    *state.mock_time.write().await = new_time;
    Ok(Json(json!({ "mock_time": new_time })))
}
