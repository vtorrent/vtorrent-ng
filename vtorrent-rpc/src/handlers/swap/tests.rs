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
