use super::*;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use vtorrent_node::{
    block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType},
    chain::{BlockAcceptance, Chain, Utxo},
    consensus::{compute_stake_modifier, COIN, MIN_STAKE_AGE},
    governance::{
        proposal_id_of, ParamId, Proposal, SignedVote, Vote, VoteChoice, DEFAULT_LOCK_GRACE_BLOCKS,
        MIN_PROPOSAL_DEPOSIT,
    },
};

const VOTERS: usize = 32;
const FUNDING_TIME: u32 = 1_700_000_001;

fn key(owner: u8) -> SecretKey {
    SecretKey::from_slice(&[owner; 32]).unwrap()
}
fn address(owner: u8) -> String {
    vtorrent_core::address::Address::from_pubkey(
        &PublicKey::from_secret_key(&Secp256k1::new(), &key(owner)),
        true,
        70,
    )
    .to_string()
}
fn output(owner: u8, value: u64) -> TxOutput {
    TxOutput {
        value,
        script_pubkey: vtorrent_core::address::Address::parse(&address(owner))
            .unwrap()
            .p2pkh_script_pubkey(),
    }
}
fn chain() -> Chain {
    let mut chain = Chain::new_regtest().unwrap();
    chain.set_governance_test_activation(2);
    chain
}
fn block(chain: &Chain, branch: u8, timestamp: u32, extra: Vec<Transaction>) -> Block {
    let height = chain.best_height() + 1;
    let parent = chain.best_hash().unwrap();
    let mut transactions = vec![Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![TxInput {
            prev_txid: [0; 32],
            prev_vout: u32::MAX,
            script_sig: vec![height as u8, branch],
            sequence: u32::MAX,
        }],
        outputs: vec![output(240, 1)],
        lock_time: height,
        claim_address: None,
        claim_signature: None,
    }];
    transactions.extend(extra);
    let mut block = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: parent,
            merkle_root: [0; 32],
            utxo_root: [0; 32],
            timestamp,
            bits: vtorrent_node::genesis::GENESIS_BITS,
            nonce: height + u32::from(branch) * 100,
            stake_modifier: compute_stake_modifier(
                chain
                    .get_header_at_height(height - 1)
                    .unwrap()
                    .stake_modifier,
                &parent,
            ),
        },
        transactions,
    };
    block.header.merkle_root = block.compute_merkle_root();
    block
}
fn spend(chain: &Chain, owner: u8, carriers: Vec<TxOutput>) -> Transaction {
    let input = chain.get_utxos_for_address(&address(owner))[0].clone();
    let total: u64 = carriers.iter().map(|o| o.value).sum();
    let mut outputs = carriers;
    outputs.push(output(owner, input.value - total - COIN));
    let mut tx = Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![TxInput {
            prev_txid: input.txid,
            prev_vout: input.vout,
            script_sig: vec![],
            sequence: u32::MAX,
        }],
        outputs,
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };
    let secp = Secp256k1::new();
    let mut sig = secp
        .sign_ecdsa(
            &Message::from_digest(tx.sighash(0, &input.script_pubkey)),
            &key(owner),
        )
        .serialize_der()
        .to_vec();
    sig.push(1);
    let mut script = vtorrent_script::Script::new();
    script.push_data(&sig).unwrap();
    script
        .push_data(&PublicKey::from_secret_key(&secp, &key(owner)).serialize())
        .unwrap();
    tx.inputs[0].script_sig = script.as_bytes().to_vec();
    tx
}
fn replay(blocks: &[Block]) -> Chain {
    let mut chain = chain();
    for block in blocks {
        chain.add_block(block.clone()).unwrap();
    }
    chain
}
fn vote(chain: &Chain, proposal: [u8; 8], payer: u8, voters: usize) -> Transaction {
    let genesis = chain.get_block_at_height(0).unwrap().hash();
    let carriers = (0..voters)
        .map(|i| {
            let owner = 50 + i as u8;
            let input = chain.get_utxos_for_address(&address(owner))[0].clone();
            let signed = SignedVote::sign(
                Vote {
                    proposal_id: proposal,
                    choice: VoteChoice::Yes,
                },
                (input.txid, input.vout),
                genesis,
                &key(owner),
            );
            TxOutput {
                value: 1,
                script_pubkey: signed.script().unwrap(),
            }
        })
        .collect();
    spend(chain, payer, carriers)
}

struct Fixture {
    old: Vec<Block>,
    winning: Vec<Block>,
    ids: [[u8; 8]; 2],
}

fn fixture() -> Fixture {
    let mut common = chain();
    let mut funding = block(&common, 0, FUNDING_TIME, vec![]);
    funding.transactions[0].outputs = vec![
        output(2, 1000 * COIN),
        output(3, 1000 * COIN),
        output(10, 100 * COIN),
        output(11, 100 * COIN),
    ];
    funding.transactions[0]
        .outputs
        .extend((0..VOTERS).map(|i| output(50 + i as u8, 2 * COIN)));
    funding.header.merkle_root = funding.compute_merkle_root();
    common.add_block(funding.clone()).unwrap();
    let proposals: Vec<_> = (0..2)
        .map(|i| Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400 + i,
            voting_end: 4000 + i as u32 * 100,
            activation_height: 7000 + i as u32 * 100,
            deposit: MIN_PROPOSAL_DEPOSIT,
        })
        .collect();
    let txs = proposals
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mut script = vtorrent_script::Script::new();
            script.push_opcode(0x6a);
            script.push_data(&p.encode()).unwrap();
            spend(
                &common,
                2 + i as u8,
                vec![TxOutput {
                    value: p.deposit,
                    script_pubkey: script.as_bytes().to_vec(),
                }],
            )
        })
        .collect();
    let proposed = block(&common, 0, FUNDING_TIME + 1, txs);
    common.add_block(proposed.clone()).unwrap();
    let ids = [
        proposal_id_of(&proposals[0], 2),
        proposal_id_of(&proposals[1], 2),
    ];
    let base = vec![funding, proposed];
    let mut old = base.clone();
    for id in ids {
        let tx = vote(&common, id, 10, VOTERS);
        let b = block(
            &common,
            1,
            FUNDING_TIME + MIN_STAKE_AGE as u32 + common.best_height(),
            vec![tx],
        );
        common.add_block(b.clone()).unwrap();
        old.push(b);
    }
    assert_eq!(common.governance.locked_utxos.len(), VOTERS);
    assert!(common
        .governance
        .locked_utxos
        .values()
        .all(|lock| lock.unlock_height == 4106));
    let mut fork = replay(&base);
    let mut winning = base;
    for (i, id) in ids.iter().enumerate() {
        let tx = vote(&fork, *id, 11, if i == 0 { VOTERS } else { VOTERS / 2 });
        let b = block(
            &fork,
            2,
            FUNDING_TIME + MIN_STAKE_AGE as u32 + fork.best_height(),
            vec![tx],
        );
        fork.add_block(b.clone()).unwrap();
        winning.push(b);
    }
    let b = block(&fork, 2, FUNDING_TIME + MIN_STAKE_AGE as u32 + 4, vec![]);
    fork.add_block(b.clone()).unwrap();
    winning.push(b);
    assert_eq!(
        fork.governance.proposals[&ids[0]].yes,
        VOTERS as u64 * 2 * COIN
    );
    assert_eq!(
        fork.governance.proposals[&ids[1]].yes,
        (VOTERS / 2) as u64 * 2 * COIN
    );
    for i in 0..VOTERS {
        let input = fork.get_utxos_for_address(&address(50 + i as u8))[0].clone();
        assert_eq!(
            fork.governance.locked_utxos[&(input.txid, input.vout)].unlock_height,
            if i < VOTERS / 2 { 4100 } else { 4000 } + DEFAULT_LOCK_GRACE_BLOCKS
        );
    }
    Fixture { old, winning, ids }
}

fn persist(directory: &Path, blocks: &[Block]) -> Chain {
    std::fs::create_dir_all(directory).unwrap();
    let store = BlockStore::open(directory.join("chain.db")).unwrap();
    let mut chain = chain();
    for block in blocks {
        let BlockAcceptance::MainChain {
            height,
            utxos_added,
            utxos_removed,
            claimed_addresses,
        } = chain.add_block(block.clone()).unwrap()
        else {
            panic!("fixture must extend main chain");
        };
        store
            .append_block(
                block,
                height,
                &utxos_added,
                &utxos_removed,
                &claimed_addresses,
            )
            .unwrap();
    }
    chain
}

async fn assert_governance(daemon: &Daemon, expected: &Chain) {
    let proposals = daemon.get("/api/v1/governance/proposals").await;
    let locks = daemon.get("/api/v1/governance/locks").await;
    assert_eq!(proposals["tip_height"], expected.best_height());
    assert_eq!(locks["tip_height"], expected.best_height());
    let records = proposals["proposals"].as_array().unwrap();
    assert_eq!(records.len(), expected.governance.proposals.len());
    for (id, p) in &expected.governance.proposals {
        let row = records.iter().find(|r| r["id"] == hex::encode(id)).unwrap();
        assert_eq!(row["yes"], p.yes);
        assert_eq!(row["no"], p.no);
        assert_eq!(row["abstain"], p.abstain);
        assert_eq!(row["voting_end"], p.proposal.voting_end);
        assert_eq!(row["created_height"], p.created_height);
        assert_eq!(row["new_value"], p.proposal.new_value);
        assert_eq!(row["activation_height"], p.proposal.activation_height);
        assert_eq!(row["quorum_supply"], p.quorum_supply);
        assert_eq!(row["applied"], p.applied);
        assert_eq!(row["decided"], p.decided);
        assert_eq!(row["passed"], p.passed);
        assert_eq!(row["activation_failed"], p.activation_failed);
    }
    let actual_locks = locks["locks"].as_array().unwrap();
    assert_eq!(actual_locks.len(), expected.governance.locked_utxos.len());
    for ((txid, vout), lock) in &expected.governance.locked_utxos {
        let row = actual_locks
            .iter()
            .find(|l| l["txid"] == hex::encode(txid) && l["vout"] == *vout)
            .unwrap();
        assert_eq!(row["unlock_height"], lock.unlock_height);
        assert_eq!(row["proposal_id"], hex::encode(lock.proposal_id));
        assert_eq!(row["voter"], lock.voter);
        assert_eq!(
            row["remaining_blocks"],
            lock.unlock_height - expected.best_height()
        );
    }
    let params = daemon.get("/api/v1/governance/params").await;
    let p = expected.params();
    assert_eq!(
        params,
        json!({ "pos_annual_rate_bps": p.pos_annual_rate_bps,
        "reward_age_cap": p.reward_age_cap, "min_stake_age": p.min_stake_age,
        "max_stake_age": p.max_stake_age, "min_stake_amount": p.min_stake_amount,
        "target_block_time": p.target_block_time })
    );
}

fn rss(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .expect("daemon VmRSS missing")
}

async fn run(crash: bool) {
    let _scenario = RECOVERY_SCENARIO.lock().await;
    let directory = tempfile::tempdir().unwrap();
    let a_dir = directory.path().join("old");
    let b_dir = directory.path().join("winning");
    let fixture = fixture();
    let old_expected = persist(&a_dir, &fixture.old);
    let mut expected = persist(&b_dir, &fixture.winning);
    let a = Daemon::start_with_activation(&a_dir, "initial.log", None, Some(2)).await;
    let pid = a.child.id().unwrap();
    assert_governance(&a, &old_expected).await;
    assert_eq!(
        old_expected.governance.proposals[&fixture.ids[1]].yes,
        64 * COIN
    );
    let baseline_rss = rss(pid);
    let peak = Arc::new(AtomicU64::new(baseline_rss));
    let samples = Arc::new(AtomicU64::new(0));
    let sampling = {
        let peak = Arc::clone(&peak);
        let samples = Arc::clone(&samples);
        tokio::spawn(async move {
            loop {
                peak.fetch_max(rss(pid), Ordering::Relaxed);
                samples.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    let b = Daemon::start_with_activation(&b_dir, "winning.log", Some(&a.p2p), Some(2)).await;
    a.wait_tip(&hex::encode(expected.best_hash().unwrap()))
        .await;
    assert_eq!(
        a.child.id(),
        Some(pid),
        "the original daemon must reorganize without restarting"
    );
    assert_governance(&a, &expected).await;
    let orphan = fixture.old.last().unwrap().transactions[1].txid();
    let response = a
        .client
        .get(format!(
            "{}/api/v1/blockchain/tx/{}",
            a.rpc,
            hex::encode(orphan)
        ))
        .send()
        .await
        .unwrap();
    if response.status().is_success() {
        let row: Value = response.json().await.unwrap();
        assert!(
            row["block_height"].is_null(),
            "orphan vote is still reported confirmed: {row}"
        );
    } else {
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    assert!(
        a.logs().contains("Reorg: fork point at height 2"),
        "no live reorg observed: {}",
        a.logs()
    );
    peak.fetch_max(rss(pid), Ordering::Relaxed);
    sampling.abort();
    assert!(
        sampling.await.unwrap_err().is_cancelled(),
        "RSS sampler failed before cancellation"
    );
    assert!(samples.load(Ordering::Relaxed) > 0);
    eprintln!(
        "governance_reorg {}",
        json!({ "restart": if crash { "sigkill" } else { "sigterm" },
        "voters": VOTERS, "old_height": 4, "new_height": 5, "fork_height": 2,
        "short_locks": 16, "long_locks": 16, "baseline_rss_kib": baseline_rss,
        "observed_reorg_peak_rss_kib": peak.load(Ordering::Relaxed), "rss_samples": samples.load(Ordering::Relaxed) })
    );
    b.stop(false).await;
    assert_eq!(a.mint(99).await["block_height"], 6);
    a.persisted(6).await;
    let tip = a.get("/api/v1/info").await["best_block_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    a.stop(crash).await;
    {
        let store = BlockStore::open(a_dir.join("chain.db")).unwrap();
        assert_eq!(store.best_height().unwrap(), 6);
        let descendant = store.get_block_at_height(6).unwrap().unwrap();
        expected.add_block(descendant).unwrap();
        assert_eq!(hex::encode(expected.best_hash().unwrap()), tip);
        for height in 1..=expected.best_height() {
            assert_eq!(
                store.get_block_at_height(height).unwrap().unwrap().hash(),
                expected.get_block_at_height(height).unwrap().hash()
            );
        }
        let mut actual: Vec<Utxo> = store.all_utxos().unwrap();
        actual.sort_by_key(|u| (u.txid, u.vout));
        let expected_utxos: Vec<_> = expected
            .get_utxo_set()
            .values()
            .filter(|u| u.height > 0)
            .cloned()
            .collect();
        assert_eq!(actual, expected_utxos);
        let loaded = store
            .load_into_regtest_chain_with_cache_and_governance(4096, 2)
            .unwrap();
        assert_eq!(loaded.governance, expected.governance);
        assert_eq!(loaded.get_utxo_set(), expected.get_utxo_set());
        assert_eq!(loaded.params(), expected.params());
    }
    let restarted = Daemon::start_with_activation(&a_dir, "restarted.log", None, Some(2)).await;
    restarted.wait_tip(&tip).await;
    assert_governance(&restarted, &expected).await;
    restarted.stop(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governance_reorg_survives_sigterm_restart() {
    run(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governance_reorg_survives_sigkill_restart() {
    run(true).await;
}
