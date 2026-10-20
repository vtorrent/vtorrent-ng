use super::*;
use std::os::unix::fs::PermissionsExt;

const MAKER_WIF: &str = "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS";
const MAKER_ADDR: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";
const TAKER_ADDR: &str = "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh";
const PASS: &str = "testpass";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_wallet_survives_restart_and_requires_the_passphrase() {
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let daemon = Daemon::start_with_activation(directory.path(), "first.log", None, None).await;

    daemon
        .post(
            "/api/v1/faucet",
            json!({"address": MAKER_ADDR, "amount_satoshis": 20_000_000_000_u64}),
        )
        .await;
    let imported = daemon
        .post(
            "/api/v1/wallet/import",
            json!({"wif": MAKER_WIF, "passphrase": PASS, "overwrite": false}),
        )
        .await;
    assert_eq!(imported["address"], MAKER_ADDR);
    daemon
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;
    let sent = daemon
        .post(
            "/api/v1/wallet/send",
            json!({
                "to_address": TAKER_ADDR,
                "amount_satoshis": 100_000_000_u64,
                "passphrase": PASS,
            }),
        )
        .await;
    let txid = sent["txid"].as_str().unwrap().to_owned();
    let mempool = daemon.get("/api/v1/mempool").await;
    assert!(mempool["txids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == &txid));
    daemon.stop(false).await;

    let wallet_path = directory.path().join("wallet.json");
    let mode = std::fs::metadata(&wallet_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "wallet.json must be 0600");

    let daemon = Daemon::start_with_activation(directory.path(), "second.log", None, None).await;
    let addresses = daemon.get("/api/v1/wallet/addresses").await;
    assert!(
        addresses["addresses"].as_array().unwrap().is_empty(),
        "wallet must start locked"
    );
    let (status, _) = daemon
        .try_post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": "wrong", "timeout_secs": 0}),
        )
        .await;
    assert!(!status.is_success(), "wrong passphrase must be rejected");

    daemon
        .post(
            "/api/v1/wallet/unlock",
            json!({"passphrase": PASS, "timeout_secs": 0}),
        )
        .await;
    let addresses = daemon.get("/api/v1/wallet/addresses").await;
    assert_eq!(addresses["addresses"][0]["address"], MAKER_ADDR);
    let balance = daemon.get("/api/v1/wallet/balance").await;
    assert!(balance["confirmed"].as_u64().unwrap() > 0);
    let mempool = daemon.get("/api/v1/mempool").await;
    assert!(
        mempool["txids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == &txid),
        "mempool must persist across restart"
    );
    daemon.stop(false).await;
}
