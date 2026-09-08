use super::*;
use vtorrent_node::{
    block::{Block, Transaction, TxInput, TxOutput, TxType},
    chain::{BlockAcceptance, Chain, Utxo},
};

fn replay(blocks: &[Block]) -> Chain {
    let mut chain = Chain::new_regtest().unwrap();
    for block in blocks {
        assert!(matches!(
            chain.add_block(block.clone()).unwrap(),
            BlockAcceptance::MainChain { .. }
        ));
    }
    chain
}

fn extend(chain: &mut Chain, tag: u8, spend: Option<([u8; 32], u64)>) -> Block {
    let current: Vec<_> = (1..=chain.best_height())
        .map(|h| chain.get_block_at_height(h).unwrap().clone())
        .collect();
    let mut template = replay(&current);
    let address = vtorrent_core::address::Address::from_hash160(&[tag; 20], 70)
        .unwrap()
        .to_string();
    template
        .mint_to_address(&address, u64::from(tag) * 100_000)
        .unwrap();
    let mut block = template
        .get_block_at_height(template.best_height())
        .unwrap()
        .clone();
    block.transactions[0].outputs[0].script_pubkey = vec![0x51];
    if let Some((txid, value)) = spend {
        block.transactions.push(Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: txid,
                prev_vout: 0,
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            outputs: vec![TxOutput {
                value,
                script_pubkey: vec![0x51],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        });
        let mut child = block.transactions.last().unwrap().clone();
        child.inputs[0].prev_txid = child.txid();
        child.outputs[0].value -= 10_000;
        block.transactions.push(child);
    }
    block.header.merkle_root = block.compute_merkle_root();
    assert!(matches!(
        chain.add_block(block.clone()).unwrap(),
        BlockAcceptance::MainChain { .. }
    ));
    block
}

fn fixture() -> (Vec<Block>, Vec<Block>) {
    let mut old = Chain::new_regtest().unwrap();
    let common = extend(&mut old, 1, None);
    let old_first = extend(&mut old, 2, Some((common.transactions[0].txid(), 90_000)));
    let old_second = extend(
        &mut old,
        3,
        Some((old_first.transactions.last().unwrap().txid(), 70_000)),
    );
    let mut fork = replay(std::slice::from_ref(&common));
    let fork_first = extend(&mut fork, 4, Some((common.transactions[0].txid(), 85_000)));
    let fork_second = extend(
        &mut fork,
        5,
        Some((fork_first.transactions.last().unwrap().txid(), 65_000)),
    );
    let fork_third = extend(
        &mut fork,
        6,
        Some((fork_second.transactions.last().unwrap().txid(), 45_000)),
    );
    (
        vec![common, old_first, old_second],
        vec![fork_first, fork_second, fork_third],
    )
}

fn checkpoint(directory: &Path, requested: usize, completed: usize) {
    if completed == requested {
        std::fs::write(directory.join("paused"), completed.to_string()).unwrap();
        loop {
            std::thread::park();
        }
    }
}

#[test]
fn reorg_writer_child() {
    let Some(directory) = std::env::var_os("VTR_TEST_REORG_DIRECTORY") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let requested: usize = std::env::var("VTR_TEST_REORG_BOUNDARY")
        .unwrap()
        .parse()
        .unwrap();
    let (old, fork): (Vec<Block>, Vec<Block>) =
        serde_json::from_slice(&std::fs::read(directory.join("fixture.json")).unwrap()).unwrap();
    let store = BlockStore::open(directory.join("chain.db")).unwrap();
    let mut chain = Chain::new_regtest().unwrap();
    for block in old {
        let BlockAcceptance::MainChain {
            height,
            utxos_added,
            utxos_removed,
            claimed_addresses,
        } = chain.add_block(block.clone()).unwrap()
        else {
            panic!("expected main chain");
        };
        store
            .append_block(
                &block,
                height,
                &utxos_added,
                &utxos_removed,
                &claimed_addresses,
            )
            .unwrap();
    }
    let mut acceptance = BlockAcceptance::Duplicate;
    for block in fork {
        acceptance = chain.add_block(block).unwrap();
    }
    let BlockAcceptance::Reorg {
        depth,
        rolled_back_blocks,
        applied_fork_blocks,
        ..
    } = acceptance
    else {
        panic!("expected reorg");
    };
    assert_eq!(depth, 2);
    assert_eq!(rolled_back_blocks.len(), 2);
    assert_eq!(applied_fork_blocks.len(), 3);
    checkpoint(&directory, requested, 0);
    let mut completed = 0;
    for rb in rolled_back_blocks {
        store
            .rollback_tip(
                &rb.utxos_to_restore,
                &rb.utxos_to_remove,
                &rb.claimed_to_remove,
            )
            .unwrap();
        completed += 1;
        checkpoint(&directory, requested, completed);
    }
    for fb in applied_fork_blocks {
        store
            .append_block(
                &fb.block,
                fb.height,
                &fb.utxos_added,
                &fb.utxos_removed,
                &fb.claimed_addresses,
            )
            .unwrap();
        completed += 1;
        checkpoint(&directory, requested, completed);
    }
    panic!("requested checkpoint was not reached");
}

fn assert_store(directory: &Path, expected: &Chain) {
    let store = BlockStore::open(directory.join("chain.db")).unwrap();
    assert_eq!(store.best_height().unwrap(), expected.best_height());
    assert_eq!(store.best_hash().unwrap(), expected.best_hash());
    let mut actual_utxos = store.all_utxos().unwrap();
    actual_utxos.sort_by_key(|utxo| (utxo.txid, utxo.vout));
    let expected_utxos: Vec<Utxo> = expected
        .get_utxo_set()
        .values()
        .filter(|utxo| utxo.height > 0)
        .cloned()
        .collect();
    assert_eq!(
        actual_utxos,
        expected_utxos,
        "raw UTXO mismatch in {}",
        directory.display()
    );
    assert!(store.all_claimed_addresses().unwrap().is_empty());
    for height in 1..=expected.best_height() {
        assert_eq!(
            store.get_block_at_height(height).unwrap().unwrap().hash(),
            expected.get_block_at_height(height).unwrap().hash()
        );
    }
    assert!(store
        .get_block_at_height(expected.best_height() + 1)
        .unwrap()
        .is_none());
    let recovered = store.load_into_regtest_chain().unwrap();
    assert_eq!(recovered.best_hash(), expected.best_hash());
    assert_eq!(recovered.get_utxo_set(), expected.get_utxo_set());
}

#[tokio::test]
async fn daemon_recovers_at_every_reorg_write_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let (old, fork) = fixture();
    let mut winning_blocks = vec![old[0].clone()];
    winning_blocks.extend(fork.clone());
    let winning_chain = replay(&winning_blocks);
    let peer_dir = directory.path().join("winning-peer");
    std::fs::create_dir_all(&peer_dir).unwrap();
    {
        let store = BlockStore::open(peer_dir.join("chain.db")).unwrap();
        let mut all_blocks = vec![winning_chain.get_block_at_height(0).unwrap().clone()];
        all_blocks.extend(winning_blocks.clone());
        store.rebuild_from_regtest_blocks(&all_blocks).unwrap();
    }
    for boundary in 0..=5 {
        let case_dir = directory.path().join(format!("boundary-{boundary}"));
        std::fs::create_dir_all(&case_dir).unwrap();
        std::fs::write(
            case_dir.join("fixture.json"),
            serde_json::to_vec(&(&old, &fork)).unwrap(),
        )
        .unwrap();
        let log_path = case_dir.join("writer.log");
        let output = std::fs::File::create(&log_path).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "interrupted_reorg::reorg_writer_child",
                "--nocapture",
            ])
            .env("VTR_TEST_REORG_DIRECTORY", &case_dir)
            .env("VTR_TEST_REORG_BOUNDARY", boundary.to_string())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while !case_dir.join("paused").exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "writer exited at boundary {boundary}: {}",
                    std::fs::read_to_string(&log_path).unwrap()
                );
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("writer did not reach boundary {boundary}"));
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        let prefix = if boundary <= 2 {
            old[..old.len() - boundary].to_vec()
        } else {
            winning_blocks[..boundary - 1].to_vec()
        };
        let expected = replay(&prefix);
        assert_store(&case_dir, &expected);
        let recovered = Daemon::start(&case_dir, "recovered.log", None).await;
        let info = recovered.get("/api/v1/info").await;
        assert_eq!(info["block_height"], expected.best_height());
        assert_eq!(
            info["best_block_hash"],
            hex::encode(expected.best_hash().unwrap())
        );
        recovered.stop(false).await;
        let peer = Daemon::start(&peer_dir, &format!("peer-{boundary}.log"), None).await;
        let recovered = Daemon::start(&case_dir, "resync.log", Some(&peer.p2p)).await;
        recovered
            .wait_tip(&hex::encode(winning_chain.best_hash().unwrap()))
            .await;
        peer.stop(false).await;
        recovered.mint(20 + boundary as u8).await;
        recovered.persisted(5).await;
        recovered.stop(false).await;
        let store = BlockStore::open(case_dir.join("chain.db")).unwrap();
        let final_chain = store.load_into_regtest_chain().unwrap();
        assert_eq!(final_chain.best_height(), 5);
        assert_eq!(
            final_chain.get_block_at_height(4).unwrap().hash(),
            winning_chain.best_hash().unwrap()
        );
        for block in &old[1..] {
            for tx in &block.transactions {
                assert!(final_chain.get_transaction(&tx.txid()).is_none());
            }
        }
        eprintln!("Verified reorg persistence boundary {boundary}/5");
    }
}
