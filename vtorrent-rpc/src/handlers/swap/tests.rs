use super::*;
use bitcoin::hashes::Hash;
use std::sync::Mutex;
use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, SwapOrder};

const NOW: u64 = 1_800_000_000;

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
