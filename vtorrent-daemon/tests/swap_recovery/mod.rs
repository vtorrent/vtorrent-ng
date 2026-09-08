use super::*;
use vtorrent_node::{
    atomic_swap::{AtomicSwap, SwapOrder},
    chain::{BlockAcceptance, Chain},
};
use vtorrent_rpc::{models::MatchOrderRequest, state::AppState};

const PASSPHRASE: &str = "regtest swap recovery only";

fn identity(tag: u8) -> (zeroize::Zeroizing<String>, String) {
    let key = vtorrent_core::keys::PrivateKey::from_bytes([tag; 32], true).unwrap();
    let address = vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address(
        &key.public_key().unwrap().serialize(),
    )
    .unwrap();
    (key.to_wif(198).into(), address)
}

async fn historical_funding(maker_dir: &Path, peer_dir: &Path) -> String {
    let (wif, maker) = identity(1);
    let now = vtorrent_core::time::now_secs();
    let mut state = AppState::new();
    *state.chain.lock().await = Chain::new_regtest().unwrap();
    state
        .chain
        .lock()
        .await
        .mint_to_address(&maker, 2_000_000)
        .unwrap();
    *state.wallet_wif.write().await = Some(wif.clone());
    *state.wallet_change_address.write().await = Some(maker.clone());
    *state.wallet_unlock_expiry.write().await = Some(0);
    *state.mock_time.write().await = Some(now - 48 * 3600);
    state.swap_recovery_dir = Some(maker_dir.join("swaps"));
    let secret = AtomicSwap::new();
    let mut order = SwapOrder::new(maker.clone(), 1_000_000, "BTC".into(), 100_000, 48 * 3600);
    order.expiry = u32::try_from(now - 3600).unwrap();
    order.hash_lock = Some(secret.hash_lock);
    order.preimage = Some(secret.preimage);
    let id = hex::encode(order.order_id);
    state.order_book.write().await.add_order(order);
    vtorrent_rpc::handlers::match_dex_order_with_wif(
        &state,
        MatchOrderRequest {
            order_id: id.clone(),
            taker_address: identity(2).1,
            passphrase: PASSPHRASE.to_owned().into(),
            otp_code: None,
        },
        &wif,
    )
    .await
    .unwrap();
    let funding = state.swaps.read().await[&id]
        .vtr_funding_tx
        .clone()
        .unwrap();
    let mut chain = state.chain.lock().await;
    let mut template = Chain::new_regtest().unwrap();
    template
        .add_block(chain.get_block_at_height(1).unwrap().clone())
        .unwrap();
    template.mint_to_address(&maker, 1).unwrap();
    let mut block = template.get_block_at_height(2).unwrap().clone();
    block.transactions.push(funding);
    block.header.merkle_root = block.compute_merkle_root();
    assert!(matches!(
        chain.add_block(block).unwrap(),
        BlockAcceptance::MainChain { .. }
    ));
    for _ in 0..5 {
        chain.mint_to_address(&maker, 1).unwrap();
    }
    let blocks: Vec<_> = (0..=chain.best_height())
        .map(|height| chain.get_block_at_height(height).unwrap().clone())
        .collect();
    for directory in [maker_dir, peer_dir] {
        std::fs::create_dir_all(directory).unwrap();
        BlockStore::open(directory.join("chain.db"))
            .unwrap()
            .rebuild_from_regtest_blocks(&blocks)
            .unwrap();
    }
    id
}

async fn post(daemon: &Daemon, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let response = daemon
        .client
        .post(format!("{}{path}", daemon.rpc))
        .timeout(Duration::from_secs(30))
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path} failed: {error}; {}", daemon.logs()));
    let status = response.status();
    (status, response.json().await.unwrap())
}

async fn post_ok(daemon: &Daemon, path: &str, body: Value) -> Value {
    let (status, response) = post(daemon, path, body).await;
    assert!(
        status.is_success(),
        "POST {path}: {status} {response}; {}",
        daemon.logs()
    );
    response
}

async fn unlock(daemon: &Daemon) {
    let result = post_ok(
        daemon,
        "/api/v1/wallet/unlock",
        json!({
            "passphrase": PASSPHRASE, "timeout_secs": 0,
        }),
    )
    .await;
    assert_eq!(result["success"], true);
}

async fn wait_mempool(daemon: &Daemon, expected: &str) {
    let mut last = String::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match rpc_polling::get_json(&daemon.client, &format!("{}/api/v1/mempool", daemon.rpc))
                .await
            {
                Ok(pool) => {
                    if pool["txids"] == json!([expected]) {
                        return;
                    }
                    last = pool.to_string();
                }
                Err(error) if error.is_timeout() || error.is_connect() => last = error.to_string(),
                Err(error) => panic!("mempool RPC failed: {error}; {}", daemon.logs()),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "mempool did not contain only {expected}: {last}; {}",
            daemon.logs()
        )
    });
}

#[tokio::test]
async fn daemon_rpc_refund_bump_survives_restart_and_replaces_peer_mempool() {
    let directory = tempfile::tempdir().unwrap();
    let maker_dir = directory.path().join("maker");
    let peer_dir = directory.path().join("peer");
    let id = historical_funding(&maker_dir, &peer_dir).await;
    let history_path = format!("/api/v1/swap/{id}/vtr-refund-history");
    let status_path = format!("/api/v1/swap/{id}/status");
    let refund_request = json!({"order_id": id, "leg": "vtr"});
    let maker = Daemon::start(&maker_dir, "initial.log", None).await;
    let peer = Daemon::start(&peer_dir, "initial.log", Some(&maker.p2p)).await;
    let imported = post_ok(
        &maker,
        "/api/v1/wallet/import",
        json!({
            "wif": identity(1).0.as_str(), "passphrase": PASSPHRASE,
        }),
    )
    .await;
    assert_eq!(imported["address"], identity(1).1);
    assert_eq!(maker.get("/api/v1/dex/orders").await, json!([]));
    unlock(&maker).await;
    assert_eq!(maker.get(&status_path).await["vtr"]["state"], "funded");
    let original = post_ok(&maker, "/api/v1/swap/refund", refund_request.clone()).await;
    let original_id = original["txid"].as_str().unwrap();
    wait_mempool(&peer, original_id).await;
    peer.stop(false).await;
    let bump_request = json!({
        "order_id": id, "replaces_txid": original_id,
        "total_fee_satoshis": 20_000, "approve": true,
    });
    let mut unapproved = bump_request.clone();
    unapproved["approve"] = json!(false);
    assert_eq!(
        post(&maker, "/api/v1/swap/vtr-refund-bump", unapproved)
            .await
            .0,
        reqwest::StatusCode::BAD_REQUEST
    );
    wait_mempool(&maker, original_id).await;
    let replacement = post_ok(&maker, "/api/v1/swap/vtr-refund-bump", bump_request.clone()).await;
    let replacement_id = replacement["txid"].as_str().unwrap();
    assert_ne!(replacement_id, original_id);
    wait_mempool(&maker, replacement_id).await;
    let tx_path = format!("/api/v1/blockchain/tx/{replacement_id}");
    let signed = maker.get(&tx_path).await;
    assert_eq!(signed["outputs"][0]["value_satoshis"], 980_000);
    assert!(signed["block_hash"].is_null());
    let history = maker.get(&history_path).await;
    assert_eq!(history["versions"].as_array().unwrap().len(), 2);
    assert_eq!(history["versions"][1]["replaces_txid"], original_id);
    assert_eq!(history["versions"][1]["total_fee_satoshis"], 20_000);
    maker.stop(false).await;

    let peer = Daemon::start(&peer_dir, "restored.log", None).await;
    wait_mempool(&peer, original_id).await;
    let maker = Daemon::start(&maker_dir, "restored.log", Some(&peer.p2p)).await;
    wait_mempool(&maker, replacement_id).await;
    assert_eq!(maker.get("/api/v1/dex/orders").await, json!([]));
    assert!(
        !post(&maker, "/api/v1/swap/vtr-refund-bump", bump_request.clone())
            .await
            .0
            .is_success()
    );
    assert!(!post(
        &maker,
        "/api/v1/wallet/unlock",
        json!({
            "passphrase": "incorrect", "timeout_secs": 0,
        })
    )
    .await
    .0
    .is_success());
    assert_eq!(maker.get("/api/v1/dex/orders").await, json!([]));
    unlock(&maker).await;
    assert_eq!(maker.get(&history_path).await, history);
    let retried = post_ok(&maker, "/api/v1/swap/vtr-refund-bump", bump_request).await;
    assert_eq!(retried["txid"], replacement_id);
    let refunded = post_ok(&maker, "/api/v1/swap/refund", refund_request).await;
    assert_eq!(refunded["txid"], replacement_id);
    wait_mempool(&peer, replacement_id).await;
    assert_eq!(maker.get(&tx_path).await, signed);
    assert_eq!(peer.get(&tx_path).await, signed);
    assert_eq!(maker.get(&history_path).await, history);
    assert_eq!(
        maker.get(&status_path).await["vtr"]["state"],
        "refund_pending"
    );
    let reconciled = post_ok(&maker, "/api/v1/swap/reconcile", json!({"order_id": id})).await;
    assert_eq!(reconciled["vtr"]["state"], "refund_pending");
    assert_eq!(reconciled["vtr"]["pending_spend_txid"], replacement_id);
    assert!(reconciled["vtr"]["spend"].is_null());
    peer.stop(false).await;
    maker.stop(false).await;
}
