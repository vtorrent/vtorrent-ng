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

    // Build and sign, removing spent UTXOs from the wallet. The removal and
    // the signed transaction are persisted together, so an uncertain
    // broadcast leaves the inputs unavailable and the send recoverable.
    let (txid_hex, raw, _) = {
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

    // Broadcast to the Bitcoin network. An error is not proof that the
    // transaction did not reach a peer, so the record is kept rather than
    // restoring inputs a peer may already have accepted.
    if let Err(e) = broadcast_btc(&state, &raw).await {
        return Err(RpcError::Internal(format!(
            "BTC broadcast status is uncertain for {txid_hex}: {e}. The signed transaction is retained and its inputs are unavailable; retry with POST /api/v1/btc/rebroadcast."
        )));
    }

    Ok(Json(BtcSendResponse {
        txid: txid_hex,
        raw_tx: String::new(),
    }))
}

/// GET /api/v1/btc/pending-sends
pub async fn list_btc_pending_sends(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<BtcPendingSendResponse>>> {
    let btc = state.btc_wallet.read().await;
    let Some(wallet) = btc.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    Ok(Json(
        wallet
            .pending_sends()
            .into_iter()
            .map(|send| BtcPendingSendResponse {
                txid: send.txid,
                has_raw_tx: send.raw.is_some(),
                inputs: send
                    .inputs
                    .into_iter()
                    .map(|input| BtcPendingInput {
                        txid: input.txid,
                        vout: input.vout,
                        value_satoshis: input.value,
                    })
                    .collect(),
            })
            .collect(),
    ))
}

/// POST /api/v1/btc/rebroadcast — retry a retained send.
///
/// Rebroadcasting is not proof of confirmation; the record is retired only
/// when a scan observes the spend.
pub async fn rebroadcast_btc(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcRebroadcastRequest>,
) -> RpcResult<Json<BtcSendResponse>> {
    rebroadcast_with_broadcast(&state, req, async |raw| broadcast_btc(&state, raw).await)
        .await
        .map(Json)
}

pub(crate) async fn rebroadcast_with_broadcast(
    state: &AppState,
    req: BtcRebroadcastRequest,
    broadcast: impl AsyncFnOnce(&[u8]) -> RpcResult<[u8; 32]>,
) -> RpcResult<BtcSendResponse> {
    crate::handlers::authorize_wallet_operation(state, &req.passphrase, req.otp_code.as_deref())
        .await?;
    let pending = {
        let btc = state.btc_wallet.read().await;
        let wallet = btc.as_ref().ok_or_else(|| {
            RpcError::BadRequest(
                "BTC wallet not initialized — call POST /api/v1/btc/init first".into(),
            )
        })?;
        wallet.pending_send(&req.txid).ok_or_else(|| {
            RpcError::NotFound(format!(
                "No retained BTC send {}; it may already be confirmed",
                truncate_chars(&req.txid, 64)
            ))
        })?
    };
    let raw = pending.raw.ok_or_else(|| {
        RpcError::BadRequest(format!(
            "Retained BTC send {} has no signed transaction and cannot be retried automatically; recover its inputs manually",
            truncate_chars(&req.txid, 64)
        ))
    })?;
    let txid = broadcast(&raw).await?;
    Ok(BtcSendResponse {
        txid: hex::encode(txid),
        raw_tx: String::new(),
    })
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

    async fn state_with_pending_send() -> (Arc<AppState>, String) {
        let mut wallet =
            vtorrent_btc::wallet::BtcWallet::with_network([7u8; 64], bitcoin::Network::Regtest);
        let owned = vtorrent_btc::utxo::Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 100_000,
            address: wallet.current_address().unwrap(),
            height: 100,
        };
        wallet.add_utxo(owned);
        let recipient =
            vtorrent_btc::keys::derive_address(&[9u8; 64], 0, bitcoin::Network::Regtest).unwrap();
        let (txid, _, _) = wallet.send_to(&recipient, 50_000, 1_000).unwrap();
        let state = Arc::new(AppState::new());
        *state.wallet_unlock_expiry.write().await = Some(0);
        *state.btc_wallet.write().await = Some(wallet);
        (state, txid)
    }

    #[tokio::test]
    async fn a_lost_rebroadcast_response_keeps_the_pending_record() {
        let (state, txid) = state_with_pending_send().await;
        let result = rebroadcast_with_broadcast(
            &state,
            BtcRebroadcastRequest {
                txid: txid.clone(),
                passphrase: zeroize::Zeroizing::new(String::new()),
                otp_code: None,
            },
            async |_| Err(RpcError::Internal("connection lost after send".into())),
        )
        .await;
        assert!(result.is_err(), "the caller must see the uncertain result");
        let btc = state.btc_wallet.read().await;
        assert!(
            btc.as_ref().unwrap().pending_send_raw(&txid).is_some(),
            "a lost response must not discard the signed transaction"
        );
    }

    #[tokio::test]
    async fn a_successful_rebroadcast_does_not_retire_the_record() {
        let (state, txid) = state_with_pending_send().await;
        let response = rebroadcast_with_broadcast(
            &state,
            BtcRebroadcastRequest {
                txid: txid.clone(),
                passphrase: zeroize::Zeroizing::new(String::new()),
                otp_code: None,
            },
            async |_| Ok([1u8; 32]),
        )
        .await
        .unwrap();
        assert_eq!(response.txid, hex::encode([1u8; 32]));
        let btc = state.btc_wallet.read().await;
        assert!(
            btc.as_ref().unwrap().pending_send(&txid).is_some(),
            "only a scan may retire the record"
        );
    }

    #[tokio::test]
    async fn a_record_without_signed_bytes_cannot_be_rebroadcast() {
        let path = wallet_file(
            "legacy-pending",
            r#"{"utxos":[],"last_scanned_height":100,"pending_removals":[{"removed":{"utxo":{"txid":"abababababababababababababababababababababababababababababababab","vout":0,"value":1000,"address":"bcrt1qtest","height":100},"prior":"Free"},"txid":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd","height":100}]}"#,
        );
        let wallet = vtorrent_btc::wallet::BtcWallet::with_persistence(
            [8u8; 64],
            bitcoin::Network::Regtest,
            path.clone(),
        )
        .unwrap();
        let state = Arc::new(AppState::new());
        *state.wallet_unlock_expiry.write().await = Some(0);
        *state.btc_wallet.write().await = Some(wallet);
        let error = rebroadcast_with_broadcast(
            &state,
            BtcRebroadcastRequest {
                txid: "cd".repeat(32),
                passphrase: zeroize::Zeroizing::new(String::new()),
                otp_code: None,
            },
            async |_| Ok([1u8; 32]),
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("no signed transaction"),
            "{error}"
        );
        std::fs::remove_file(path).unwrap();
    }
}
