use super::*;
use bitcoin::Network;
use std::process::Command;

const CONTAINER: &str = "vtr-btc-regtest";
const BTC_PEER: &str = "127.0.0.1:18448";
const WALLET: &str = "multinode";

fn bitcoin_cli(args: &[&str]) -> String {
    let output = Command::new("docker")
        .args([
            "exec",
            CONTAINER,
            "bitcoin-cli",
            "-regtest",
            "-rpcuser=vtr",
            "-rpcpassword=vtrtest",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("docker exec {CONTAINER} failed: {error}"));
    assert!(
        output.status.success(),
        "bitcoin-cli {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn ensure_wallet() {
    let wallets = bitcoin_cli(&["listwallets"]);
    if !wallets.contains(&format!("\"{WALLET}\"")) {
        let loaded = Command::new("docker")
            .args([
                "exec",
                CONTAINER,
                "bitcoin-cli",
                "-regtest",
                "-rpcuser=vtr",
                "-rpcpassword=vtrtest",
                "loadwallet",
                WALLET,
            ])
            .output()
            .unwrap();
        if !loaded.status.success() {
            bitcoin_cli(&["createwallet", WALLET]);
        }
    }
    let balance: f64 = bitcoin_cli(&["-rpcwallet=multinode", "getbalance"])
        .parse()
        .unwrap();
    if balance < 3.0 {
        let address = bitcoin_cli(&["-rpcwallet=multinode", "getnewaddress"]);
        bitcoin_cli(&["-rpcwallet=multinode", "generatetoaddress", "101", &address]);
    }
}

fn mine(count: u32) -> Vec<String> {
    let address = bitcoin_cli(&["-rpcwallet=multinode", "getnewaddress"]);
    serde_json::from_str(&bitcoin_cli(&[
        "-rpcwallet=multinode",
        "generatetoaddress",
        &count.to_string(),
        &address,
    ]))
    .unwrap()
}

fn height() -> u64 {
    bitcoin_cli(&["getblockcount"]).parse().unwrap()
}

fn wait_filters() {
    let tip = bitcoin_cli(&["getbestblockhash"]);
    for _ in 0..100 {
        let output = Command::new("docker")
            .args([
                "exec",
                CONTAINER,
                "bitcoin-cli",
                "-regtest",
                "-rpcuser=vtr",
                "-rpcpassword=vtrtest",
                "getblockfilter",
                &tip,
            ])
            .output()
            .unwrap();
        if output.status.success() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("BTC compact-filter index did not catch up to {tip}");
}

async fn wait_btc(daemon: &mut Daemon, tip: u64, balance: u64) {
    let mut reported = false;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "daemon exited: {}",
                daemon.logs()
            );
            let status = daemon.get("/api/v1/btc/status").await;
            if status["synced"] == true
                && status["best_height"] == tip
                && status["balance_satoshis"] == balance
            {
                return;
            }
            if !reported && status["synced"] == true {
                eprintln!(
                    "wait_btc: want tip={tip} balance={balance}; got tip={} balance={} address={}",
                    status["best_height"], status["balance_satoshis"], status["address"]
                );
                reported = true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "BTC SPV did not reach tip {tip} with balance {balance}: {}",
            daemon.logs()
        )
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode test; requires the vtr-btc-regtest container"]
async fn independent_daemons_agree_on_btc_spv_tip_and_reorg() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let _scenario = RECOVERY_SCENARIO.lock().await;
    ensure_wallet();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let seeds = [[0x55u8; 64], [0x66u8; 64]].map(|mut seed| {
        seed[..nanos.len()].copy_from_slice(&nanos);
        seed
    });
    let addresses: Vec<_> = seeds
        .iter()
        .map(|seed| vtorrent_btc::keys::derive_address(seed, 0, Network::Regtest).unwrap())
        .collect();
    for address in &addresses {
        let txid = bitcoin_cli(&["-rpcwallet=multinode", "sendtoaddress", address, "1.0"]);
        eprintln!("funded {address} with 1.0 BTC in {txid}");
    }
    let funding_block = mine(1).remove(0);
    wait_filters();
    let funded_tip = height();
    let directory = tempfile::tempdir().unwrap();
    let mut daemons = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        let seed = hex::encode(seed);
        let mut daemon = Daemon::start_with_btc(
            &directory.path().join(format!("node-{index}")),
            "daemon.log",
            None,
            None,
            &seed,
            BTC_PEER,
        )
        .await;
        wait_btc(&mut daemon, funded_tip, 100_000_000).await;
        let status = daemon.get("/api/v1/btc/status").await;
        assert_eq!(status["address"], addresses[index]);
        daemons.push(daemon);
    }
    for daemon in daemons {
        daemon.stop(false).await;
    }

    let mut daemons = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        let seed = hex::encode(seed);
        let mut daemon = Daemon::start_with_btc(
            &directory.path().join(format!("node-{index}")),
            "restarted.log",
            None,
            None,
            &seed,
            BTC_PEER,
        )
        .await;
        wait_btc(&mut daemon, funded_tip, 100_000_000).await;
        daemons.push(daemon);
    }

    bitcoin_cli(&["invalidateblock", &funding_block]);
    mine(2);
    let reorged_tip = height();
    assert!(reorged_tip > funded_tip);
    for daemon in daemons {
        daemon.stop(false).await;
    }
    let mut daemons = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        let seed = hex::encode(seed);
        let mut daemon = Daemon::start_with_btc(
            &directory.path().join(format!("node-{index}")),
            "reorged.log",
            None,
            None,
            &seed,
            BTC_PEER,
        )
        .await;
        wait_btc(&mut daemon, reorged_tip, 100_000_000).await;
        let status = daemon.get("/api/v1/btc/status").await;
        assert_eq!(status["best_height"], reorged_tip);
        assert_eq!(status["balance_satoshis"], 100_000_000);
        daemons.push(daemon);
    }
    for daemon in daemons {
        daemon.stop(false).await;
    }
    eprintln!(
        "Two independent daemons agreed on BTC tip {funded_tip}, then reorged tip {reorged_tip}, with the funding output re-included"
    );
}

const MAKER_WIF: &str = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
const MAKER_ADDR: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
const TAKER_WIF: &str = "WHorCxM7fSQDwTd6W19pdTjSH2nAHrgwEULonwbkDkDw7TdSxTVU";
const TAKER_ADDR: &str = "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh";

async fn post(daemon: &Daemon, path: &str, body: Value) -> Value {
    let (status, response) = rpc_polling::post_json_once(
        &daemon.client,
        &format!("{}{path}", daemon.rpc),
        &body,
        MUTATION_TIMEOUT,
    )
    .await
    .unwrap_or_else(|error| panic!("POST {path} failed: {error}; {}", daemon.logs()));
    assert!(
        status.is_success(),
        "POST {path}: {status} {response}; {}",
        daemon.logs()
    );
    response
}

async fn reconcile_btc(daemon: &Daemon, order_id: &str) -> Value {
    for attempt in 0..3 {
        let (status, response) = rpc_polling::post_json_once(
            &daemon.client,
            &format!("{}/api/v1/swap/btc-reconcile", daemon.rpc),
            &json!({"order_id": order_id}),
            MUTATION_TIMEOUT,
        )
        .await
        .unwrap();
        if status.is_success() {
            return response;
        }
        if attempt == 2 {
            panic!(
                "btc-reconcile failed: {status} {response}; {}",
                daemon.logs()
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    unreachable!()
}

async fn mine_vtr(daemon: &Daemon) {
    post(
        daemon,
        "/api/v1/blockchain/mine",
        json!({"address": MAKER_ADDR, "amount_satoshis": 100_000_000u64}),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode test; requires the vtr-btc-regtest container"]
async fn multi_node_htlc_swap_settles_across_real_btc_spv() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let _scenario = RECOVERY_SCENARIO.lock().await;
    ensure_wallet();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let mut seed = [0x77u8; 64];
    seed[..nanos.len()].copy_from_slice(&nanos);
    let btc_address = vtorrent_btc::keys::derive_address(&seed, 0, Network::Regtest).unwrap();
    bitcoin_cli(&["-rpcwallet=multinode", "sendtoaddress", &btc_address, "1.0"]);
    mine(1);

    let directory = tempfile::tempdir().unwrap();
    let mut maker = Daemon::start_with_btc(
        &directory.path().join("maker"),
        "daemon.log",
        None,
        None,
        &hex::encode(seed),
        BTC_PEER,
    )
    .await;
    wait_btc(&mut maker, height(), 100_000_000).await;

    post(
        &maker,
        "/api/v1/faucet",
        json!({"address": MAKER_ADDR, "amount_satoshis": 20_000_000_000_u64}),
    )
    .await;
    post(
        &maker,
        "/api/v1/wallet/import",
        json!({"wif": MAKER_WIF, "passphrase": "testpass"}),
    )
    .await;
    post(
        &maker,
        "/api/v1/wallet/unlock",
        json!({"passphrase": "testpass", "timeout_secs": 0}),
    )
    .await;

    let order = post(
        &maker,
        "/api/v1/dex/order",
        json!({
            "maker_address": MAKER_ADDR,
            "maker_btc_address": btc_address,
            "offer_amount_satoshis": 10_000_000_000_u64,
            "offer_asset": "VTR",
            "request_amount_satoshis": 100_000u64,
            "request_asset": "BTC",
            "expiry_secs": 86_400u64,
            "passphrase": "testpass",
        }),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    let matched = post(
        &maker,
        "/api/v1/dex/match",
        json!({
            "order_id": order_id,
            "taker_address": TAKER_ADDR,
            "passphrase": "testpass",
        }),
    )
    .await;
    let vtr_funding = matched["funding_txid"].as_str().unwrap().to_owned();

    for _ in 0..6 {
        mine_vtr(&maker).await;
    }
    let status = maker.get(&format!("/api/v1/swap/{order_id}/status")).await;
    assert_eq!(status["vtr"]["state"], "funded");
    assert_eq!(status["vtr"]["funding"]["txid"], vtr_funding);

    let refund_address = btc_address.clone();
    let funded = post(
        &maker,
        "/api/v1/swap/btc-fund",
        json!({"order_id": order_id, "btc_refund_address": refund_address}),
    )
    .await;
    let btc_funding = funded["btc_funding_txid"].as_str().unwrap().to_owned();
    mine(6);
    wait_filters();

    let claimed = post(
        &maker,
        "/api/v1/swap/btc-claim",
        json!({"order_id": order_id}),
    )
    .await;
    let btc_claim = claimed["txid"].as_str().unwrap().to_owned();
    mine(6);
    wait_filters();
    let btc_status = reconcile_btc(&maker, &order_id).await;
    assert_eq!(btc_status["state"], "claimed");
    assert_eq!(btc_status["funding"]["txid"], btc_funding);
    assert_eq!(btc_status["spend"]["txid"], btc_claim);

    let preimage = maker
        .get(&format!("/api/v1/debug/order/{order_id}/preimage"))
        .await["preimage"]
        .as_str()
        .unwrap()
        .to_owned();
    let vtr_claimed = post(
        &maker,
        "/api/v1/swap/vtr-claim",
        json!({
            "order_id": order_id,
            "preimage": preimage,
            "taker_wif": TAKER_WIF,
        }),
    )
    .await;
    let vtr_claim = vtr_claimed["txid"].as_str().unwrap().to_owned();
    for _ in 0..6 {
        mine_vtr(&maker).await;
    }
    let status = maker.get(&format!("/api/v1/swap/{order_id}/status")).await;
    assert_eq!(status["vtr"]["state"], "claimed");
    assert_eq!(status["vtr"]["spend"]["txid"], vtr_claim);
    assert_eq!(status["vtr"]["funding"]["txid"], vtr_funding);

    maker.stop(false).await;
    eprintln!(
        "Real BTC SPV HTLC swap settled: VTR funding {vtr_funding}, BTC funding {btc_funding}, BTC claim {btc_claim}, VTR claim {vtr_claim}"
    );
}

async fn wait_height(daemon: &Daemon, target: u64) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let height = daemon.get("/api/v1/blockchain/height").await["height"]
                .as_u64()
                .unwrap();
            if height >= target {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("daemon did not reach height {target}: {}", daemon.logs()));
}

fn hash32(value: &str) -> [u8; 32] {
    hex::decode(value)
        .unwrap()
        .try_into()
        .unwrap_or_else(|_| panic!("expected 32-byte hex, got {value}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual release-mode test; requires the vtr-btc-regtest container"]
async fn taker_claims_vtr_on_a_separate_node() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let _scenario = RECOVERY_SCENARIO.lock().await;
    ensure_wallet();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let mut seed = [0xaa; 64];
    seed[..nanos.len()].copy_from_slice(&nanos);
    let btc_address = vtorrent_btc::keys::derive_address(&seed, 0, Network::Regtest).unwrap();
    bitcoin_cli(&["-rpcwallet=multinode", "sendtoaddress", &btc_address, "1.0"]);
    mine(1);

    let directory = tempfile::tempdir().unwrap();
    let mut maker = Daemon::start_with_btc(
        &directory.path().join("maker"),
        "maker.log",
        None,
        None,
        &hex::encode(seed),
        BTC_PEER,
    )
    .await;
    wait_btc(&mut maker, height(), 100_000_000).await;

    post(
        &maker,
        "/api/v1/faucet",
        json!({"address": MAKER_ADDR, "amount_satoshis": 20_000_000_000_u64}),
    )
    .await;
    post(
        &maker,
        "/api/v1/wallet/import",
        json!({"wif": MAKER_WIF, "passphrase": "testpass"}),
    )
    .await;
    post(
        &maker,
        "/api/v1/wallet/unlock",
        json!({"passphrase": "testpass", "timeout_secs": 0}),
    )
    .await;

    let order = post(
        &maker,
        "/api/v1/dex/order",
        json!({
            "maker_address": MAKER_ADDR,
            "maker_btc_address": btc_address,
            "offer_amount_satoshis": 10_000_000_000u64,
            "offer_asset": "VTR",
            "request_amount_satoshis": 100_000u64,
            "request_asset": "BTC",
            "expiry_secs": 86_400u64,
            "passphrase": "testpass",
        }),
    )
    .await;
    let order_id = order["order_id"].as_str().unwrap().to_owned();
    let matched = post(
        &maker,
        "/api/v1/dex/match",
        json!({
            "order_id": order_id,
            "taker_address": TAKER_ADDR,
            "passphrase": "testpass",
        }),
    )
    .await;
    for _ in 0..6 {
        mine_vtr(&maker).await;
    }

    let target = maker.get("/api/v1/blockchain/height").await["height"]
        .as_u64()
        .unwrap();
    let taker = Daemon::start(
        &directory.path().join("taker"),
        "taker.log",
        Some(&maker.p2p),
    )
    .await;
    wait_height(&taker, target).await;

    let preimage = maker
        .get(&format!("/api/v1/debug/order/{order_id}/preimage"))
        .await["preimage"]
        .as_str()
        .unwrap()
        .to_owned();
    let claim =
        vtorrent_wallet_service::build_vtr_htlc_claim(vtorrent_wallet_service::VtrClaimParams {
            hash_lock: hash32(matched["hash_lock"].as_str().unwrap()),
            taker_address: TAKER_ADDR,
            maker_address: MAKER_ADDR,
            expiry: matched["expiry"].as_u64().unwrap() as u32,
            vtr_amount: matched["vtr_amount"].as_u64().unwrap(),
            funding_txid: hash32(matched["funding_txid"].as_str().unwrap()),
            preimage: hash32(&preimage),
            taker_wif: TAKER_WIF,
        })
        .unwrap();
    let txid = hex::encode(claim.txid());
    let accepted = post(
        &taker,
        "/api/v1/blockchain/broadcast",
        json!({"raw_tx": hex::encode(bincode::serialize(&claim).unwrap())}),
    )
    .await;
    assert_eq!(accepted["txid"], txid);

    tokio::time::sleep(Duration::from_secs(1)).await;
    mine_vtr(&maker).await;
    wait_height(&taker, target + 1).await;
    let lookup = taker.get(&format!("/api/v1/blockchain/tx/{txid}")).await;
    assert!(
        lookup["block_height"].is_number(),
        "taker did not confirm the claim"
    );
    let status = maker.get(&format!("/api/v1/swap/{order_id}/status")).await;
    assert_eq!(status["vtr"]["state"], "claim_confirming");
    for _ in 0..5 {
        mine_vtr(&maker).await;
    }
    wait_height(&taker, target + 6).await;
    let status = maker.get(&format!("/api/v1/swap/{order_id}/status")).await;
    assert_eq!(status["vtr"]["state"], "claimed");
    assert_eq!(status["vtr"]["spend"]["txid"], txid);

    taker.stop(false).await;
    maker.stop(false).await;
    eprintln!("Separate-node taker claimed VTR: order {order_id}, claim {txid}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual release-mode test; requires the vtr-btc-regtest container"]
async fn btc_spv_drops_a_reorged_out_funding_output() {
    assert!(!cfg!(debug_assertions), "run with --release");
    let _scenario = RECOVERY_SCENARIO.lock().await;
    ensure_wallet();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let mut seed = [0x88u8; 64];
    seed[..nanos.len()].copy_from_slice(&nanos);
    let address = vtorrent_btc::keys::derive_address(&seed, 0, Network::Regtest).unwrap();
    let mut key_seed = [0x99u8; 64];
    key_seed[..nanos.len()].copy_from_slice(&nanos);
    let key_address = vtorrent_btc::keys::derive_address(&key_seed, 0, Network::Regtest).unwrap();
    let key_wif = vtorrent_btc::keys::derive_wif(&key_seed, 0, Network::Regtest).unwrap();

    let source_txid = bitcoin_cli(&["-rpcwallet=multinode", "sendtoaddress", &key_address, "1.0"]);
    mine(1);
    let source: Value =
        serde_json::from_str(&bitcoin_cli(&["getrawtransaction", &source_txid, "true"])).unwrap();
    let (source_vout, source_script, source_value) = source["vout"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .find(|(_, output)| output["scriptPubKey"]["address"] == key_address)
        .map(|(vout, output)| {
            (
                vout as u64,
                output["scriptPubKey"]["hex"].as_str().unwrap().to_owned(),
                output["value"].as_f64().unwrap(),
            )
        })
        .expect("funding output for the test key");
    let prevtxs = json!([{
        "txid": source_txid,
        "vout": source_vout,
        "scriptPubKey": source_script,
        "amount": source_value,
    }]);

    let funding = bitcoin_cli(&[
        "createrawtransaction",
        &json!([{"txid": source_txid, "vout": source_vout}]).to_string(),
        &json!({address: source_value - 0.00002}).to_string(),
    ]);
    let signed: Value = serde_json::from_str(&bitcoin_cli(&[
        "signrawtransactionwithkey",
        &funding,
        &json!([key_wif]).to_string(),
        &prevtxs.to_string(),
    ]))
    .unwrap();
    assert_eq!(signed["complete"], true);
    let funding_txid = bitcoin_cli(&["sendrawtransaction", signed["hex"].as_str().unwrap()]);
    let funding_block = mine(1).remove(0);
    let funded_tip = height();

    let directory = tempfile::tempdir().unwrap();
    let node_dir = directory.path().join("node");
    let mut daemon = Daemon::start_with_btc(
        &node_dir,
        "funded.log",
        None,
        None,
        &hex::encode(seed),
        BTC_PEER,
    )
    .await;
    wait_btc(&mut daemon, funded_tip, 99_998_000).await;
    daemon.stop(false).await;

    let burn = bitcoin_cli(&["-rpcwallet=multinode", "getnewaddress"]);
    let conflict = bitcoin_cli(&[
        "createrawtransaction",
        &json!([{"txid": source_txid, "vout": source_vout}]).to_string(),
        &json!({burn: source_value - 0.00003}).to_string(),
    ]);
    let signed: Value = serde_json::from_str(&bitcoin_cli(&[
        "signrawtransactionwithkey",
        &conflict,
        &json!([key_wif]).to_string(),
        &prevtxs.to_string(),
    ]))
    .unwrap();
    assert_eq!(signed["complete"], true);
    bitcoin_cli(&["invalidateblock", &funding_block]);
    let conflict_txid = bitcoin_cli(&["sendrawtransaction", signed["hex"].as_str().unwrap()]);
    mine(1);
    wait_filters();
    let original: Value =
        serde_json::from_str(&bitcoin_cli(&["getrawtransaction", &funding_txid, "true"]))
            .unwrap_or(Value::Null);
    assert_eq!(original["confirmations"].as_u64().unwrap_or(0), 0);
    assert_ne!(conflict_txid, funding_txid);

    let mut daemon = Daemon::start_with_btc(
        &node_dir,
        "reorged.log",
        None,
        None,
        &hex::encode(seed),
        BTC_PEER,
    )
    .await;
    wait_btc(&mut daemon, height(), 0).await;
    daemon.stop(false).await;
    eprintln!(
        "Reorged-out BTC funding {funding_txid} was replaced by {conflict_txid}; the restarted daemon reports a zero balance"
    );
}
