#![cfg(unix)]

use std::process::Command;

fn run(source: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap()
}

#[test]
fn stages_exact_bytes_and_never_overwrites_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    let original = b"{\"utxos\":[],\"next_index\":7}\n";
    std::fs::write(&source, original).unwrap();
    let result = run(&source, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(std::fs::read(&output).unwrap(), original);
    let retry = run(&source, &output);
    assert!(!retry.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), original);
}

#[test]
fn rejects_wallet_paths_outside_the_data_directory() {
    let data_dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let source = data_dir.path().join("btc_utxos-regtest.json");
    let output = other.path().join("btc_utxos-regtest-0123456789abcdef.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[test]
fn malformed_input_does_not_create_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    std::fs::write(&source, b"{ invalid").unwrap();
    assert!(!run(&source, &output).status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(source).unwrap(), b"{ invalid");
}

#[test]
fn interrupted_stage_blocks_retry_without_modifying_any_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    let stage = output.with_extension("json.recovery-stage");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    std::fs::write(&stage, b"partial interrupted write").unwrap();
    assert!(!run(&source, &output).status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), b"{\"utxos\":[]}");
    assert_eq!(std::fs::read(stage).unwrap(), b"partial interrupted write");
}

#[test]
fn rejects_wrong_network_and_symbolic_link_sources() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let real = dir.path().join("actual.json");
    let output = dir.path().join("btc_utxos-mainnet-0123456789abcdef.json");
    std::fs::write(&real, b"{\"utxos\":[]}").unwrap();
    symlink(&real, &source).unwrap();
    assert!(!run(&source, &output).status.success());
    let correct_network = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    assert!(!run(&source, &correct_network).status.success());
    assert!(!output.exists());
    assert!(!correct_network.exists());
}

#[test]
fn requires_explicit_confirmations() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[tokio::test]
async fn daemon_starts_after_archive_with_the_seed_scoped_file() {
    use sha2::{Digest, Sha256};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let mut hasher = Sha256::new();
    hasher.update(b"vtorrent-btc-wallet-id-v1");
    hasher.update([1u8; 64]);
    let id = hex::encode(&hasher.finalize()[..8]);
    let output = dir.path().join(format!("btc_utxos-regtest-{id}.json"));
    let archive = dir.path().join("legacy-btc-state.json");
    std::fs::write(&source, b"{\"utxos\":[],\"next_index\":7}").unwrap();
    assert!(run(&source, &output).status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .arg("--confirm-isolated")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let rpc_port = TcpListener::bind("127.0.0.1:0").unwrap();
    let p2p_port = TcpListener::bind("127.0.0.1:0").unwrap();
    let rpc = rpc_port.local_addr().unwrap().to_string();
    let p2p = p2p_port.local_addr().unwrap().to_string();
    drop(rpc_port);
    drop(p2p_port);
    let mut child = Command::new(env!("CARGO_BIN_EXE_vtorrent-daemon"))
        .args([
            "--regtest",
            "--testnet",
            "--isolated",
            "--no-dht",
            "--btc-regtest",
            "--rpc-addr",
            &rpc,
            "--listen",
            &p2p,
        ])
        .arg("--data-dir")
        .arg(dir.path())
        .env("VTORRENT_BTC_SEED", "01".repeat(64))
        .env_remove("VTORRENT_RPC_API_KEY")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let ready = loop {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("daemon exited with {status} after archive");
        }
        if TcpStream::connect(&rpc).is_ok() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let address = if ready {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let body: serde_json::Value = client
            .get(format!("http://{rpc}/api/v1/btc/address"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        Some(body["address"].as_str().unwrap().to_string())
    } else {
        None
    };
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(ready, "daemon did not open RPC after archive");
    let expected =
        vtorrent_btc::keys::derive_address(&[1u8; 64], 7, bitcoin::Network::Regtest).unwrap();
    assert_eq!(
        address.as_deref(),
        Some(expected.as_str()),
        "the daemon did not load the staged wallet index"
    );
}

#[test]
fn refuses_to_archive_without_a_matching_staged_copy() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .arg("--confirm-isolated")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(source.exists());
    assert!(!archive.exists());
    std::fs::write(&output, b"{\"utxos\":[{\"txid\":\"other\"}]}").unwrap();
    let mismatch = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .arg("--confirm-isolated")
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(source.exists());
    assert!(!archive.exists());
}

#[test]
fn archives_exact_original_and_removes_only_the_active_legacy_name() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    std::fs::write(&source, b"{\"utxos\":[],\"swap_contracts\":{}}").unwrap();
    assert!(run(&source, &output).status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .arg("--confirm-isolated")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!source.exists());
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        b"{\"utxos\":[],\"swap_contracts\":{}}"
    );
    let retry = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .arg("--confirm-isolated")
        .output()
        .unwrap();
    assert!(!retry.status.success());
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        b"{\"utxos\":[],\"swap_contracts\":{}}"
    );
}
