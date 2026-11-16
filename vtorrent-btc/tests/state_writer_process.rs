#![cfg(unix)]

use bitcoin::Network;
use vtorrent_btc::utxo::{Utxo, UtxoSet};
use vtorrent_btc::wallet::BtcWallet;

#[test]
fn another_process_cannot_open_state_with_pending_send() {
    const CHILD_PATH: &str = "VTR_BTC_LOCK_TEST_CHILD_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let error = BtcWallet::with_persistence([7; 64], Network::Regtest, path.into())
            .err()
            .expect("another process must not open the state");
        assert!(error.to_string().contains("in use"));
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "vtr-btc-process-lock-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("utxos.json");
    UtxoSet::new().save(&path).unwrap();
    let mut wallet = BtcWallet::with_persistence([7; 64], Network::Regtest, path.clone()).unwrap();
    let owned = Utxo {
        txid: "ab".repeat(32),
        vout: 0,
        value: 100_000,
        address: wallet.current_address().unwrap(),
        height: 100,
    };
    wallet.add_utxo(owned.clone());
    let recipient = vtorrent_btc::keys::derive_address(&[9; 64], 0, Network::Regtest).unwrap();
    let (txid, raw, _) = wallet.send_to(&recipient, 50_000, 1_000).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "another_process_cannot_open_state_with_pending_send",
        ])
        .env(CHILD_PATH, &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    drop(wallet);
    let reopened = BtcWallet::with_persistence([7; 64], Network::Regtest, path.clone()).unwrap();
    assert_eq!(reopened.pending_send_raw(&txid), Some(raw));
    assert!(!UtxoSet::load(&path).unwrap().add(owned));
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}
