//! BTC wallet endpoints: status, address, send.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use super::broadcast_btc;
use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn get_btc_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BtcStatusResponse>> {
    let btc = state.btc_wallet.read().await;
    match &*btc {
        None => Ok(Json(BtcStatusResponse {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
            synced: false,
        })),
        Some(w) => Ok(Json(BtcStatusResponse {
            initialized: true,
            balance_satoshis: w.balance(),
            address: w.current_address().ok(),
            best_height: w.best_height(),
            synced: w.synced(),
        })),
    }
}

/// GET /api/v1/btc/address
pub async fn get_btc_address(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    // Read-only: return the current receiving address without advancing the
    // wallet's address index (a GET must not mutate state).
    let btc = state.btc_wallet.read().await;
    match &*btc {
        None => Err(RpcError::BadRequest(
            "BTC wallet not initialized — call POST /api/v1/btc/init first".into(),
        )),
        Some(w) => {
            let address = w
                .current_address()
                .map_err(|e| RpcError::Internal(format!("Failed to derive BTC address: {}", e)))?;
            Ok(Json(json!({ "address": address })))
        }
    }
}

/// POST /api/v1/btc/send
pub async fn send_btc(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcSendRequest>,
) -> RpcResult<Json<BtcSendResponse>> {
    if req.amount_satoshis == 0 {
        return Err(RpcError::BadRequest(
            "Amount must be non-zero — provide a positive satoshi amount".into(),
        ));
    }
    if req.to_address.trim().is_empty() {
        return Err(RpcError::BadRequest(
            "Recipient address is required — provide a valid Bitcoin address".into(),
        ));
    }
    let fee = req.fee_satoshis.unwrap_or(1_000);

    // Build and sign, removing spent UTXOs from the wallet. The selected
    // UTXOs are returned so the spend can be rolled back if broadcasting
    // fails (otherwise the wallet forgets outputs for a tx that never made
    // it onto the network).
    let (txid_hex, raw, spent_utxos) = {
        let mut btc = state.btc_wallet.write().await;
        let w = btc.as_mut().ok_or_else(|| {
            RpcError::BadRequest(
                "BTC wallet not initialized — call POST /api/v1/btc/init first".into(),
            )
        })?;
        w.send_to(&req.to_address, req.amount_satoshis, fee)
            .map_err(|e| {
                RpcError::BadRequest(format!(
                    "BTC send_to {} failed: {}",
                    &req.to_address[..req.to_address.len().min(64)],
                    e
                ))
            })?
    };

    // Broadcast to the Bitcoin network.
    if let Err(e) = broadcast_btc(&state, &raw).await {
        if let Some(w) = state.btc_wallet.write().await.as_mut() {
            w.restore_utxos(&spent_utxos);
        }
        return Err(e);
    }

    Ok(Json(BtcSendResponse {
        txid: txid_hex,
        raw_tx: String::new(),
    }))
}
