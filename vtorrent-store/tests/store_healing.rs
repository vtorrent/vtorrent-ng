//! Integration tests for the block store's self-healing behavior.
//!
//! Covers the failure modes that motivated the healing machinery:
//! 1. A diverged/corrupted tail (e.g. from a pre-fix reorg) — `load_into_chain`
//!    must truncate to the last good height and rebuild derived state instead
//!    of refusing to boot.
//! 2. Full reconciliation after event loss — `rebuild_from_blocks` must
//!    reconstruct UTXO/claim state purely from an in-memory block list.

use vtorrent_node::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};
use vtorrent_node::chain::Chain;
use vtorrent_store::store::BlockStore;

fn make_block_value(
    prev_hash: [u8; 32],
    prev_stake_modifier: u64,
    height: u32,
    nonce: u32,
    value: u64,
) -> Block {
    let transactions = vec![Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0u8; 32],
            prev_vout: 0xffffffff,
            script_sig: vec![height as u8, nonce as u8],
            sequence: 0xffffffff,
        }],
        outputs: vec![TxOutput {
            value,
            script_pubkey: vec![
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
                15, 16, 17, 0x88, 0xac,
            ],
        }],
        lock_time: height,
        claim_address: None,
        claim_signature: None,
    }];
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: prev_hash,
            merkle_root: [0u8; 32],
            timestamp: 1_700_000_000 + height,
            bits: vtorrent_node::genesis::GENESIS_BITS,
            nonce,
            stake_modifier: vtorrent_node::consensus::compute_stake_modifier(
                prev_stake_modifier,
                &prev_hash,
            ),
        },
        transactions,
    };
    block.header.merkle_root = block.compute_merkle_root();
    block
}

/// Build a valid chain of `n` blocks in memory, returning them plus the chain.
fn build_chain(n: u32) -> (Chain, Vec<Block>) {
    let mut chain = Chain::new_regtest().unwrap();
    let mut blocks = Vec::new();
    let mut prev = chain.best_hash().unwrap();
    let mut modifier = 0u64;
    for i in 1..=n {
        let b = make_block_value(prev, modifier, i, i * 7, 1_000_000 + i as u64);
        chain.add_block(b.clone()).unwrap();
        modifier = b.header.stake_modifier;
        prev = b.hash();
        blocks.push(b);
    }
    (chain, blocks)
}

/// Scenario 1: the store's tail diverged (a block at height 4 whose parent
/// does not connect to our height-3 tip). `load_into_chain` must truncate
/// back to height 3 and rebuild derived tables.
#[test]
fn diverged_tail_is_truncated_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

    let (_chain, blocks) = build_chain(3);

    // Persist blocks 1..=3 with their real acceptance diffs.
    let mut replay = Chain::new_regtest().unwrap();
    for (i, b) in blocks.iter().enumerate() {
        let h = i as u32 + 1;
        match replay.add_block(b.clone()).unwrap() {
            vtorrent_node::chain::BlockAcceptance::MainChain {
                utxos_added,
                utxos_removed,
                claimed_addresses,
                ..
            } => {
                store
                    .append_block(b, h, &utxos_added, &utxos_removed, &claimed_addresses)
                    .unwrap();
            }
            _ => panic!("expected MainChain"),
        }
    }

    // Diverge: append a garbage block at height 4 claiming a parent that
    // nothing connects to (simulates a hybrid store from a pre-fix reorg).
    let garbage = make_block_value([0xDE; 32], 0, 4, 99, 42_000_000);
    let bogus_utxo = vtorrent_node::chain::Utxo {
        txid: [0xAA; 32],
        vout: 0,
        value: 42_000_000,
        script_pubkey: garbage.transactions[0].outputs[0].script_pubkey.clone(),
        height: 4,
        timestamp: 0,
    };
    store
        .append_block(&garbage, 4, std::slice::from_ref(&bogus_utxo), &[], &[])
        .unwrap();
    assert_eq!(store.best_height().unwrap(), 4);

    // Heal: must succeed and land on the last GOOD block.
    let loaded = store.load_into_regtest_chain().unwrap();
    assert_eq!(loaded.best_height(), 3);
    assert_eq!(loaded.best_hash(), Some(blocks[2].hash()));

    // Derived state must be rebuilt for exactly the surviving chain.
    assert_eq!(store.best_height().unwrap(), 3);
    let script = blocks[2].transactions[0].outputs[0].script_pubkey.clone();
    // Each coinbase mints 1_000_000 + height into the same script.
    assert_eq!(store.balance_for_script(&script).unwrap(), 3_000_006);

    // The bogus UTXO must be gone from disk.
    assert!(!store.has_utxo(&[0xAA; 32], 0).unwrap());
}

/// Scenario 2: full reconciliation from an in-memory block list after event
/// loss (`rebuild_from_blocks`).
#[test]
fn rebuild_from_blocks_restores_derived_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

    let (_chain, mut blocks) = build_chain(4);
    // rebuild_from_blocks expects the full list INCLUDING genesis at index 0
    // (the daemon collects 0..=best_height from the chain).
    let genesis = vtorrent_node::genesis::create_genesis_block();
    blocks.insert(0, genesis);

    for (i, b) in blocks.iter().enumerate() {
        println!(
            "block idx={} lock_time={} parent={}",
            i,
            b.transactions[0].lock_time,
            &hex::encode(b.header.prev_block_hash)[..8]
        );
    }
    store.rebuild_from_regtest_blocks(&blocks).unwrap();

    assert_eq!(store.best_height().unwrap(), 4);
    assert_eq!(
        store.best_hash().unwrap(),
        Some(blocks.last().unwrap().hash())
    );
    assert_eq!(store.block_count().unwrap(), 5); // genesis + 4

    // UTXO set must contain every coinbase output of the rebuilt chain.
    for (i, b) in blocks.iter().enumerate().skip(1) {
        let key_txid = b.transactions[0].txid();
        assert!(
            store.has_utxo(&key_txid, 0).unwrap(),
            "coinbase {} missing from rebuilt UTXO set",
            i
        );
    }

    // Idempotent: rebuilding twice lands in the same place.
    store.rebuild_from_regtest_blocks(&blocks).unwrap();
    assert_eq!(store.best_height().unwrap(), 4);
}
