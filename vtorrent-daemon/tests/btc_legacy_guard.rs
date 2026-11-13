#![cfg(unix)]

use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn legacy_btc_wallet_blocks_startup_without_modifying_the_file() {
    assert_legacy_guard(true, false);
}

#[test]
fn legacy_and_seed_scoped_state_both_remain_untouched() {
    assert_legacy_guard(true, true);
}

#[test]
fn mainnet_legacy_btc_wallet_blocks_startup() {
    assert_legacy_guard(false, false);
}

#[test]
fn oldest_unlabelled_btc_wallet_blocks_startup() {
    assert_legacy_guard_named(true, false, Some("btc_utxos.json"));
}

#[test]
fn wallet_state_round_trip_preserves_swap_metadata_after_chain_reset() {
    use vtorrent_btc::utxo::{SwapContract, Utxo, UtxoSet};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("btc_utxos-regtest.json");
    let mut state = UtxoSet::new();
    state.reserve_utxo(Utxo {
        txid: "reserved".into(),
        vout: 1,
        value: 5000,
        address: "test-address".into(),
        height: 50,
    });
    state.reserve("spent", 2);
    assert!(state.note_spend("spent", 2));
    state.record_swap_transaction("funding".into(), vec![1, 2, 3]);
    state.record_swap_contract(SwapContract {
        order_id: "order".into(),
        funding_txid: "funding".into(),
        hash_lock: [7; 32],
        recipient: "recipient".into(),
        refund_address: "refund".into(),
        expiry: 500,
        amount: 1000,
        refund_raw: Some(vec![4, 5, 6]),
        input_txid: None,
        input_vout: None,
    });
    state.save(&path).unwrap();

    let mut restored = UtxoSet::load(&path).unwrap();
    restored.clear_chain_utxos();
    restored.save(&path).unwrap();
    let restored = UtxoSet::load(&path).unwrap();

    assert_eq!(restored.reserved_utxo("reserved", 1).unwrap().value, 5000);
    assert!(restored.was_reserved_spent("spent", 2));
    assert_eq!(restored.pending_swap_transactions()["funding"], [1, 2, 3]);
    let contract = restored.swap_contract("order").unwrap();
    assert_eq!(contract.refund_raw.as_deref(), Some([4, 5, 6].as_slice()));
    assert!(contract.input_txid.is_none());
}

fn assert_legacy_guard(regtest: bool, with_scoped: bool) {
    assert_legacy_guard_named(regtest, with_scoped, None);
}

fn assert_legacy_guard_named(regtest: bool, with_scoped: bool, legacy_name: Option<&str>) {
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let label = if regtest { "regtest" } else { "mainnet" };
    let legacy = dir.path().join(
        legacy_name
            .map(str::to_owned)
            .unwrap_or_else(|| format!("btc_utxos-{label}.json")),
    );
    let contents = b"legacy wallet state must be preserved";
    std::fs::write(&legacy, contents).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(b"vtorrent-btc-wallet-id-v1");
    hasher.update([1u8; 64]);
    let id = hex::encode(&hasher.finalize()[..8]);
    let scoped = dir.path().join(format!("btc_utxos-{label}-{id}.json"));
    if with_scoped {
        std::fs::write(&scoped, b"existing seed-scoped wallet state").unwrap();
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_vtorrent-daemon"));
    command.args(["--regtest", "--testnet", "--isolated", "--no-dht"]);
    if regtest {
        command.arg("--btc-regtest");
    }
    let mut child = command
        .arg("--data-dir")
        .arg(dir.path())
        .env("VTORRENT_BTC_SEED", "01".repeat(64))
        .env_remove("VTORRENT_RPC_API_KEY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let output = child.wait_with_output().unwrap();
    assert_eq!(std::fs::read(legacy).unwrap(), contents);
    if with_scoped {
        assert_eq!(
            std::fs::read(scoped).unwrap(),
            b"existing seed-scoped wallet state"
        );
    }
    assert!(
        status.is_some_and(|status| !status.success()),
        "daemon did not refuse the legacy wallet"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("legacy BTC wallet state"));
}
