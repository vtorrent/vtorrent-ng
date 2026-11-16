#![cfg(unix)]

use std::process::Command;

fn write_seed(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("btc-seed.hex");
    std::fs::write(&path, format!("{}\n", "01".repeat(64))).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn run(source: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    let seed = write_seed(source.parent().unwrap());
    Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap()
}

#[test]
fn stages_exact_bytes_and_never_overwrites_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
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
    let output = other.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let seed = write_seed(data_dir.path());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            data_dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[test]
fn stages_the_unlabelled_wallet_when_the_network_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let seed = write_seed(dir.path());
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&output).unwrap(), b"{\"utxos\":[]}");
}

#[test]
fn rejects_an_output_id_not_derived_from_the_seed() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-0123456789abcdef.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    assert!(!run(&source, &output).status.success());
    assert!(!output.exists());
}

#[test]
fn malformed_input_does_not_create_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    std::fs::write(&source, b"{ invalid").unwrap();
    assert!(!run(&source, &output).status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(source).unwrap(), b"{ invalid");
}

#[test]
fn interrupted_stage_blocks_retry_without_modifying_any_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let stage = output.with_extension("json.recovery-stage");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    std::fs::write(&stage, b"partial interrupted write").unwrap();
    assert!(!run(&source, &output).status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&source).unwrap(), b"{\"utxos\":[]}");
    assert_eq!(std::fs::read(stage).unwrap(), b"partial interrupted write");
}

#[test]
fn interrupted_publication_leaves_output_and_stage_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let stage = output.with_extension("json.recovery-stage");
    let original = b"{\"utxos\":[]}";
    std::fs::write(&source, original).unwrap();
    std::fs::write(&output, original).unwrap();
    std::fs::write(&stage, original).unwrap();
    assert!(!run(&source, &output).status.success());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(std::fs::read(&output).unwrap(), original);
    assert_eq!(std::fs::read(&stage).unwrap(), original);
}

#[test]
fn rejects_wrong_network_and_symbolic_link_sources() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let real = dir.path().join("actual.json");
    let output = dir.path().join("btc_utxos-mainnet-62d04ad2793b8849.json");
    std::fs::write(&real, b"{\"utxos\":[]}").unwrap();
    symlink(&real, &source).unwrap();
    assert!(!run(&source, &output).status.success());
    let correct_network = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    assert!(!run(&source, &correct_network).status.success());
    assert!(!output.exists());
    assert!(!correct_network.exists());
}

#[test]
fn requires_explicit_confirmations() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    let seed = write_seed(dir.path());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            source.parent().unwrap().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

fn promote_command(
    dir: &std::path::Path,
    archive: &std::path::Path,
    output: &std::path::Path,
) -> std::process::Command {
    use sha2::{Digest, Sha256};
    let seed = write_seed(dir);
    let digest = hex::encode(Sha256::digest(std::fs::read(archive).unwrap()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"));
    command.args([
        "promote",
        "--data-dir",
        dir.to_str().unwrap(),
        "--archive",
        archive.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--seed-file",
        seed.to_str().unwrap(),
        "--network",
        "regtest",
        "--confirm-isolated",
        "--confirm-owner",
        "--confirm-recovery-preconditions",
        "--confirm-archive-sha256",
        &digest,
    ]);
    command
}

#[test]
fn promote_requires_the_recovery_preconditions_assertion() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state.json");
    std::fs::write(&output, b"{\"utxos\":[]}").unwrap();
    std::fs::write(&archive, b"{\"utxos\":[]}").unwrap();
    let seed = write_seed(dir.path());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "promote",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
            "--confirm-isolated",
            "--confirm-owner",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), b"{\"utxos\":[]}");
}

#[test]
fn promote_converts_a_verified_copy_and_preserves_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state.json");
    let original = b"{\"utxos\":[],\"next_index\":7}";
    std::fs::write(&output, original).unwrap();
    std::fs::write(&archive, original).unwrap();
    let result = promote_command(dir.path(), &archive, &output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&archive).unwrap(), original);
    let promoted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(promoted["journal_version"], 3);
    let wallet = vtorrent_btc::wallet::BtcWallet::with_persistence(
        [1u8; 64],
        bitcoin::Network::Regtest,
        output,
    )
    .unwrap();
    assert_eq!(wallet.current_address().unwrap(), {
        vtorrent_btc::keys::derive_address(&[1u8; 64], 7, bitcoin::Network::Regtest).unwrap()
    });
}

#[test]
fn promote_refuses_when_the_archive_differs() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state.json");
    let original = b"{\"utxos\":[],\"next_index\":7}";
    std::fs::write(&output, original).unwrap();
    std::fs::write(&archive, b"{\"utxos\":[],\"next_index\":8}").unwrap();
    let result = promote_command(dir.path(), &archive, &output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), original);
}

#[test]
fn promote_refuses_a_different_reviewed_digest() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state.json");
    let original = b"{\"utxos\":[],\"next_index\":7}";
    std::fs::write(&output, original).unwrap();
    std::fs::write(&archive, original).unwrap();
    let seed = write_seed(dir.path());
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "promote",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
            "--confirm-isolated",
            "--confirm-owner",
            "--confirm-recovery-preconditions",
            "--confirm-archive-sha256",
            &"00".repeat(32),
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&output).unwrap(), original);
}

#[test]
fn mainnet_promotion_is_disabled_pending_review() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("btc_utxos-mainnet-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state.json");
    let original = b"{\"utxos\":[],\"next_index\":7}";
    std::fs::write(&output, original).unwrap();
    std::fs::write(&archive, original).unwrap();
    let seed = write_seed(dir.path());
    let digest = hex::encode(Sha256::digest(original));
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "promote",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "mainnet",
            "--confirm-isolated",
            "--confirm-owner",
            "--confirm-recovery-preconditions",
            "--confirm-archive-sha256",
            &digest,
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("mainnet legacy promotion is disabled")
    );
    assert_eq!(std::fs::read(&output).unwrap(), original);
}

#[test]
fn rejects_a_seed_file_readable_by_group_or_others() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let seed = dir.path().join("btc-seed.hex");
    std::fs::write(&source, b"{\"utxos\":[]}").unwrap();
    std::fs::write(&seed, format!("{}\n", "01".repeat(64))).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o644)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "stage",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("0600"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn staged_legacy_state_still_blocks_daemon_startup() {
    use sha2::{Digest, Sha256};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let mut hasher = Sha256::new();
    hasher.update(b"vtorrent-btc-wallet-id-v1");
    hasher.update([1u8; 64]);
    let id = hex::encode(&hasher.finalize()[..8]);
    let output = dir.path().join(format!("btc_utxos-regtest-{id}.json"));
    let archive = dir.path().join("legacy-btc-state.json");
    let seed = write_seed(dir.path());
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
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
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
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("daemon did not reject staged legacy state");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("legacy BTC wallet history"));
    let original = b"{\"utxos\":[],\"next_index\":7}";
    assert_eq!(std::fs::read(&output).unwrap(), original);
    assert_eq!(std::fs::read(&archive).unwrap(), original);
}

#[test]
fn refuses_to_archive_without_a_matching_staged_copy() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    let seed = write_seed(dir.path());
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
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
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
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
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
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    let seed = write_seed(dir.path());
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
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
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
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!retry.status.success());
    assert_eq!(
        std::fs::read(&archive).unwrap(),
        b"{\"utxos\":[],\"swap_contracts\":{}}"
    );
}

#[test]
fn interrupted_archive_stage_blocks_retry_without_modifying_any_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    let stage = archive.with_extension("json.archive-stage");
    let seed = write_seed(dir.path());
    let original = b"{\"utxos\":[],\"swap_contracts\":{}}";
    std::fs::write(&source, original).unwrap();
    assert!(run(&source, &output).status.success());
    std::fs::write(&stage, b"partial interrupted archive").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(std::fs::read(&output).unwrap(), original);
    assert!(!archive.exists());
    assert_eq!(
        std::fs::read(&stage).unwrap(),
        b"partial interrupted archive"
    );
}

#[test]
fn interrupted_archive_publication_keeps_the_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("btc_utxos-regtest.json");
    let output = dir.path().join("btc_utxos-regtest-62d04ad2793b8849.json");
    let archive = dir.path().join("legacy-btc-state-20261113.json");
    let stage = archive.with_extension("json.archive-stage");
    let seed = write_seed(dir.path());
    let original = b"{\"utxos\":[],\"swap_contracts\":{}}";
    std::fs::write(&source, original).unwrap();
    assert!(run(&source, &output).status.success());
    std::fs::write(&archive, original).unwrap();
    std::fs::write(&stage, original).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_btc-state-stage"))
        .args([
            "archive",
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--archive",
            archive.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--seed-file",
            seed.to_str().unwrap(),
            "--network",
            "regtest",
        ])
        .args(["--confirm-isolated", "--confirm-owner"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&source).unwrap(), original);
    assert_eq!(std::fs::read(&output).unwrap(), original);
    assert_eq!(std::fs::read(&archive).unwrap(), original);
    assert_eq!(std::fs::read(&stage).unwrap(), original);
}
