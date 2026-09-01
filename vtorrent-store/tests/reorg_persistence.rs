//! Integration test: a chain reorg must be persisted correctly.
//!
//! Simulates exactly what the daemon's event bridge does on
//! `NodeEvent::Reorg`: `rollback_tip` for every abandoned block (tip first),
//! then `append_block` for every fork block now canonical. After the bridge
//! runs, `load_into_chain` must replay the FORK chain — not a hybrid of two
//! chains (the pre-fix failure mode that bricked nodes at startup).

use vtorrent_node::block::{
    compute_utxo_root_sorted, Block, BlockHeader, Transaction, TxInput, TxOutput, TxType,
};
use vtorrent_node::chain::{BlockAcceptance, Chain, Utxo};
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
                0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0x88, 0xac,
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
            utxo_root: [0u8; 32],
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

/// Persist a MainChain acceptance the way the daemon bridge does.
fn persist_main(store: &BlockStore, block: &Block, height: u32, acceptance: &BlockAcceptance) {
    if let BlockAcceptance::MainChain {
        utxos_added,
        utxos_removed,
        claimed_addresses,
        ..
    } = acceptance
    {
        store
            .append_block(block, height, utxos_added, utxos_removed, claimed_addresses)
            .unwrap();
    } else {
        panic!("expected MainChain acceptance");
    }
}

#[test]
fn reorg_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlockStore::open(dir.path().join("chain.db")).unwrap();
    let mut chain = Chain::new_regtest().unwrap();
    let genesis_hash = chain.best_hash().unwrap();

    // ── Main chain A: genesis → A1 → A2 ─────────────────────────────────────
    let a1 = make_block_value(genesis_hash, 0, 1, 1, 1_000_000);
    let acc = chain.add_block(a1.clone()).unwrap();
    let a1_stored = chain.get_block_at_height(1).unwrap().clone();
    persist_main(&store, &a1_stored, 1, &acc);

    let a2 = make_block_value(
        a1_stored.hash(),
        a1_stored.header.stake_modifier,
        2,
        2,
        1_000_000,
    );
    let acc = chain.add_block(a2.clone()).unwrap();
    let a2_stored = chain.get_block_at_height(2).unwrap().clone();
    persist_main(&store, &a2_stored, 2, &acc);
    let tip_a = a2_stored.hash();

    // ── Fork B: B1 competes at height 1 (Fork acceptance — NOT persisted,
    // matching daemon behaviour), then B2 makes the fork longer ────────────
    // Build fork chain with correct utxo_roots so reorg's sequential prev
    // hashes (which become corrected after reorg) stay consistent.
    let genesis_block = vtorrent_node::genesis::create_genesis_block();
    let coinbase_txid = genesis_block.transactions[0].txid();
    let legacy_txid = genesis_block.transactions[1].txid();
    let mut genesis_utxos: Vec<Utxo> =
        Vec::with_capacity(1 + genesis_block.transactions[1].outputs.len());
    genesis_utxos.push(Utxo {
        txid: coinbase_txid,
        vout: 0,
        value: genesis_block.transactions[0].outputs[0].value,
        script_pubkey: genesis_block.transactions[0].outputs[0]
            .script_pubkey
            .clone(),
        height: 0,
        timestamp: vtorrent_node::genesis::GENESIS_TIMESTAMP,
    });
    for (i, o) in genesis_block.transactions[1].outputs.iter().enumerate() {
        genesis_utxos.push(Utxo {
            txid: legacy_txid,
            vout: i as u32,
            value: o.value,
            script_pubkey: o.script_pubkey.clone(),
            height: 0,
            timestamp: vtorrent_node::genesis::GENESIS_TIMESTAMP,
        });
    }

    let mut b1 = make_block_value(genesis_hash, 0, 1, 77, 2_000_000);
    b1.header.timestamp += 1;
    b1.header.merkle_root = b1.compute_merkle_root();
    {
        let txid = b1.transactions[0].txid();
        let mut utxos = genesis_utxos.clone();
        utxos.push(Utxo {
            txid,
            vout: 0,
            value: 2_000_000,
            script_pubkey: b1.transactions[0].outputs[0].script_pubkey.clone(),
            height: 1,
            timestamp: b1.header.timestamp,
        });
        b1.header.utxo_root = compute_utxo_root_sorted(&utxos);
    }
    let acc_b1 = chain.add_block(b1.clone()).unwrap();
    assert!(matches!(acc_b1, BlockAcceptance::Fork { .. }));

    let mut b2 = make_block_value(b1.hash(), b1.header.stake_modifier, 2, 78, 2_000_000);
    b2.header.timestamp += 1;
    b2.header.merkle_root = b2.compute_merkle_root();
    {
        let txid = b2.transactions[0].txid();
        let mut utxos2 = genesis_utxos.clone();
        let t1 = b1.transactions[0].txid();
        utxos2.push(Utxo {
            txid: t1,
            vout: 0,
            value: 2_000_000,
            script_pubkey: b1.transactions[0].outputs[0].script_pubkey.clone(),
            height: 1,
            timestamp: b1.header.timestamp,
        });
        utxos2.push(Utxo {
            txid,
            vout: 0,
            value: 2_000_000,
            script_pubkey: b2.transactions[0].outputs[0].script_pubkey.clone(),
            height: 2,
            timestamp: b2.header.timestamp,
        });
        b2.header.utxo_root = compute_utxo_root_sorted(&utxos2);
    }
    let acc_b2 = chain.add_block(b2.clone()).unwrap();
    assert!(matches!(acc_b2, BlockAcceptance::Fork { .. }));

    // B3 makes the fork strictly longer than main → reorg.
    let mut b3 = make_block_value(b2.hash(), b2.header.stake_modifier, 3, 79, 2_000_000);
    b3.header.timestamp += 2;
    b3.header.merkle_root = b3.compute_merkle_root();
    {
        let txid = b3.transactions[0].txid();
        let mut utxos3 = genesis_utxos.clone();
        for (blk, h) in [(&b1, 1u32), (&b2, 2u32)] {
            let tid = blk.transactions[0].txid();
            utxos3.push(Utxo {
                txid: tid,
                vout: 0,
                value: 2_000_000,
                script_pubkey: blk.transactions[0].outputs[0].script_pubkey.clone(),
                height: h,
                timestamp: blk.header.timestamp,
            });
        }
        utxos3.push(Utxo {
            txid,
            vout: 0,
            value: 2_000_000,
            script_pubkey: b3.transactions[0].outputs[0].script_pubkey.clone(),
            height: 3,
            timestamp: b3.header.timestamp,
        });
        b3.header.utxo_root = compute_utxo_root_sorted(&utxos3);
    }
    let acc_b3 = chain.add_block(b3.clone()).unwrap();

    // ── Bridge simulation: undo abandoned blocks, persist fork blocks ──────
    let (rolled_back_txs, rolled_back_blocks, applied_fork_blocks) = match &acc_b3 {
        BlockAcceptance::Reorg {
            rolled_back_txs,
            rolled_back_blocks,
            applied_fork_blocks,
            ..
        } => (
            rolled_back_txs.clone(),
            rolled_back_blocks.clone(),
            applied_fork_blocks.clone(),
        ),
        other => panic!("expected Reorg, got {:?}", other),
    };

    assert_eq!(rolled_back_blocks.len(), 2, "A1 + A2 must be rolled back");
    assert_eq!(rolled_back_blocks[0].hash, tip_a);
    assert_eq!(applied_fork_blocks.len(), 3, "B1 + B2 + B3 must be applied");
    assert!(!rolled_back_txs.is_empty());

    for rb in &rolled_back_blocks {
        store
            .rollback_tip(
                &rb.utxos_to_restore,
                &rb.utxos_to_remove,
                &rb.claimed_to_remove,
            )
            .unwrap();
    }
    for fb in &applied_fork_blocks {
        let corrected = chain.get_block_at_height(fb.height).unwrap().clone();
        store
            .append_block(
                &corrected,
                fb.height,
                &fb.utxos_added,
                &fb.utxos_removed,
                &fb.claimed_addresses,
            )
            .unwrap();
    }

    // ── Verify: fresh load replays the FORK chain, not a hybrid ────────────
    let loaded = store.load_into_regtest_chain().unwrap();
    assert_eq!(loaded.best_height(), 3);
    assert_eq!(loaded.best_hash(), chain.best_hash());
    assert_ne!(loaded.best_hash(), Some(tip_a));

    // The store's UTXO set must reflect the fork chain's coinbases.
    let total = store.balance_for_script(&b3.transactions[0].outputs[0].script_pubkey);
    assert_eq!(
        total.unwrap(),
        6_000_000,
        "B1+B2+B3 coinbase values on fork"
    );

    // In-memory and on-disk chains must agree block-for-block.
    for h in 1..=3u32 {
        let mem = loaded.get_block_at_height(h).unwrap().hash();
        let disk = store.get_block_at_height(h).unwrap().unwrap().hash();
        assert_eq!(mem, disk, "height {} diverges between memory and disk", h);
    }
}
