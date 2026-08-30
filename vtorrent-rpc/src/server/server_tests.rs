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
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
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
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn delete(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
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
    use secp256k1::{Message, Secp256k1, SecretKey};
    use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};
    use vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address;

    // Fund a real UTXO so the broadcast path's fee verification passes.
    let state = AppState::new();
    let secret = SecretKey::from_slice(&[42u8; 32]).unwrap();
    let secp = Secp256k1::new();
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret);
    let address = pubkey_to_vtorrent_address(&pubkey.serialize()).unwrap();
    let funding_txid = state
        .chain
        .lock()
        .await
        .mint_to_address(&address, 100_000)
        .unwrap();

    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: funding_txid,
            prev_vout: 0,
            script_sig: vec![],
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
    // Sign the input so the mempool's script validation accepts it.
    let sighash = tx.sighash(0, &[]);
    let msg = Message::from_digest(sighash);
    let sig = secp.sign_ecdsa(&msg, &secret);
    let mut sig_der = sig.serialize_der().to_vec();
    sig_der.push(0x01);
    let mut script_sig = Vec::new();
    script_sig.push(sig_der.len() as u8);
    script_sig.extend_from_slice(&sig_der);
    script_sig.push(pubkey.serialize().len() as u8);
    script_sig.extend_from_slice(&pubkey.serialize());
    tx.inputs[0].script_sig = script_sig;

    let txid = hex::encode(tx.txid());
    let app = build_router(state);
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
        vtorrent_wallet::otp::TotpSecret::from_base32("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
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
    assert_eq!(body["blocks_staked"], 0);
    assert!(body["last_stake_time"].is_null());
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

/// Regression: btc-fund on an order whose VTR leg was never funded must be
/// rejected (previously it passed the lifecycle guard and let the taker
/// lock BTC into an HTLC for an unfunded order).
#[tokio::test]
async fn test_swap_btc_fund_requires_vtr_funded() {
    let app = build_router(AppState::new());

    // Import + unlock the maker wallet.
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
    let (status, _) = post_json(
        app.clone(),
        "/api/v1/wallet/unlock",
        serde_json::json!({ "passphrase": "testpassphrase", "timeout_secs": 300 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Place an order (maker = the imported wallet's address).
    let (status, body) = post_json(
        app.clone(),
        "/api/v1/dex/order",
        serde_json::json!({
            "maker_address": "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k",
            "offer_asset": "VTR",
            "offer_amount_satoshis": 1_000_000,
            "request_asset": "BTC",
            "request_amount_satoshis": 1_000,
            "expiry_secs": 3600,
            "passphrase": "testpassphrase",
            "maker_btc_address": "bcrt1qtest"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = body["order_id"].as_str().unwrap().to_string();

    // btc-fund without a prior match: swap state does not exist → 400.
    let (status, body) = post_json(
        app,
        "/api/v1/swap/btc-fund",
        serde_json::json!({ "order_id": order_id, "btc_refund_address": "bcrt1qtest" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("VTR leg not funded"),
        "unexpected error: {}",
        body["message"]
    );
}

/// Regression: cancelling a DEX order with the wallet locked must be
/// refused — previously the ownership check was skipped when the maker
/// address was unknown, letting any caller cancel any order.
#[tokio::test]
async fn test_dex_cancel_requires_unlocked_wallet() {
    let app = build_router(AppState::new());

    // Import + unlock + place an order.
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
    let (status, _) = post_json(
        app.clone(),
        "/api/v1/wallet/unlock",
        serde_json::json!({ "passphrase": "testpassphrase", "timeout_secs": 300 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        app.clone(),
        "/api/v1/dex/order",
        serde_json::json!({
            "maker_address": "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k",
            "offer_asset": "VTR",
            "offer_amount_satoshis": 1_000_000,
            "request_asset": "BTC",
            "request_amount_satoshis": 1_000,
            "expiry_secs": 3600,
            "passphrase": "testpassphrase"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = body["order_id"].as_str().unwrap().to_string();

    // Lock the wallet — cancellation must now be refused.
    let (status, _) = post_json(app.clone(), "/api/v1/wallet/lock", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = delete(app, &format!("/api/v1/dex/order/{}", order_id)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body["message"].as_str().unwrap().contains("Wallet locked"),
        "unexpected error: {}",
        body["message"]
    );
}

#[tokio::test]
async fn test_block_not_found() {
    let app = build_router(AppState::new());
    let (status, _body) = get(
        app,
        "/api/v1/blockchain/block/deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    )
    .await;
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
    // Genesis header: prev_hash = all zeros. Headers must satisfy their
    // own PoW target, so use the easiest target and mine a nonce.
    let bits = 0x207f_ffffu32;
    let mut nonce = 0u32;
    let meets_target = |nonce: u32| -> bool {
        use sha2::Digest as _;
        let mut buf = Vec::with_capacity(80);
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(&[0x01u8; 32]);
        buf.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(&nonce.to_le_bytes());
        let h1 = sha2::Sha256::digest(&buf);
        let h2 = sha2::Sha256::digest(h1);
        let exponent = (bits >> 24) as usize;
        let mantissa = bits & 0x00ff_ffff;
        let mut target = [0u8; 32];
        let low_zeros = exponent - 3;
        let mb = mantissa.to_le_bytes();
        target[low_zeros] = mb[0];
        target[low_zeros + 1] = mb[1];
        target[low_zeros + 2] = mb[2];
        for i in (0..32).rev() {
            match h2[i].cmp(&target[i]) {
                std::cmp::Ordering::Less => return true,
                std::cmp::Ordering::Greater => return false,
                _ => {}
            }
        }
        true
    };
    while !meets_target(nonce) {
        nonce += 1;
    }
    let (status, body) = post_json(
        app,
        "/api/v1/spv/headers",
        serde_json::json!({
            "headers": [{
                "version": 1,
                "prev_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                "merkle_root": "0101010101010101010101010101010101010101010101010101010101010101",
                "timestamp": 1700000000u32,
                "bits": bits,
                "nonce": nonce,
                "height": 0u32
            }]
        }),
    )
    .await;
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
