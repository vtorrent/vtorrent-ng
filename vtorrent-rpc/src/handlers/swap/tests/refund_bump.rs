use super::*;
use crate::models::VtrRefundBumpRequest;
use crate::refund_bump::{bump, history};

pub(super) async fn fixture(path: &std::path::Path) -> (AppState, String, [u8; 32]) {
    let (mut state, id, taker) = reconciliation_fixture().await;
    let now = vtorrent_core::time::now_secs();
    let mut order = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    order.expiry = now as u32 - 3600;
    *state.mock_time.write().await = Some(now - 48 * 3600);
    state.order_book.write().await.replace_order(order);
    match_recovery(&state, &id, &taker).await.unwrap();
    let funding = state.swaps.read().await[&id]
        .vtr_funding_tx
        .clone()
        .unwrap();
    confirm_vtr(&state, funding).await;
    *state.mock_time.write().await = Some(now);
    swap_refund_with_state(
        &state,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Vtr),
        },
    )
    .await
    .unwrap();
    state.swap_recovery_dir = Some(path.to_path_buf());
    let original = state.swaps.read().await[&id].vtr_refund_txid.unwrap();
    (state, id, original)
}

pub(super) fn request(id: &str, parent: [u8; 32], fee: u64) -> VtrRefundBumpRequest {
    VtrRefundBumpRequest {
        order_id: id.into(),
        replaces_txid: hex::encode(parent),
        total_fee_satoshis: fee,
        approve: true,
    }
}

#[tokio::test]
async fn refund_bump_preserves_lineage_and_retries_exact_bytes_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (state, id, original) = fixture(directory.path()).await;
    let original_bytes = bincode::serialize(
        state.swaps.read().await[&id]
            .vtr_refund_tx
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let first = bump(&state, request(&id, original, 20_000)).await.unwrap();
    let first_id = parse_hash32(&first.txid, "txid").unwrap();
    assert_ne!(first_id, original);
    assert!(state
        .mempool
        .lock()
        .await
        .get_transaction(&original)
        .is_none());
    assert!(state
        .mempool
        .lock()
        .await
        .get_transaction(&first_id)
        .is_some());
    let retry = bump(&state, request(&id, original, 20_000)).await.unwrap();
    assert_eq!(retry.txid, first.txid);
    assert_eq!(
        state.swaps.read().await[&id].vtr_refund_replacements.len(),
        1
    );
    let second = bump(&state, request(&id, first_id, 40_000)).await.unwrap();
    assert!(bump(&state, request(&id, original, 20_000)).await.is_err());
    let signed =
        bincode::serialize(state.swaps.read().await[&id].latest_vtr_refund().unwrap()).unwrap();
    let restored = restart_recovery(&state).await;
    assert_eq!(
        restored.swaps.read().await[&id]
            .vtr_refund_replacements
            .len(),
        2
    );
    assert_eq!(
        restored.swaps.read().await[&id].vtr_refund_txid,
        Some(original)
    );
    assert_eq!(
        bincode::serialize(
            restored.swaps.read().await[&id]
                .vtr_refund_tx
                .as_ref()
                .unwrap()
        )
        .unwrap(),
        original_bytes
    );
    let retry = swap_refund_with_state(
        &restored,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Vtr),
        },
    )
    .await
    .unwrap();
    assert_eq!(retry.txid, second.txid);
    assert_eq!(
        bincode::serialize(
            restored
                .mempool
                .lock()
                .await
                .get_transaction(&parse_hash32(&retry.txid, "id").unwrap())
                .unwrap()
        )
        .unwrap(),
        signed
    );
    let view = history(&restored, &id).await.unwrap();
    assert_eq!(view.versions.len(), 3);
    assert_eq!(view.versions[2].total_fee_satoshis, 40_000);
    let order = restored
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    let mut shortened = restored.swaps.read().await[&id].clone();
    shortened.vtr_refund_replacements.pop();
    assert!(
        crate::swap_recovery::persist(&restored, &order, Some(&shortened))
            .await
            .is_err()
    );
    let mut corrupt = restored.swaps.read().await[&id].clone();
    corrupt.vtr_refund_replacements[0].transaction.outputs[0].value += 1;
    assert!(
        crate::swap_recovery::persist(&restored, &order, Some(&corrupt))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn refund_bump_requires_approval_and_preserves_state_on_preflight_failures() {
    let directory = tempfile::tempdir().unwrap();
    let (state, id, original) = fixture(directory.path()).await;
    let mut unapproved = request(&id, original, 20_000);
    unapproved.approve = false;
    assert!(bump(&state, unapproved).await.is_err());
    for (parent, fee) in [
        (original, 10_000),
        (original, 1_000_000),
        (original, u64::MAX),
        ([0; 32], 20_000),
    ] {
        assert!(bump(&state, request(&id, parent, fee)).await.is_err());
    }
    *state.wallet_wif.write().await = Some(vtr_identity(3).0);
    assert!(bump(&state, request(&id, original, 20_000)).await.is_err());
    *state.wallet_wif.write().await = Some(vtr_identity(1).0);
    let mut inconsistent = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    inconsistent.vtr_amount -= 100;
    state
        .order_book
        .write()
        .await
        .replace_order(inconsistent.clone());
    assert!(bump(&state, request(&id, original, 20_000))
        .await
        .unwrap_err()
        .to_string()
        .contains("differs from the approved total"));
    inconsistent.vtr_amount += 100;
    state.order_book.write().await.replace_order(inconsistent);
    assert!(state.swaps.read().await[&id]
        .vtr_refund_replacements
        .is_empty());
    assert!(state
        .mempool
        .lock()
        .await
        .get_transaction(&original)
        .is_some());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    let guard = state.swaps.read().await;
    let mut unknown = guard[&id].vtr_refund_tx.clone().unwrap();
    drop(guard);
    unknown.outputs[0].value -= 50_000;
    state.mempool.lock().await.remove_transaction(&original);
    state
        .mempool
        .lock()
        .await
        .add_transaction_with_fee(unknown, 60_000)
        .unwrap();
    assert!(bump(&state, request(&id, original, 20_000)).await.is_err());
    assert!(state.swaps.read().await[&id]
        .vtr_refund_replacements
        .is_empty());
}

#[tokio::test]
async fn refund_bump_keeps_prepared_transaction_when_relay_fails() {
    let directory = tempfile::tempdir().unwrap();
    let (mut state, id, original) = fixture(directory.path()).await;
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    state.tx_submit = Some(sender);
    let error = bump(&state, request(&id, original, 20_000))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("relay is unavailable"));
    let latest = state.swaps.read().await[&id]
        .latest_vtr_refund()
        .unwrap()
        .txid();
    assert_ne!(latest, original);
    assert!(state
        .mempool
        .lock()
        .await
        .get_transaction(&latest)
        .is_some());
    let restored = restart_recovery(&state).await;
    let response = bump(&restored, request(&id, original, 20_000))
        .await
        .unwrap();
    assert_eq!(response.txid, hex::encode(latest));
    assert_eq!(
        restored.swaps.read().await[&id]
            .vtr_refund_replacements
            .len(),
        1
    );
}

#[tokio::test]
async fn refund_bump_cannot_replace_without_durable_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let (mut state, id, original) = fixture(directory.path()).await;
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, b"blocked").unwrap();
    state.swap_recovery_dir = Some(blocked);
    assert!(bump(&state, request(&id, original, 20_000)).await.is_err());
    assert!(state.swaps.read().await[&id]
        .vtr_refund_replacements
        .is_empty());
    assert!(state
        .mempool
        .lock()
        .await
        .get_transaction(&original)
        .is_some());
    state.swap_recovery_dir = None;
    assert!(bump(&state, request(&id, original, 20_000)).await.is_err());
}

#[tokio::test]
async fn refund_bump_serializes_concurrent_approvals_and_rejects_descendants() {
    let directory = tempfile::tempdir().unwrap();
    let (state, id, original) = fixture(directory.path()).await;
    let (first, second) = tokio::join!(
        bump(&state, request(&id, original, 20_000)),
        bump(&state, request(&id, original, 20_000)),
    );
    assert_eq!(first.unwrap().txid, second.unwrap().txid);
    assert_eq!(
        state.swaps.read().await[&id].vtr_refund_replacements.len(),
        1
    );
    let latest = state.swaps.read().await[&id]
        .latest_vtr_refund()
        .unwrap()
        .clone();
    let mut child = latest.clone();
    child.inputs[0].prev_txid = latest.txid();
    child.outputs[0].value -= 10_000;
    state
        .mempool
        .lock()
        .await
        .add_transaction_with_fee(child, 10_000)
        .unwrap();
    assert!(bump(&state, request(&id, latest.txid(), 40_000))
        .await
        .unwrap_err()
        .to_string()
        .contains("pending transaction spends this refund"));
    assert_eq!(
        state.swaps.read().await[&id].vtr_refund_replacements.len(),
        1
    );
}

#[tokio::test]
async fn refund_bump_api_requires_auth_and_explicit_fee_approval() {
    use tower::ServiceExt;
    let directory = tempfile::tempdir().unwrap();
    let (mut state, id, original) = fixture(directory.path()).await;
    state.rpc_api_key = Some("refund-fee-key".into());
    let app = crate::server::build_router(state.clone());
    let unauthorized = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/swap/vtr-refund-bump")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&request(&id, original, 20_000)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), axum::http::StatusCode::UNAUTHORIZED);
    let mut body = serde_json::to_value(request(&id, original, 20_000)).unwrap();
    body.as_object_mut().unwrap().remove("approve");
    let unapproved = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/swap/vtr-refund-bump")
                .header("Content-Type", "application/json")
                .header("X-API-Key", "refund-fee-key")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unapproved.status(), axum::http::StatusCode::BAD_REQUEST);
    assert!(state.swaps.read().await[&id]
        .vtr_refund_replacements
        .is_empty());
    let view = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v1/swap/{id}/vtr-refund-history"))
                .header("X-API-Key", "refund-fee-key")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(view.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(view.into_body(), 16_384)
        .await
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains(&hex::encode(original)));
    for secret in ["script_sig", "preimage", "wif", "transaction", "private"] {
        assert!(!text.contains(secret));
    }
}

#[tokio::test]
async fn refund_bump_observes_reorgs_and_the_original_winning() {
    use crate::swap_reconciliation::{reconcile, status};
    use vtorrent_node::atomic_swap::VtrSettlementState::*;
    let directory = tempfile::tempdir().unwrap();
    let (state, id, original) = fixture(directory.path()).await;
    let response = bump(&state, request(&id, original, 20_000)).await.unwrap();
    let latest = state.swaps.read().await[&id]
        .latest_vtr_refund()
        .unwrap()
        .clone();
    let before: Vec<_> = {
        let chain = state.chain.lock().await;
        (1..=chain.best_height())
            .map(|h| chain.get_block_at_height(h).unwrap().clone())
            .collect()
    };
    assert_eq!(status(&state, &id).await.unwrap().state, RefundPending);
    confirm_vtr(&state, latest).await;
    assert_eq!(status(&state, &id).await.unwrap().state, RefundConfirming);
    add_confirmations(&state, 5).await;
    assert_eq!(reconcile(&state, &id).await.unwrap().state, Refunded);
    {
        let mut chain = state.chain.lock().await;
        *chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
        for block in before {
            chain.add_block(block).unwrap();
        }
    }
    let reorg = reconcile(&state, &id).await.unwrap();
    assert_eq!(reorg.state, RefundPrepared);
    assert_eq!(reorg.reorg_count, 1);
    assert_eq!(
        bump(&state, request(&id, original, 20_000))
            .await
            .unwrap()
            .txid,
        response.txid
    );
    let old = state.swaps.read().await[&id].vtr_refund_tx.clone().unwrap();
    confirm_vtr(&state, old).await;
    assert_eq!(status(&state, &id).await.unwrap().state, RefundConfirming);
    let retry = swap_refund_with_state(
        &state,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Vtr),
        },
    )
    .await
    .unwrap();
    assert_eq!(retry.txid, hex::encode(original));
    assert!(bump(
        &state,
        request(&id, parse_hash32(&response.txid, "id").unwrap(), 40_000)
    )
    .await
    .is_err());
    assert_eq!(
        state.swaps.read().await[&id].vtr_refund_replacements.len(),
        1
    );
}
