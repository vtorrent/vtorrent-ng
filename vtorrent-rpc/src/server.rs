use crate::handlers::*;
use crate::metrics::metrics_handler;
use crate::state::AppState;
use crate::ws::ws_handler;
use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Default RPC port — same as legacy vTorrent RPC port + 1.
pub const DEFAULT_RPC_PORT: u16 = 22525;

/// Constant-time string comparison to avoid timing side-channels on the API key.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Reject requests without a valid `X-API-Key` header when a key is configured.
///
/// When `AppState::rpc_api_key` is `None` (auth disabled), requests pass
/// through unchanged — this keeps the standalone/test mode and the read-only
/// info endpoints working without a key.
async fn require_api_key(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    if let Some(expected) = &state.rpc_api_key {
        let provided = request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(provided, expected) {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(request).await)
}

/// Build the Axum router with all API routes.
pub fn build_router(state: AppState) -> Router {
    let state = Arc::new(state);
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let auth = middleware::from_fn_with_state(Arc::clone(&state), require_api_key);

    // Routes that manage funds, keys, or broadcast to the network require the
    // API key when one is configured.
    let protected = Router::new()
        .route("/api/v1/wallet/import", post(import_wallet))
        .route("/api/v1/wallet/send", post(send_vtr))
        .route("/api/v1/wallet/unlock", post(unlock_wallet))
        .route("/api/v1/wallet/lock", post(lock_wallet))
        .route("/api/v1/staking/start", post(start_staking))
        .route("/api/v1/staking/stop", post(stop_staking))
        .route("/api/v1/torrent/add", post(add_torrent))
        .route("/api/v1/torrent/:id", delete(remove_torrent))
        .route("/api/v1/dex/order", post(place_dex_order))
        .route("/api/v1/dex/order/:id", delete(cancel_dex_order))
        .route("/api/v1/dex/match", post(match_dex_order))
        .route("/api/v1/swap/btc-fund", post(btc_fund))
        .route("/api/v1/swap/vtr-claim", post(vtr_claim))
        .route("/api/v1/swap/btc-claim", post(btc_claim))
        .route("/api/v1/swap/refund", post(swap_refund))
        .route("/api/v1/blockchain/broadcast", post(broadcast_transaction))
        .route("/api/v1/claim/submit", post(submit_claim))
        .route("/api/v1/spv/headers", post(add_spv_headers))
        .route("/api/v1/btc/send", post(send_btc))
        .layer(auth);

    Router::new()
        // Node info
        .route("/api/v1/info", get(get_node_info))
        // Blockchain (read-only)
        .route("/api/v1/blockchain/height", get(get_block_height))
        .route(
            "/api/v1/blockchain/block/height/:height",
            get(get_block_by_height),
        )
        .route("/api/v1/blockchain/block/:hash", get(get_block_by_hash))
        .route("/api/v1/blockchain/tx/:txid", get(get_transaction_by_id))
        .route("/api/v1/mempool", get(get_mempool))
        .route("/api/v1/fee/estimate", get(get_fee_estimate))
        // Wallet (read-only)
        .route("/api/v1/wallet/balance", get(get_balance))
        .route("/api/v1/wallet/addresses", get(get_addresses))
        .route("/api/v1/wallet/utxos", get(get_wallet_utxos))
        .route("/api/v1/wallet/transactions", get(get_transactions))
        // Staking (read-only)
        .route("/api/v1/staking/status", get(get_staking_status))
        // Torrent
        .route("/api/v1/torrent/sessions", get(list_torrent_sessions))
        // DEX
        .route("/api/v1/dex/orders", get(get_dex_orders))
        // Bitcoin wallet
        .route("/api/v1/btc/status", get(get_btc_status))
        .route("/api/v1/btc/address", get(get_btc_address))
        // Legacy claim
        .route("/api/v1/claim/check", post(check_claim))
        // SPV light client
        .route("/api/v1/spv/status", get(get_spv_status))
        // Peers
        .route("/api/v1/peers", get(get_peers))
        // WebSocket event stream
        .route("/ws", get(ws_handler))
        // Prometheus metrics
        .route("/metrics", get(metrics_handler))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Start the RPC server on the given address.
pub async fn start_server(
    bind_addr: &str,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    tracing::info!("vTorrent RPC server listening on {}", bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt; // for `oneshot`

    async fn get(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    async fn post_json(
        app: axum::Router,
        uri: &str,
        payload: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn test_get_node_info() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/info").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["network"], "vtorrent-mainnet");
        assert!(body["block_height"].is_number());
    }

    #[tokio::test]
    async fn test_get_block_height() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/blockchain/height").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["height"].is_number());
    }

    #[tokio::test]
    async fn test_get_mempool() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/mempool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["count"], 0);
    }

    #[tokio::test]
    async fn test_get_genesis_block_by_height() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/blockchain/block/height/0").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["height"], 0);
        assert!(body["hash"].as_str().unwrap().len() == 64);
    }

    #[tokio::test]
    async fn test_get_genesis_transaction_by_id() {
        let state = AppState::new();
        let txid = {
            let chain = state.chain.lock().await;
            hex::encode(chain.genesis_block().transactions[0].txid())
        };
        let app = build_router(state);
        let (status, body) = get(app, &format!("/api/v1/blockchain/tx/{}", txid)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["txid"], txid);
        assert_eq!(body["block_height"], 0);
    }

    #[tokio::test]
    async fn test_broadcast_raw_transaction_and_lookup() {
        use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};

        let tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: [7u8; 32],
                prev_vout: 0,
                script_sig: vec![0x51],
                sequence: 0xffff_ffff,
            }],
            outputs: vec![TxOutput {
                value: 95_000,
                script_pubkey: vec![0x51],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let txid = hex::encode(tx.txid());
        let app = build_router(AppState::new());
        let (status, body) = post_json(
            app.clone(),
            "/api/v1/blockchain/broadcast",
            serde_json::json!({ "raw_tx": hex::encode(bincode::serialize(&tx).unwrap()) }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["accepted"], true);
        assert_eq!(body["relayed"], false);

        let (status, body) = get(app, &format!("/api/v1/blockchain/tx/{}", txid)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["block_hash"], serde_json::Value::Null);
        assert_eq!(body["tx_type"], "transfer");
    }

    #[tokio::test]
    async fn test_fee_estimate_and_wallet_utxos() {
        let app = build_router(AppState::new());
        let (status, fee_body) = get(app.clone(), "/api/v1/fee/estimate").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fee_body["recommended_sat_per_byte"], 1);
        assert_eq!(fee_body["mempool_transactions"], 0);

        let (status, utxo_body) = get(app, "/api/v1/wallet/utxos?address=VTestAddress").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(utxo_body["total_satoshis"], 0);
        assert!(utxo_body["utxos"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_wallet_lock_unlock() {
        let app = build_router(AppState::new());
        // Import a wallet with a passphrase (wallet starts locked).
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/import",
            serde_json::json!({
                "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                "passphrase": "testpassphrase"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Unlock with the wrong passphrase is rejected.
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "wrong", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Unlock with the correct passphrase succeeds.
        let (status, body) = post_json(
            app,
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "testpassphrase", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
    }

    #[tokio::test]
    async fn test_send_requires_unlock() {
        let app = build_router(AppState::new());
        let (status, _body) = post_json(
            app,
            "/api/v1/wallet/send",
            serde_json::json!({
                "to_address": "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT",
                "amount_satoshis": 1000000,
                "passphrase": "test"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_send_rejects_wrong_passphrase() {
        let app = build_router(AppState::new());
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/import",
            serde_json::json!({
                "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                "passphrase": "correct-passphrase"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "correct-passphrase", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Sending with the wrong passphrase is rejected even though unlocked.
        let (status, _) = post_json(
            app,
            "/api/v1/wallet/send",
            serde_json::json!({
                "to_address": "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT",
                "amount_satoshis": 1000000,
                "passphrase": "wrong-passphrase"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_unlock_requires_totp_when_enabled() {
        let app = build_router(AppState::new());
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/import",
            serde_json::json!({
                "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                "passphrase": "testpassphrase",
                "otp_secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Unlock without a TOTP code is rejected.
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "testpassphrase", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Unlock with the correct passphrase and a valid TOTP code succeeds.
        let secret =
            vtorrent_wallet::otp::TotpSecret::from_base32("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
                .unwrap();
        let code = secret.current_code().unwrap();
        let (status, body) = post_json(
            app,
            "/api/v1/wallet/unlock",
            serde_json::json!({
                "passphrase": "testpassphrase",
                "otp_code": code,
                "timeout_secs": 300
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
    }

    #[tokio::test]
    async fn test_staking_status() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/staking/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);
    }

    #[tokio::test]
    async fn test_list_torrent_sessions_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/torrent/sessions").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dex_orders_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/dex/orders").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_swap_btc_fund_unknown_order() {
        let app = build_router(AppState::new());
        let (status, body) = post_json(
            app,
            "/api/v1/swap/btc-fund",
            serde_json::json!({ "order_id": "00".repeat(32), "btc_refund_address": "bc1qtest" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], true);
    }

    #[tokio::test]
    async fn test_block_not_found() {
        let app = build_router(AppState::new());
        let (status, _body) = get(
            app,
            "/api/v1/blockchain/block/deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        ).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_spv_status_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/spv/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["header_count"], 0);
        assert_eq!(body["best_height"], 0);
    }

    #[tokio::test]
    async fn test_btc_status_uninitialized() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/btc/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["initialized"], false);
        assert_eq!(body["balance_satoshis"], 0);
    }

    #[tokio::test]
    async fn test_get_peers_empty() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/peers").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["peers"].as_array().unwrap().is_empty());
        assert_eq!(body["count"], 0);
    }

    #[tokio::test]
    async fn test_spv_add_headers() {
        let app = build_router(AppState::new());
        // Genesis header: prev_hash = all zeros
        let (status, body) = post_json(
            app,
            "/api/v1/spv/headers",
            serde_json::json!({
                "headers": [{
                    "version": 1,
                    "prev_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                    "merkle_root": "0101010101010101010101010101010101010101010101010101010101010101",
                    "timestamp": 1700000000u32,
                    "bits": 0x1d00ffffu32,
                    "nonce": 0u32,
                    "height": 0u32
                }]
            }),
        ).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["added"], 1);
        assert_eq!(body["best_height"], 0);
    }

    #[tokio::test]
    async fn test_api_key_protects_sensitive_endpoints() {
        let mut state = AppState::new();
        state.rpc_api_key = Some("s3cret".into());
        let app = build_router(state);

        // Without the key: sensitive endpoint rejected, read-only endpoint open.
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "test", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = get(app.clone(), "/api/v1/info").await;
        assert_eq!(status, StatusCode::OK);

        // Wrong key rejected.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/wallet/unlock")
                    .header("content-type", "application/json")
                    .header("x-api-key", "wrong")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "passphrase": "test", "timeout_secs": 300
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Correct key accepted: import then unlock.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/wallet/import")
                    .header("content-type", "application/json")
                    .header("x-api-key", "s3cret")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                            "passphrase": "test"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/wallet/unlock")
                    .header("content-type", "application/json")
                    .header("x-api-key", "s3cret")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "passphrase": "test", "timeout_secs": 300
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_key_disabled_by_default() {
        let app = build_router(AppState::new());
        let (status, _) = post_json(
            app.clone(),
            "/api/v1/wallet/import",
            serde_json::json!({
                "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                "passphrase": "test"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = post_json(
            app,
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "test", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
    }
}
