use super::*;
use bitcoin::hashes::Hash;
use std::sync::Mutex;
use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, SwapOrder};

const NOW: u64 = 1_800_000_000;

fn vtr_identity(byte: u8) -> (zeroize::Zeroizing<String>, String) {
    let key = vtorrent_core::keys::PrivateKey::from_bytes([byte; 32], true).unwrap();
    let address = vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address(
        &key.public_key().unwrap().serialize(),
    )
    .unwrap();
    (key.to_wif(198).into(), address)
}

async fn recovery_fixture(path: &std::path::Path) -> (AppState, String, String) {
    let (wif, maker) = vtr_identity(1);
    let (_, taker) = vtr_identity(2);
    let mut state = AppState::new();
    *state.chain.lock().await = vtorrent_node::chain::Chain::new_regtest().unwrap();
    state
        .chain
        .lock()
        .await
        .mint_to_address(&maker, 2_000_000)
        .unwrap();
    state.swap_recovery_dir = Some(path.to_path_buf());
    *state.wallet_wif.write().await = Some(wif);
    *state.wallet_change_address.write().await = Some(maker.clone());
    *state.wallet_unlock_expiry.write().await = Some(0);
    *state.mock_time.write().await = Some(NOW);
    let secret = AtomicSwap::new();
    let mut order = SwapOrder::new(maker, 1_000_000, "BTC".into(), 100_000, 48 * 3600);
    order.expiry = NOW as u32 + 48 * 3600;
    order.preimage = Some(secret.preimage);
    order.hash_lock = Some(secret.hash_lock);
    let id = hex::encode(order.order_id);
    state.order_book.write().await.add_order(order);
    (state, id, taker)
}

async fn restart_recovery(state: &AppState) -> AppState {
    let mut restored = AppState::new_with_shared(
        state.chain.clone(),
        Arc::new(tokio::sync::Mutex::new(
            vtorrent_node::mempool::Mempool::new(10_000),
        )),
    );
    restored.swap_recovery_dir = state.swap_recovery_dir.clone();
    *restored.mock_time.write().await = *state.mock_time.read().await;
    let wif = state.wallet_wif.read().await.clone().unwrap();
    crate::swap_recovery::restore_with_wif(&restored, &wif)
        .await
        .unwrap();
    *restored.wallet_wif.write().await = Some(wif);
    *restored.wallet_change_address.write().await =
        state.wallet_change_address.read().await.clone();
    *restored.wallet_unlock_expiry.write().await = Some(0);
    restored
}

async fn match_recovery(state: &AppState, id: &str, taker: &str) -> RpcResult<MatchOrderResponse> {
    let wif = state.wallet_wif.read().await.clone().unwrap();
    match_dex_order_with_wif(
        state,
        MatchOrderRequest {
            order_id: id.into(),
            taker_address: taker.into(),
            passphrase: String::new().into(),
            otp_code: None,
        },
        &wif,
    )
    .await
}

#[tokio::test]
async fn encrypted_maker_secret_and_signed_funding_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (state, id, taker) = recovery_fixture(directory.path()).await;
    let before = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    crate::swap_recovery::persist(&state, &before, None)
        .await
        .unwrap();
    let restored_open = restart_recovery(&state).await;
    assert_eq!(
        restored_open
            .order_book
            .read()
            .await
            .get_order(&id)
            .unwrap()
            .preimage,
        before.preimage
    );
    let funded = match_recovery(&restored_open, &id, &taker).await.unwrap();
    let raw = bincode::serialize(
        restored_open.swaps.read().await[&id]
            .vtr_funding_tx
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let restored = restart_recovery(&restored_open).await;
    assert_eq!(restored.mempool.lock().await.size(), 0);
    assert!(match_recovery(&restored, &id, &vtr_identity(3).1)
        .await
        .is_err());
    let retried = match_recovery(&restored, &id, &taker).await.unwrap();
    assert_eq!(retried.funding_txid, funded.funding_txid);
    let txid = parse_hash32(&retried.funding_txid, "txid").unwrap();
    assert_eq!(
        bincode::serialize(
            restored
                .mempool
                .lock()
                .await
                .get_transaction(&txid)
                .unwrap()
        )
        .unwrap(),
        raw
    );
    assert_eq!(
        restored
            .order_book
            .read()
            .await
            .get_order(&id)
            .unwrap()
            .preimage,
        before.preimage
    );
}

#[tokio::test]
async fn vtr_funding_persistence_failure_prevents_admission_and_preserves_retry() {
    let directory = tempfile::tempdir().unwrap();
    let blocker = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
    let (mut state, id, taker) = recovery_fixture(blocker.path()).await;
    assert!(match_recovery(&state, &id, &taker).await.is_err());
    assert_eq!(state.mempool.lock().await.size(), 0);
    let prepared = bincode::serialize(
        state.swaps.read().await[&id]
            .vtr_funding_tx
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    state.swap_recovery_dir = Some(directory.path().join("fixed"));
    let result = match_recovery(&state, &id, &taker).await.unwrap();
    let txid = parse_hash32(&result.funding_txid, "txid").unwrap();
    assert_eq!(
        bincode::serialize(state.mempool.lock().await.get_transaction(&txid).unwrap()).unwrap(),
        prepared
    );
}

#[tokio::test]
async fn corrupt_recovery_is_rejected_without_overwrite_or_partial_restore() {
    let directory = tempfile::tempdir().unwrap();
    let (state, id, _) = recovery_fixture(directory.path()).await;
    let order = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    crate::swap_recovery::persist(&state, &order, None)
        .await
        .unwrap();
    let namespace = std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let path = namespace.join(format!("{id}.json"));
    let encrypted = std::fs::read(&path).unwrap();
    let plaintext = serde_json::to_vec(&order).unwrap();
    assert!(!encrypted.windows(plaintext.len()).any(|w| w == plaintext));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, b"corrupt").unwrap();
    let mut restored = AppState::new_with_shared(
        state.chain.clone(),
        Arc::new(tokio::sync::Mutex::new(
            vtorrent_node::mempool::Mempool::new(10_000),
        )),
    );
    restored.swap_recovery_dir = state.swap_recovery_dir.clone();
    let wif = state.wallet_wif.read().await.clone().unwrap();
    assert!(crate::swap_recovery::restore_with_wif(&restored, &wif)
        .await
        .is_err());
    assert!(restored.order_book.read().await.get_order(&id).is_none());
    assert!(crate::swap_recovery::persist(&state, &order, None)
        .await
        .is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
}

async fn mine_recovery_transaction(
    state: &AppState,
    transaction: vtorrent_node::block::Transaction,
    timestamp: u32,
) {
    let address = state.wallet_change_address.read().await.clone().unwrap();
    let mut chain = state.chain.lock().await;
    chain.mint_to_address(&address, 1).unwrap();
    let mut block = chain
        .get_block_at_height(chain.best_height())
        .unwrap()
        .clone();
    let previous: Vec<_> = (1..chain.best_height())
        .map(|height| chain.get_block_at_height(height).unwrap().clone())
        .collect();
    *chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
    for block in previous {
        chain.add_block(block).unwrap();
    }
    block.transactions.push(transaction);
    block.header.timestamp = timestamp.max(
        chain
            .get_block_at_height(chain.best_height())
            .unwrap()
            .header
            .timestamp
            + 1,
    );
    block.header.merkle_root = block.compute_merkle_root();
    chain.add_block(block).unwrap();
}

async fn reconciliation_fixture() -> (AppState, String, String) {
    let (mut state, id, taker) = recovery_fixture(std::path::Path::new("unused")).await;
    state.swap_recovery_dir = None;
    (state, id, taker)
}

async fn confirm_vtr(state: &AppState, transaction: vtorrent_node::block::Transaction) {
    let txid = transaction.txid();
    mine_recovery_transaction(state, transaction, vtorrent_core::time::now_secs() as u32).await;
    state.mempool.lock().await.remove_transaction(&txid);
}

async fn add_confirmations(state: &AppState, count: u32) {
    let address = state.wallet_change_address.read().await.clone().unwrap();
    for _ in 0..count {
        state
            .chain
            .lock()
            .await
            .mint_to_address(&address, 1)
            .unwrap();
    }
}

#[tokio::test]
async fn reconciliation_distinguishes_submissions_confirmations_and_reorgs() {
    use crate::swap_reconciliation::{reconcile, status};
    use vtorrent_node::atomic_swap::VtrSettlementState::*;
    let (state, id, taker) = reconciliation_fixture().await;
    match_recovery(&state, &id, &taker).await.unwrap();
    assert_eq!(status(&state, &id).await.unwrap().state, FundingPending);
    let funding = state.swaps.read().await[&id]
        .vtr_funding_tx
        .clone()
        .unwrap();
    state
        .mempool
        .lock()
        .await
        .remove_transaction(&funding.txid());
    assert_eq!(status(&state, &id).await.unwrap().state, FundingPrepared);
    confirm_vtr(&state, funding).await;
    assert_eq!(status(&state, &id).await.unwrap().state, FundingConfirming);
    add_confirmations(&state, 5).await;
    assert_eq!(reconcile(&state, &id).await.unwrap().state, Funded);
    let before_claim: Vec<_> = {
        let chain = state.chain.lock().await;
        (1..=chain.best_height())
            .map(|h| chain.get_block_at_height(h).unwrap().clone())
            .collect()
    };
    let preimage = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .preimage
        .unwrap();
    vtr_claim_with_state(
        &state,
        VtrClaimRequest {
            order_id: id.clone(),
            preimage: hex::encode(preimage),
            taker_wif: vtr_identity(2).0,
        },
    )
    .await
    .unwrap();
    assert_eq!(status(&state, &id).await.unwrap().state, ClaimPending);
    let claim = state.swaps.read().await[&id].vtr_claim_tx.clone().unwrap();
    confirm_vtr(&state, claim.clone()).await;
    assert_eq!(status(&state, &id).await.unwrap().state, ClaimConfirming);
    add_confirmations(&state, 5).await;
    let settled = reconcile(&state, &id).await.unwrap();
    assert_eq!(settled.state, Claimed);
    assert_eq!(settled.spend.unwrap().confirmations, 6);
    {
        let mut chain = state.chain.lock().await;
        *chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
        for block in before_claim {
            chain.add_block(block).unwrap();
        }
    }
    let reorged = reconcile(&state, &id).await.unwrap();
    assert_eq!(reorged.state, ClaimPrepared);
    assert!(reorged.spend.is_none());
    assert_eq!(reorged.reorg_count, 1);
    assert_eq!(reconcile(&state, &id).await.unwrap().reorg_count, 1);
    assert_eq!(
        state.swaps.read().await[&id].vtr_claim_txid,
        Some(claim.txid())
    );
    confirm_vtr(&state, claim).await;
    assert_eq!(status(&state, &id).await.unwrap().state, ClaimConfirming);
}

#[tokio::test]
async fn reconciliation_does_not_infer_refund_from_expiry_and_detects_external_spends() {
    use crate::swap_reconciliation::status;
    use vtorrent_node::atomic_swap::VtrSettlementState::*;
    let (state, id, taker) = reconciliation_fixture().await;
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
    add_confirmations(&state, 5).await;
    assert_eq!(status(&state, &id).await.unwrap().state, Funded);
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
    assert_eq!(status(&state, &id).await.unwrap().state, RefundPending);
    let refund = state.swaps.read().await[&id].vtr_refund_tx.clone().unwrap();
    state
        .swaps
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .vtr_refund_txid = None;
    assert_eq!(
        status(&state, &id).await.unwrap().state,
        CompetingSpendPending
    );
    confirm_vtr(&state, refund.clone()).await;
    let external = status(&state, &id).await.unwrap();
    assert_eq!(external.state, SpentElsewhere);
    assert_eq!(external.spend.unwrap().txid, hex::encode(refund.txid()));
    state
        .swaps
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .vtr_refund_txid = Some(refund.txid());
    assert_eq!(status(&state, &id).await.unwrap().state, RefundConfirming);
    add_confirmations(&state, 5).await;
    assert_eq!(status(&state, &id).await.unwrap().state, Refunded);
}

#[tokio::test]
async fn reconciliation_rejects_wrong_contract_and_persists_observation() {
    use crate::swap_reconciliation::{reconcile, status};
    use vtorrent_node::atomic_swap::VtrSettlementState::*;
    let directory = tempfile::tempdir().unwrap();
    let (state, id, taker) = recovery_fixture(directory.path()).await;
    match_recovery(&state, &id, &taker).await.unwrap();
    let recorded = reconcile(&state, &id).await.unwrap();
    assert_eq!(recorded.state, FundingPending);
    let restored = restart_recovery(&state).await;
    assert_eq!(
        restored.swaps.read().await[&id].vtr_observation.as_ref(),
        Some(&recorded)
    );
    // The fresh status cannot trust the persisted mempool observation after restart.
    assert_eq!(status(&restored, &id).await.unwrap().state, FundingPrepared);
    let mut wrong = restored
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    wrong.hash_lock = Some([99; 32]);
    restored.order_book.write().await.replace_order(wrong);
    assert_eq!(status(&restored, &id).await.unwrap().state, InvalidFunding);
    assert!(reconcile(&restored, &id).await.is_err());
}

#[tokio::test]
async fn reconciliation_api_is_authenticated_and_does_not_expose_secrets() {
    use tower::ServiceExt;
    let (mut state, id, taker) = reconciliation_fixture().await;
    state.rpc_api_key = Some("reconciliation-test-key".into());
    match_recovery(&state, &id, &taker).await.unwrap();
    let app = crate::server::build_router(state);
    let uri = format!("/api/v1/swap/{id}/status");
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri(&uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .header("X-API-Key", "reconciliation-test-key")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16_384)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["order_id"], id);
    assert_eq!(body["vtr"]["state"], "funding_pending");
    assert_eq!(body["btc_reconciled"], false);
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    for forbidden in ["preimage", "wif", "script_sig", "passphrase"] {
        assert!(!text.contains(forbidden));
    }
}

#[tokio::test]
async fn vtr_claim_and_refund_retry_exact_signed_transactions_after_restart() {
    for claim in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let (state, id, taker) = recovery_fixture(directory.path()).await;
        let wall_now = vtorrent_core::time::now_secs();
        let mut timed_order = state
            .order_book
            .read()
            .await
            .get_order(&id)
            .unwrap()
            .clone();
        timed_order.expiry = if claim {
            wall_now as u32 + 48 * 3600
        } else {
            wall_now as u32 - 3600
        };
        *state.mock_time.write().await = Some(u64::from(timed_order.expiry) - 48 * 3600);
        state.order_book.write().await.replace_order(timed_order);
        match_recovery(&state, &id, &taker).await.unwrap();
        let funding = state.swaps.read().await[&id]
            .vtr_funding_tx
            .clone()
            .unwrap();
        let order = state
            .order_book
            .read()
            .await
            .get_order(&id)
            .unwrap()
            .clone();
        let now = wall_now;
        mine_recovery_transaction(&state, funding, now as u32).await;
        *state.mock_time.write().await = Some(now);
        if claim {
            let invalid = vtr_claim_with_state(
                &state,
                VtrClaimRequest {
                    order_id: id.clone(),
                    preimage: hex::encode(order.preimage.unwrap()),
                    taker_wif: vtr_identity(3).0,
                },
            )
            .await;
            assert!(invalid.is_err());
            assert!(state.swaps.read().await[&id].vtr_claim_txid.is_none());
        }
        let action = async |s: &AppState| {
            if claim {
                vtr_claim_with_state(
                    s,
                    VtrClaimRequest {
                        order_id: id.clone(),
                        preimage: hex::encode(order.preimage.unwrap()),
                        taker_wif: vtr_identity(2).0,
                    },
                )
                .await
            } else {
                swap_refund_with_state(
                    s,
                    SwapRefundRequest {
                        order_id: id.clone(),
                        leg: Some(SwapLeg::Vtr),
                    },
                )
                .await
            }
        };
        let first = action(&state).await.unwrap();
        let txid = parse_hash32(&first.txid, "txid").unwrap();
        let raw =
            bincode::serialize(state.mempool.lock().await.get_transaction(&txid).unwrap()).unwrap();
        let restored = restart_recovery(&state).await;
        assert_eq!(restored.mempool.lock().await.size(), 0);
        let retry = action(&restored).await.unwrap();
        assert_eq!(retry.txid, first.txid);
        assert_eq!(
            bincode::serialize(
                restored
                    .mempool
                    .lock()
                    .await
                    .get_transaction(&txid)
                    .unwrap()
            )
            .unwrap(),
            raw
        );
        let other = swap_refund_with_state(
            &restored,
            SwapRefundRequest {
                order_id: id.clone(),
                leg: Some(SwapLeg::Vtr),
            },
        )
        .await;
        assert_eq!(other.is_err(), claim);
    }
}

async fn claim_fixture() -> (AppState, String) {
    let (state, id, refund_address) = fixture().await;
    fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address,
        },
        async |raw: &[u8]| Ok(txid(raw)),
    )
    .await
    .unwrap();
    *state.btc_wallet.write().await = Some(vtorrent_btc::wallet::BtcWallet::with_network(
        [1; 64],
        bitcoin::Network::Regtest,
    ));
    (state, id)
}

#[tokio::test]
async fn btc_claim_verification_failure_never_broadcasts_or_updates_state() {
    let (state, id) = claim_fixture().await;
    let before = state.swaps.read().await[&id].status.clone();
    let result = claim_btc_with_verification(
        &state,
        BtcClaimRequest {
            order_id: id.clone(),
        },
        async |_: &vtorrent_btc::htlc::BtcHtlc, _| {
            Err(RpcError::BadRequest("unconfirmed or spent funding".into()))
        },
        async |_: &[u8]| panic!("must not disclose secret without verified funding"),
    )
    .await;
    assert!(result.is_err());
    let swaps = state.swaps.read().await;
    assert_eq!(swaps[&id].status, before);
    assert!(swaps[&id].btc_claim_txid.is_none());
}

#[tokio::test]
async fn btc_claim_verifies_exact_public_terms_before_revealing_secret() {
    let (state, id) = claim_fixture().await;
    let funding = state.swaps.read().await[&id].btc_funding_txid.unwrap();
    let order = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    let verified = std::sync::atomic::AtomicBool::new(false);
    let result = claim_btc_with_verification(
        &state,
        BtcClaimRequest {
            order_id: id.clone(),
        },
        async |htlc: &vtorrent_btc::htlc::BtcHtlc, txid| {
            assert_eq!(txid, funding);
            assert_eq!(htlc.hash_lock, order.hash_lock.unwrap());
            assert_eq!(htlc.amount, order.target_amount);
            assert_eq!(Some(&htlc.recipient), order.maker_btc_address.as_ref());
            verified.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        async |raw: &[u8]| {
            assert!(verified.load(std::sync::atomic::Ordering::SeqCst));
            let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(raw).unwrap();
            assert!(tx.input[0]
                .witness
                .iter()
                .any(|item| item == order.preimage.unwrap()));
            Ok(tx.compute_txid().to_byte_array())
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, "BtcClaimSubmitted");
    assert!(state.swaps.read().await[&id].btc_claim_txid.is_some());
}

#[tokio::test]
async fn btc_claim_rechecks_deadline_after_network_verification() {
    let (state, id) = claim_fixture().await;
    let expiry = state.swaps.read().await[&id].btc_expiry;
    let result = claim_btc_with_verification(
        &state,
        BtcClaimRequest {
            order_id: id.clone(),
        },
        async |_: &vtorrent_btc::htlc::BtcHtlc, _| {
            *state.mock_time.write().await = Some(u64::from(
                expiry - vtorrent_wallet_service::swap_policy::MIN_BTC_SWAP_WINDOW,
            ));
            Ok(())
        },
        async |_: &[u8]| panic!("must not reveal at deadline"),
    )
    .await;
    assert!(result.unwrap_err().to_string().contains("window expired"));
    assert!(state.swaps.read().await[&id].btc_claim_txid.is_none());
}

#[tokio::test]
async fn btc_claim_rejects_order_mismatch_before_network_access() {
    for field in 0..4 {
        let (state, id) = claim_fixture().await;
        {
            let mut swaps = state.swaps.write().await;
            let swap = swaps.get_mut(&id).unwrap();
            match field {
                0 => swap.btc_amount += 1,
                1 => swap.hash_lock[0] ^= 1,
                2 => swap.btc_expiry = NOW as u32 + 48 * 3600,
                _ => swap.preimage = Some([0; 32]),
            }
        }
        assert!(claim_btc_with_verification(
            &state,
            BtcClaimRequest { order_id: id },
            async |_: &vtorrent_btc::htlc::BtcHtlc, _| panic!("invalid terms must fail locally"),
            async |_: &[u8]| panic!("invalid terms must not reveal secret"),
        )
        .await
        .is_err());
    }
}

#[tokio::test]
async fn btc_claim_rechecks_vtr_funding_after_network_verification() {
    let (state, id) = claim_fixture().await;
    let result = claim_btc_with_verification(
        &state,
        BtcClaimRequest {
            order_id: id.clone(),
        },
        async |_: &vtorrent_btc::htlc::BtcHtlc, _| {
            *state.chain.lock().await = vtorrent_node::chain::Chain::new_regtest().unwrap();
            Ok(())
        },
        async |_: &[u8]| panic!("VTR funding disappeared during verification"),
    )
    .await;
    assert!(result.is_err());
    assert!(state.swaps.read().await[&id].btc_claim_txid.is_none());
}

fn txid(raw: &[u8]) -> [u8; 32] {
    let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(raw).unwrap();
    tx.compute_txid().to_byte_array()
}

async fn fixture() -> (AppState, String, String) {
    let maker = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
    let taker = "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh";
    let mut btc = vtorrent_btc::wallet::BtcWallet::with_network([2; 64], bitcoin::Network::Regtest);
    btc.next_address().unwrap();
    let refund_address = btc.current_address().unwrap();
    btc.add_utxo(vtorrent_btc::utxo::Utxo {
        txid: "11".repeat(32),
        vout: 0,
        value: 200_000,
        address: refund_address.clone(),
        height: 100,
    });
    let maker_btc =
        vtorrent_btc::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let secret = AtomicSwap::new();
    let mut order = SwapOrder::new(maker.into(), 1_000_000, "BTC".into(), 100_000, 48 * 3600);
    order.expiry = NOW as u32 + 48 * 3600;
    order.hash_lock = Some(secret.hash_lock);
    order.preimage = Some(secret.preimage);
    order.taker_address = Some(taker.into());
    order.maker_btc_address = Some(maker_btc.current_address().unwrap());
    let htlc = Htlc::with_expiry(
        secret.hash_lock,
        taker.into(),
        maker.into(),
        order.expiry,
        order.vtr_amount,
    )
    .unwrap();
    let mut chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
    chain.mint_to_address(maker, order.vtr_amount).unwrap();
    let mut block = chain.get_block_at_height(1).unwrap().clone();
    chain = vtorrent_node::chain::Chain::new_regtest().unwrap();
    block.transactions[0].outputs[0].script_pubkey = htlc.build_script().unwrap();
    block.header.merkle_root = block.compute_merkle_root();
    order.funding_txid = Some(block.transactions[0].txid());
    chain.add_block(block).unwrap();
    for _ in 0..5 {
        chain.mint_to_address(maker, 1).unwrap();
    }
    let state = AppState::new();
    *state.chain.lock().await = chain;
    *state.mock_time.write().await = Some(NOW);
    *state.btc_network.write().await = bitcoin::Network::Regtest;
    *state.btc_wallet.write().await = Some(btc);
    let id = hex::encode(order.order_id);
    let mut swap = SwapState::new(order.order_id, secret.hash_lock);
    swap.vtr_funding_txid = order.funding_txid;
    swap.status = SwapStatus::VtrFunded;
    state.swaps.write().await.insert(id.clone(), swap);
    state.order_book.write().await.add_order(order);
    (state, id, refund_address)
}

async fn btc_reconciliation_fixture() -> (AppState, String, vtorrent_btc::sync::SwapScan) {
    let (state, id, refund) = fixture().await;
    fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund,
        },
        async |raw: &[u8]| Ok(txid(raw)),
    )
    .await
    .unwrap();
    let funding = state.swaps.read().await[&id].btc_funding_txid.unwrap();
    let scan = vtorrent_btc::sync::SwapScan {
        tip_hash: [80; 32],
        tip_height: 20,
        scan_start: 1,
        funding: Some(vtorrent_btc::sync::SwapAnchor {
            txid: funding,
            block_hash: [90; 32],
            height: 15,
        }),
        spend: None,
        invalid_funding: false,
        coinbase: false,
        invalidated_anchor: false,
    };
    (state, id, scan)
}

#[tokio::test]
async fn btc_reconciliation_distinguishes_confirmations_expiry_and_reorgs() {
    use crate::btc_reconciliation::reconcile_with_scan;
    use vtorrent_node::atomic_swap::BtcSettlementState::*;
    let (state, id, mut scan) = btc_reconciliation_fixture().await;
    let raw = state.swaps.read().await[&id].btc_funding_raw.clone();
    let first = reconcile_with_scan(&state, &id, async |_, _, previous| {
        assert!(previous.is_empty());
        Ok(scan.clone())
    })
    .await
    .unwrap();
    assert_eq!(first.state, Funded);
    assert_eq!(first.funding.as_ref().unwrap().confirmations, 6);
    // Expiry and a recorded submission alone are not refund evidence.
    *state.mock_time.write().await = Some(NOW + 100_000);
    state
        .swaps
        .write()
        .await
        .get_mut(&id)
        .unwrap()
        .btc_refund_txid = Some([7; 32]);
    let expired = reconcile_with_scan(&state, &id, async |_, _, previous| {
        assert_eq!(previous.len(), 1);
        Ok(scan.clone())
    })
    .await
    .unwrap();
    assert_eq!(expired.state, Funded);
    scan.spend = Some(vtorrent_btc::sync::SwapAnchor {
        txid: [7; 32],
        block_hash: [91; 32],
        height: 20,
    });
    let shallow = reconcile_with_scan(&state, &id, async |_, _, _| Ok(scan.clone()))
        .await
        .unwrap();
    assert_eq!(shallow.state, RefundConfirming);
    scan.tip_height = 25;
    let settled = reconcile_with_scan(&state, &id, async |_, _, _| Ok(scan.clone()))
        .await
        .unwrap();
    assert_eq!(settled.state, Refunded);
    scan.spend = None;
    scan.invalidated_anchor = true;
    let reorg = reconcile_with_scan(&state, &id, async |_, _, previous| {
        assert_eq!(previous.len(), 2);
        Ok(scan.clone())
    })
    .await
    .unwrap();
    assert_eq!(reorg.state, Funded);
    assert_eq!(reorg.reorg_count, 1);
    scan.invalidated_anchor = false;
    scan.funding = None;
    let missing = reconcile_with_scan(&state, &id, async |_, _, _| Ok(scan))
        .await
        .unwrap();
    assert_eq!(missing.state, FundingNotObserved);
    assert_eq!(missing.reorg_count, 1);
    assert_eq!(state.swaps.read().await[&id].btc_funding_raw, raw);
    assert!(state
        .btc_wallet
        .read()
        .await
        .as_ref()
        .unwrap()
        .list_utxos()
        .is_empty());
}

#[tokio::test]
async fn btc_reconciliation_rejects_failed_or_changed_scans_and_preserves_previous_evidence() {
    use crate::btc_reconciliation::reconcile_with_scan;
    let (state, id, scan) = btc_reconciliation_fixture().await;
    let first = reconcile_with_scan(&state, &id, async |_, _, _| Ok(scan.clone()))
        .await
        .unwrap();
    let failed = reconcile_with_scan(&state, &id, async |_, _, _| {
        Err(RpcError::BadRequest("bad filter commitment".into()))
    })
    .await;
    assert!(failed.is_err());
    assert_eq!(
        state.swaps.read().await[&id].btc_observation.as_ref(),
        Some(&first)
    );
    let guard = state.btc_swap_scan_lock.lock().await;
    assert!(reconcile_with_scan(&state, &id, async |_, _, _| {
        panic!("busy scan must not start")
    })
    .await
    .is_err());
    drop(guard);
    let changed = reconcile_with_scan(&state, &id, async |_, _, _| {
        state
            .swaps
            .write()
            .await
            .get_mut(&id)
            .unwrap()
            .taker_btc_refund_address = Some(
            vtorrent_btc::keys::derive_address(&[3; 64], 0, bitcoin::Network::Regtest).unwrap(),
        );
        Ok(scan)
    })
    .await;
    assert!(changed.is_err());
    assert_eq!(
        state.swaps.read().await[&id].btc_observation.as_ref(),
        Some(&first)
    );
    state.swaps.write().await.get_mut(&id).unwrap().hash_lock = [0; 32];
    assert!(reconcile_with_scan(&state, &id, async |_, _, _| {
        panic!("invalid terms must not scan")
    })
    .await
    .is_err());
}

#[tokio::test]
async fn btc_reconciliation_journal_restores_snapshot_without_exposing_secrets() {
    use crate::btc_reconciliation::reconcile_with_scan;
    use tower::ServiceExt;
    let (mut state, id, scan) = btc_reconciliation_fixture().await;
    let directory = tempfile::tempdir().unwrap();
    state.swap_recovery_dir = Some(directory.path().to_path_buf());
    *state.wallet_wif.write().await = Some(vtr_identity(1).0);
    *state.wallet_unlock_expiry.write().await = Some(0);
    let observed = reconcile_with_scan(&state, &id, async |_, _, _| Ok(scan.clone()))
        .await
        .unwrap();
    assert!(reconcile_with_scan(&state, &id, async |_, _, _| {
        *state.wallet_wif.write().await = Some(vtr_identity(2).0);
        Ok(scan)
    })
    .await
    .is_err());
    assert_eq!(
        state.swaps.read().await[&id].btc_observation.as_ref(),
        Some(&observed)
    );
    let mut restored = AppState::new_with_shared(state.chain.clone(), state.mempool.clone());
    restored.swap_recovery_dir = state.swap_recovery_dir.clone();
    *restored.btc_network.write().await = bitcoin::Network::Regtest;
    crate::swap_recovery::restore_with_wif(&restored, &vtr_identity(1).0)
        .await
        .unwrap();
    assert_eq!(
        restored.swaps.read().await[&id].btc_observation.as_ref(),
        Some(&observed)
    );
    restored.rpc_api_key = Some("btc-observation-key".into());
    let app = crate::server::build_router(restored);
    let locked = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/swap/btc-reconcile")
                .header("Content-Type", "application/json")
                .header("X-API-Key", "btc-observation-key")
                .body(axum::body::Body::from(format!(r#"{{"order_id":"{id}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(locked.status(), axum::http::StatusCode::FORBIDDEN);
    let unauthorized = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/swap/btc-reconcile")
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(format!(r#"{{"order_id":"{id}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), axum::http::StatusCode::UNAUTHORIZED);
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v1/swap/{id}/status"))
                .header("X-API-Key", "btc-observation-key")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["btc"]["observed_at"], NOW);
    assert_eq!(body["btc_reconciled"], false);
    assert!(!text.contains(&hex::encode(
        state
            .order_book
            .read()
            .await
            .get_order(&id)
            .unwrap()
            .preimage
            .unwrap()
    )));
    for secret in ["preimage", "witness", "private", "raw", "wif"] {
        assert!(!text.contains(secret));
    }
}

#[tokio::test]
async fn funding_failure_reserves_input_and_retries_identical_transaction() {
    let (state, id, refund_address) = fixture().await;
    let attempts = Mutex::new(Vec::new());
    let result = fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address.clone(),
        },
        async |raw: &[u8]| {
            attempts.lock().unwrap().push(raw.to_vec());
            Err(RpcError::BadRequest("ambiguous broadcast failure".into()))
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(state.swaps.read().await[&id].status, SwapStatus::BtcFunding);
    let btc = state.btc_wallet.read().await;
    assert!(btc.as_ref().unwrap().list_utxos().is_empty());
    assert_eq!(btc.as_ref().unwrap().pending_swap_transactions().len(), 1);
    drop(btc);
    let response = fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address,
        },
        async |raw: &[u8]| {
            attempts.lock().unwrap().push(raw.to_vec());
            Ok(txid(raw))
        },
    )
    .await
    .unwrap();
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(response.btc_funding_txid, btc_txid_hex(&txid(&attempts[0])));
}

#[tokio::test]
async fn concurrent_funding_of_same_order_broadcasts_once() {
    let (state, id, refund_address) = fixture().await;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let first = BtcFundRequest {
        order_id: id.clone(),
        btc_refund_address: refund_address.clone(),
    };
    let second = BtcFundRequest {
        order_id: id,
        btc_refund_address: refund_address,
    };
    let (first, second) = tokio::join!(
        fund_btc_with_broadcast(&state, first, async |raw: &[u8]| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(txid(raw))
        }),
        fund_btc_with_broadcast(&state, second, async |raw: &[u8]| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(txid(raw))
        })
    );
    assert_eq!(
        first.unwrap().btc_funding_txid,
        second.unwrap().btc_funding_txid
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn btc_refund_is_independent_and_retry_preserves_raw_transaction() {
    let (state, id, refund_address) = fixture().await;
    fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address,
        },
        async |raw: &[u8]| Ok(txid(raw)),
    )
    .await
    .unwrap();
    let btc_expiry = state.swaps.read().await[&id].btc_expiry;
    *state.mock_time.write().await = Some(u64::from(btc_expiry) + 1);
    let result = refund_with_broadcast(
        &state,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Vtr),
        },
        async |_: &[u8]| panic!("VTR refund must not touch BTC"),
    )
    .await;
    assert!(result.is_err());
    assert!(state.swaps.read().await[&id].vtr_refund_txid.is_none());
    assert!(!state.is_wallet_unlocked().await);
    let attempts = Mutex::new(Vec::new());
    let result = refund_with_broadcast(
        &state,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Btc),
        },
        async |raw: &[u8]| {
            attempts.lock().unwrap().push(raw.to_vec());
            Err(RpcError::BadRequest("ambiguous refund broadcast".into()))
        },
    )
    .await;
    assert!(result.is_err());
    {
        let mut swaps = state.swaps.write().await;
        let swap = swaps.get_mut(&id).unwrap();
        assert!(swap.btc_refund_txid.is_some());
        assert!(swap.vtr_refund_txid.is_none());
        swap.vtr_claim_txid = Some([9; 32]);
        swap.refresh_status();
    }
    let result = refund_with_broadcast(
        &state,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Btc),
        },
        async |raw: &[u8]| {
            attempts.lock().unwrap().push(raw.to_vec());
            Ok(txid(raw))
        },
    )
    .await
    .unwrap();
    assert_eq!(result.status, "BtcRefundSubmitted");
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0], attempts[1]);
    let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(&attempts[0]).unwrap();
    assert_eq!(tx.lock_time.to_consensus_u32(), btc_expiry);
}

#[tokio::test]
async fn input_reservation_and_signed_funding_survive_wallet_reload() {
    let (state, id, refund_address) = fixture().await;
    let path = std::env::temp_dir().join(format!(
        "vtr-swap-reservation-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let input = {
        let mut btc = state.btc_wallet.write().await;
        let wallet = btc.as_mut().unwrap();
        wallet.set_utxo_path(path.clone()).unwrap();
        wallet.list_utxos()[0].clone()
    };
    let response = fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address,
        },
        async |raw: &[u8]| Ok(txid(raw)),
    )
    .await
    .unwrap();
    let reloaded = vtorrent_btc::wallet::BtcWallet::with_persistence(
        [2; 64],
        bitcoin::Network::Regtest,
        path.clone(),
    )
    .unwrap();
    assert!(reloaded.list_utxos().is_empty());
    let transactions = reloaded.pending_swap_transactions();
    assert_eq!(transactions.len(), 1);
    let raw = transactions.get(&response.btc_funding_txid).unwrap();
    assert_eq!(btc_txid_hex(&txid(raw)), response.btc_funding_txid);
    reloaded.add_utxo(input);
    assert!(
        reloaded.list_utxos().is_empty(),
        "rescan must not restore a reserved input"
    );
    let expiry = state.swaps.read().await[&id].btc_expiry;
    let restarted = AppState::new();
    *restarted.btc_wallet.write().await = Some(reloaded);
    *restarted.mock_time.write().await = Some(u64::from(expiry) + 1);
    let attempted_refund = Mutex::new(Vec::new());
    let result = refund_with_broadcast(
        &restarted,
        SwapRefundRequest {
            order_id: id.clone(),
            leg: Some(SwapLeg::Btc),
        },
        async |raw: &[u8]| {
            *attempted_refund.lock().unwrap() = raw.to_vec();
            Err(RpcError::BadRequest("refund acknowledgement lost".into()))
        },
    )
    .await;
    assert!(result.is_err());
    assert!(!attempted_refund.lock().unwrap().is_empty());
    let restarted_again = AppState::new();
    *restarted_again.btc_wallet.write().await = Some(
        vtorrent_btc::wallet::BtcWallet::with_persistence(
            [2; 64],
            bitcoin::Network::Regtest,
            path.clone(),
        )
        .unwrap(),
    );
    *restarted_again.mock_time.write().await = Some(u64::from(expiry) + 1);
    let response = refund_with_broadcast(
        &restarted_again,
        SwapRefundRequest {
            order_id: id,
            leg: Some(SwapLeg::Btc),
        },
        async |raw: &[u8]| {
            assert_eq!(raw, attempted_refund.lock().unwrap().as_slice());
            Ok(txid(raw))
        },
    )
    .await
    .unwrap();
    assert_eq!(response.status, "BtcRefundSubmitted");
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn competing_orders_cannot_reuse_reserved_btc_input() {
    let (state, id, refund_address) = fixture().await;
    let mut other = state
        .order_book
        .read()
        .await
        .get_order(&id)
        .unwrap()
        .clone();
    other.order_id[0] ^= 1;
    let other_id = hex::encode(other.order_id);
    let mut other_swap = SwapState::new(other.order_id, other.hash_lock.unwrap());
    other_swap.vtr_funding_txid = other.funding_txid;
    other_swap.status = SwapStatus::VtrFunded;
    state.order_book.write().await.add_order(other);
    state
        .swaps
        .write()
        .await
        .insert(other_id.clone(), other_swap);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let (first, second) = tokio::join!(
        fund_btc_with_broadcast(
            &state,
            BtcFundRequest {
                order_id: id,
                btc_refund_address: refund_address.clone()
            },
            async |raw: &[u8]| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(txid(raw))
            }
        ),
        fund_btc_with_broadcast(
            &state,
            BtcFundRequest {
                order_id: other_id,
                btc_refund_address: refund_address
            },
            async |raw: &[u8]| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(txid(raw))
            }
        )
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn persistence_failure_never_broadcasts_or_consumes_input() {
    let (state, id, refund_address) = fixture().await;
    let missing = std::env::temp_dir()
        .join(format!(
            "vtr-missing-swap-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("utxos.json");
    assert!(state
        .btc_wallet
        .write()
        .await
        .as_mut()
        .unwrap()
        .set_utxo_path(missing)
        .is_err());
    let result = fund_btc_with_broadcast(
        &state,
        BtcFundRequest {
            order_id: id.clone(),
            btc_refund_address: refund_address,
        },
        async |_: &[u8]| panic!("must persist before broadcast"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        state
            .btc_wallet
            .read()
            .await
            .as_ref()
            .unwrap()
            .list_utxos()
            .len(),
        1
    );
    assert!(state
        .btc_wallet
        .read()
        .await
        .as_ref()
        .unwrap()
        .pending_swap_transactions()
        .is_empty());
    assert_eq!(state.swaps.read().await[&id].status, SwapStatus::VtrFunded);
}
