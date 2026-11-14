use super::*;

const WIF: &str = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
const ADDRESS: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
const PASS: &str = "testpass";
const COIN: u64 = vtorrent_node::consensus::COIN;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_stake_funding_propagates_and_confirms_on_two_daemons() {
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let sender_dir = directory.path().join("sender");
    let receiver_dir = directory.path().join("receiver");
    let sender = Daemon::start(&sender_dir, "sender.log", None).await;
    sender
        .post(
            "/api/v1/faucet",
            json!({"address": ADDRESS, "amount_satoshis": 10 * COIN}),
        )
        .await;
    sender.persisted(1).await;
    sender
        .post(
            "/api/v1/wallet/import",
            json!({"wif": WIF, "passphrase": PASS, "overwrite": false}),
        )
        .await;
    sender
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;

    let receiver = Daemon::start(&receiver_dir, "receiver.log", Some(&sender.p2p)).await;
    let funding_tip = sender.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    receiver.wait_tip(&funding_tip).await;

    let keys = sender
        .post(
            "/api/v1/wallet/cold-stake",
            json!({"locktime": 0, "spending_key_passphrase": "test-pass"}),
        )
        .await;
    assert!(keys.get("address").is_none(), "no burn address in response");
    assert!(
        keys.get("spending_wif").is_none(),
        "the spending key must not cross the RPC boundary"
    );
    assert!(std::path::Path::new(keys["spending_key_file"].as_str().unwrap()).exists());
    let second = sender
        .post(
            "/api/v1/wallet/cold-stake",
            json!({"locktime": 0, "spending_key_passphrase": "test-pass"}),
        )
        .await;
    let first_path = keys["spending_key_file"].as_str().unwrap();
    let second_path = second["spending_key_file"].as_str().unwrap();
    assert_ne!(first_path, second_path);
    for path in [first_path, second_path] {
        vtorrent_wallet::cold_stake_export::load_spending_key(
            std::path::Path::new(path),
            "test-pass",
        )
        .unwrap();
    }
    let script_hex = keys["script_pubkey"].as_str().unwrap();
    let script = hex::decode(script_hex).unwrap();
    let funded = sender
        .post(
            "/api/v1/wallet/fund-cold-stake",
            json!({"script_pubkey": script_hex, "amount_satoshis": COIN, "passphrase": PASS}),
        )
        .await;
    let txid = funded["txid"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mempool = receiver.get("/api/v1/mempool").await;
            if mempool["txids"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id.as_str() == Some(&txid))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "funding did not propagate over P2P; sender: {}; receiver: {}",
            sender.logs(),
            receiver.logs()
        )
    });

    receiver
        .post(
            "/api/v1/blockchain/mine",
            json!({"address": ADDRESS, "amount_satoshis": COIN}),
        )
        .await;
    let confirmation_tip = receiver.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    sender.wait_tip(&confirmation_tip).await;
    sender.persisted(2).await;
    receiver.persisted(2).await;

    // Spend the confirmed P2CS output offline, using only the encrypted
    // spending-key export, then submit the signed transaction to the node.
    let (spending_wif, _) = vtorrent_wallet::cold_stake_export::load_spending_key(
        std::path::Path::new(keys["spending_key_file"].as_str().unwrap()),
        "test-pass",
    )
    .unwrap();
    let funding_txid: [u8; 32] = hex::decode(&txid).unwrap().try_into().unwrap();
    let spend = vtorrent_wallet::tx_builder::build_p2cs_spend(
        &vtorrent_wallet::tx_builder::P2csSpendInput {
            txid: funding_txid,
            vout: 0,
            value_satoshis: COIN,
            script_pubkey: script.clone(),
        },
        spending_wif.as_str(),
        ADDRESS,
        COIN / 2,
        10_000,
        Some(ADDRESS),
        0,
    )
    .unwrap();
    let spend_txid = spend.txid();
    receiver
        .post(
            "/api/v1/blockchain/broadcast",
            json!({"raw_tx": hex::encode(bincode::serialize(&spend).unwrap())}),
        )
        .await;
    receiver
        .post(
            "/api/v1/blockchain/mine",
            json!({"address": ADDRESS, "amount_satoshis": COIN}),
        )
        .await;
    let spend_tip = receiver.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    sender.wait_tip(&spend_tip).await;
    sender.persisted(3).await;
    receiver.persisted(3).await;
    sender.stop(false).await;
    receiver.stop(false).await;

    for path in [&sender_dir, &receiver_dir] {
        let store = BlockStore::open(path.join("chain.db")).unwrap();
        let chain = store.load_into_regtest_chain().unwrap();
        assert_eq!(hex::encode(chain.best_hash().unwrap()), spend_tip);
        assert!(
            chain.get_transaction_owned(&spend_txid).is_some(),
            "the cold-key spend must be confirmed on both nodes"
        );
        assert!(
            chain.get_utxo(&funding_txid, 0).is_none(),
            "the P2CS output is spent after the cold-key withdrawal"
        );
        let withdrawn = chain
            .get_utxo(&spend_txid, 0)
            .expect("recipient output from the cold-key spend");
        assert_eq!(withdrawn.value, COIN / 2);
        assert_eq!(withdrawn.script_pubkey.len(), 25);
    }
}
