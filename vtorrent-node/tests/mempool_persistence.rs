//! Integration test: mempool entries survive save → reload.

use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};
use vtorrent_node::mempool::Mempool;

fn make_tx(nonce: u8, value: u64) -> Transaction {
    Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: [nonce; 32],
            prev_vout: 0,
            script_sig: vec![nonce],
            sequence: 0xffff_ffff,
        }],
        outputs: vec![TxOutput {
            value,
            script_pubkey: vec![0x51],
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    }
}

#[test]
fn mempool_save_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mempool.json");

    let mut mp = Mempool::new(10_000);
    let tx1 = make_tx(1, 5_000_000);
    let tx2 = make_tx(2, 3_000_000);
    mp.add_transaction_with_fee(tx1.clone(), 1_000).unwrap();
    mp.add_transaction_with_fee(tx2.clone(), 1_500).unwrap();

    // Save
    mp.save_to(&path).unwrap();
    assert!(path.exists(), "mempool.json must exist after save");

    // Load
    let loaded = Mempool::load_saved(&path);
    assert_eq!(loaded.len(), 2, "both entries must survive roundtrip");

    // Verify txids match
    let loaded_txids: Vec<[u8; 32]> = loaded.iter().map(|(tx, _)| tx.txid()).collect();
    assert!(loaded_txids.contains(&tx1.txid()));
    assert!(loaded_txids.contains(&tx2.txid()));

    // Verify fees match
    for (tx, fee) in &loaded {
        if tx.txid() == tx1.txid() {
            assert_eq!(*fee, 1_000);
        }
        if tx.txid() == tx2.txid() {
            assert_eq!(*fee, 1_500);
        }
    }
}

#[test]
fn mempool_load_missing_file_returns_empty() {
    let loaded = Mempool::load_saved(std::path::Path::new("/nonexistent/mempool.json"));
    assert!(loaded.is_empty());
}

#[test]
fn mempool_load_corrupt_file_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mempool.json");
    std::fs::write(&path, b"not valid json").unwrap();
    let loaded = Mempool::load_saved(&path);
    assert!(loaded.is_empty(), "corrupt file must not panic");
}
