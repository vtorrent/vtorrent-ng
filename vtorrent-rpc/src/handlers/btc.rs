//! BTC wallet endpoints: status, address, send.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use super::broadcast_btc;
use super::truncate_chars;
use crate::error::{RpcError, RpcResult};
use crate::handlers::verify_wallet_auth;
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
            balance_hidden: false,
        })),
        Some(w) => Ok(Json(BtcStatusResponse {
            initialized: true,
            balance_satoshis: w.balance(),
            address: w.current_address().ok(),
            best_height: w.best_height(),
            synced: w.synced(),
            balance_hidden: w.balance_hidden(),
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
    // Spending the node's BTC wallet requires an unlocked wallet, matching the
    // VTR path (`send_vtr`). The API key is a transport credential; it must not
    // by itself authorize moving funds. Without this, anyone holding the key
    // could drain the BTC wallet.
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.amount_satoshis == 0 {
        return Err(RpcError::BadRequest(
            "Amount must be non-zero — provide a positive satoshi amount".into(),
        ));
    }
    if req.amount_satoshis > vtorrent_btc::MAX_MONEY_SATS {
        return Err(RpcError::BadRequest(
            "Amount exceeds the Bitcoin maximum supply".into(),
        ));
    }
    if req.to_address.trim().is_empty() {
        return Err(RpcError::BadRequest(
            "Recipient address is required — provide a valid Bitcoin address".into(),
        ));
    }
    if req.passphrase.is_empty() {
        return Err(RpcError::BadRequest(
            "Wallet passphrase is required to send BTC".into(),
        ));
    }
    let _ = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    let fee = req.fee_satoshis.unwrap_or(1_000);
    if fee > vtorrent_btc::MAX_MONEY_SATS {
        return Err(RpcError::BadRequest(
            "Fee exceeds the Bitcoin maximum supply".into(),
        ));
    }

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
                    truncate_chars(&req.to_address, 64),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet_file(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vtr-rpc-btc-status-{}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[tokio::test]
    async fn btc_status_reports_a_hidden_balance() {
        let path = wallet_file(
            "hidden",
            r#"{"utxos":[],"birthday":0,"seen_funds":true,"rescan_required":true}"#,
        );
        let hidden = vtorrent_btc::wallet::BtcWallet::with_persistence(
            [1u8; 64],
            bitcoin::Network::Regtest,
            path.clone(),
        )
        .unwrap();

        let state = Arc::new(AppState::new());
        *state.btc_wallet.write().await = Some(hidden);
        let status = get_btc_status(State(Arc::clone(&state))).await.unwrap().0;
        assert!(status.initialized);
        assert_eq!(
            status.balance_satoshis, 0,
            "the persisted reset hides the restored balance"
        );
        assert!(
            status.balance_hidden,
            "a withheld balance must be reported, not rendered as 0 BTC"
        );

        let visible =
            vtorrent_btc::wallet::BtcWallet::with_network([2u8; 64], bitcoin::Network::Regtest);
        *state.btc_wallet.write().await = Some(visible);
        let status = get_btc_status(State(state)).await.unwrap().0;
        assert!(!status.balance_hidden);
        std::fs::remove_file(path).unwrap();
    }
}
