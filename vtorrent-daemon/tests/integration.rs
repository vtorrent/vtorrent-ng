/// Daemon integration tests.
///
/// These tests spin up the full RPC router (backed by a real `AppState`)
/// and exercise the complete HTTP API surface, verifying that the daemon
/// wiring is correct end-to-end.
///
/// The tests do **not** start the P2P node event loop — they only test the
/// RPC layer — so they run quickly and without network access.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt; // for `oneshot`
use vtorrent_rpc::{server::build_router, state::AppState};

// ─── Helpers ─────────────────────────────────────────────────────────────────

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

fn app() -> axum::Router {
    build_router(AppState::new())
}

// ─── Node info ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_node_info_returns_mainnet() {
    let (status, body) = get(app(), "/api/v1/info").await;
    assert_eq!(status, StatusCode::OK, "GET /api/v1/info should return 200");
    assert_eq!(
        body["network"], "vtorrent-mainnet",
        "network field should be vtorrent-mainnet"
    );
    assert!(
        body["block_height"].is_number(),
        "block_height should be a number"
    );
    assert!(body["version"].is_string(), "version should be a string");
}

#[tokio::test]
async fn integration_block_height_is_zero_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/blockchain/height").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["height"], 0, "fresh node should report height 0");
}

// ─── Mempool ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_mempool_empty_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/mempool").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0);
    // MempoolResponse uses `txids`, not `transactions`
    assert!(body["txids"].as_array().unwrap().is_empty());
}

// ─── Wallet ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_wallet_import_unlock_flow() {
    let router = build_router(AppState::new());
    // Import with a passphrase (wallet starts locked).
    let (status, _) = post_json(
        router.clone(),
        "/api/v1/wallet/import",
        serde_json::json!({
            "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
            "passphrase": "hunter2"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wallet import should succeed");

    // Wrong passphrase is rejected.
    let (status, _) = post_json(
        router.clone(),
        "/api/v1/wallet/unlock",
        serde_json::json!({ "passphrase": "wrong", "timeout_secs": 60 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Correct passphrase unlocks.
    let (status, body) = post_json(
        router,
        "/api/v1/wallet/unlock",
        serde_json::json!({ "passphrase": "hunter2", "timeout_secs": 60 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);
}

#[tokio::test]
async fn integration_send_requires_unlock() {
    // Without unlocking first, send should return 403 Forbidden.
    let (status, _body) = post_json(
        app(),
        "/api/v1/wallet/send",
        serde_json::json!({
            "to_address": "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT",
            "amount_satoshis": 1_000_000u64,
            "passphrase": "hunter2"
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "send without unlock should return 403"
    );
}

// ─── Staking ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_staking_disabled_by_default() {
    let (status, body) = get(app(), "/api/v1/staking/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["enabled"], false,
        "staking should be disabled by default"
    );
    assert_eq!(body["blocks_staked"], 0);
}

#[tokio::test]
async fn integration_staking_start_stop_roundtrip() {
    // build_router wraps AppState in Arc; we need a single Arc shared across
    // all requests so that the wallet-unlock state is visible to start_staking.
    // We do this by building one router and using `tower::Service::call` via
    // a shared Arc<Mutex<Router>> — but the simplest approach is to use the
    // state's Arc fields directly by cloning the state before wrapping.
    //
    // Because build_router(state) moves state into Arc::new(state), we must
    // unlock and start staking through the *same* router instance.
    let router = build_router(AppState::new());

    // Import a wallet, then unlock it — start_staking requires an unlocked wallet.
    let (status, _) = post_json(
        router.clone(),
        "/api/v1/wallet/import",
        serde_json::json!({
            "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
            "passphrase": "test"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wallet import should succeed");
    let (status, _) = post_json(
        router.clone(),
        "/api/v1/wallet/unlock",
        serde_json::json!({ "passphrase": "test", "timeout_secs": 300 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "wallet unlock should succeed");

    // Start staking — StakingStartRequest requires `address` + `passphrase`.
    let (status, body) = post_json(
        router.clone(),
        "/api/v1/staking/start",
        serde_json::json!({
            "address": "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT",
            "passphrase": "test"
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "start staking should return 200; body={}",
        body
    );
    assert_eq!(body["success"], true);

    // Verify status reflects the change.
    let (status, body) = get(router.clone(), "/api/v1/staking/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true, "staking should now be enabled");

    // Stop staking.
    let (status, body) = post_json(
        router.clone(),
        "/api/v1/staking/stop",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stop staking should return 200; body={}",
        body
    );
    assert_eq!(body["success"], true);

    // Verify status reflects the change.
    let (status, body) = get(router.clone(), "/api/v1/staking/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false, "staking should be disabled again");
}

// ─── Peers ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_peers_empty_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/peers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0, "fresh node should have no peers");
    assert!(
        body["peers"].as_array().unwrap().is_empty(),
        "peers array should be empty"
    );
}

#[tokio::test]
async fn integration_peers_reflects_peer_list_state() {
    use vtorrent_rpc::state::PeerInfo;

    let state = AppState::new();
    // Manually inject a peer into the live peer list (simulating what the
    // daemon event bridge does on PeerConnected).
    {
        let mut peers = state.peer_list.write().await;
        peers.push(PeerInfo {
            addr: "1.2.3.4:22526".to_string(),
            user_agent: "/vTorrent:2.0.0/".to_string(),
            services: 0x01,
            best_height: 42,
        });
    }

    let router = build_router(state);
    let (status, body) = get(router, "/api/v1/peers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    let peers = body["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["addr"], "1.2.3.4:22526");
    assert_eq!(peers[0]["user_agent"], "/vTorrent:2.0.0/");
    assert_eq!(peers[0]["best_height"], 42);
}

// ─── SPV ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_spv_status_empty_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/spv/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["header_count"], 0);
    assert_eq!(body["best_height"], 0);
}

#[tokio::test]
async fn integration_spv_add_genesis_header() {
    // Headers must satisfy their own compact-target PoW now, so mine a nonce
    // against the easiest possible target (~2 attempts on average).
    let bits = 0x207f_ffffu32;
    let mut nonce = 1u32;
    let meets_target = |nonce: u32| -> bool {
        let mut buf = Vec::with_capacity(120);
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(&[0x01u8; 32]);
        buf.extend_from_slice(&[0u8; 32]);
        buf.extend_from_slice(&1_700_000_000u32.to_le_bytes());
        buf.extend_from_slice(&bits.to_le_bytes());
        buf.extend_from_slice(&nonce.to_le_bytes());
        buf.extend_from_slice(&0u64.to_le_bytes());
        use sha2::Digest as _;
        let h1 = sha2::Sha256::digest(&buf);
        let h2 = sha2::Sha256::digest(h1);
        // Compare as little-endian numbers vs target from (bits).
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
        app(),
        "/api/v1/spv/headers",
        serde_json::json!({
            "headers": [{
                "version": 1,
                "prev_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                "merkle_root": "0101010101010101010101010101010101010101010101010101010101010101",
                "timestamp": 1_700_000_000u32,
                "bits": bits,
                "nonce": nonce,
                "height": 0u32
            }]
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "adding genesis header should return 200"
    );
    assert_eq!(body["added"], 1, "one header should be added");
    assert_eq!(
        body["best_height"], 0,
        "best height should be 0 after genesis"
    );
}

// ─── DEX ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_dex_orders_empty_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/dex/orders").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn integration_dex_place_and_cancel_order() {
    let state = AppState::new();
    // Import a wallet, then unlock it — place_dex_order requires an unlocked wallet.
    {
        let (status, _) = post_json(
            build_router(state.clone()),
            "/api/v1/wallet/import",
            serde_json::json!({
                "wif": "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
                "passphrase": "test"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "wallet import should succeed");
        let (status, _) = post_json(
            build_router(state.clone()),
            "/api/v1/wallet/unlock",
            serde_json::json!({ "passphrase": "test", "timeout_secs": 300 }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "wallet unlock should succeed");
    }
    let router = build_router(state);

    // Place an order using the correct PlaceOrderRequest fields. The maker
    // address must match the imported wallet (the WIF above derives to
    // VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k) so the ownership check passes.
    let (status, body) = post_json(
        router.clone(),
        "/api/v1/dex/order",
        serde_json::json!({
            "maker_address": "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k",
            "offer_amount_satoshis": 1_000_000u64,
            "offer_asset": "VTR",
            "request_amount_satoshis": 100u64,
            "request_asset": "BTC",
            "expiry_secs": 3600u64,
            "passphrase": "test"
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "placing a DEX order should return 200; body={}",
        body
    );
    let order_id = body["order_id"]
        .as_str()
        .expect("response should have an order_id")
        .to_string();
    assert!(!order_id.is_empty(), "order_id should not be empty");

    // Verify it appears in the order book.
    let (status, body) = get(router.clone(), "/api/v1/dex/orders").await;
    assert_eq!(status, StatusCode::OK);
    let orders = body.as_array().unwrap();
    assert_eq!(orders.len(), 1, "order book should have one order");

    // Cancel the order.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/dex/order/{}", order_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "cancelling an order should return 200"
    );

    // Verify the order book is empty again.
    let (status, body) = get(router, "/api/v1/dex/orders").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.as_array().unwrap().is_empty(),
        "order book should be empty after cancel"
    );
}

// ─── Torrent ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_torrent_sessions_empty_on_fresh_node() {
    let (status, body) = get(app(), "/api/v1/torrent/sessions").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

// ─── Legacy claim ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_claim_check_unknown_address_returns_zero() {
    // ClaimCheckRequest uses `legacy_address`, not `address`.
    let (status, body) = post_json(
        app(),
        "/api/v1/claim/check",
        serde_json::json!({ "legacy_address": "1NotARealLegacyAddress" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["claimable_satoshis"], 0,
        "unknown address should have zero claimable balance"
    );
}

// ─── 404 handling ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn integration_unknown_route_returns_404() {
    let (status, _body) = get(app(), "/api/v1/does-not-exist").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unknown route should return 404"
    );
}

#[tokio::test]
async fn integration_block_not_found_returns_404() {
    let (status, _body) = get(
        app(),
        "/api/v1/blockchain/block/deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "missing block should return 404"
    );
}
