use super::*;
use crate::{
    block::{TxInput, TxOutput, TxType},
    consensus::{compute_stake_modifier, COIN, MIN_STAKE_AGE},
    governance::{
        proposal_id_of, ParamId, Proposal, SignedVote, Vote, VoteChoice, MIN_ACTIVATION_DELAY,
        MIN_PROPOSAL_DEPOSIT, MIN_VOTING_BLOCKS,
    },
    staking::StakingEngine,
};
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

const FUNDING_TIME: u32 = 1_700_000_001;

fn key(byte: u8) -> SecretKey {
    SecretKey::from_slice(&[byte; 32]).unwrap()
}
fn address(byte: u8) -> String {
    let pk = PublicKey::from_secret_key(&Secp256k1::new(), &key(byte));
    vtorrent_core::address::Address::from_pubkey(&pk, true, 70).to_string()
}
fn output(byte: u8, value: u64) -> TxOutput {
    TxOutput {
        value,
        script_pubkey: vtorrent_core::address::Address::parse(&address(byte))
            .unwrap()
            .p2pkh_script_pubkey(),
    }
}
fn transaction(kind: TxType, outputs: Vec<TxOutput>, height: u32) -> Transaction {
    Transaction {
        version: 1,
        tx_type: kind,
        inputs: vec![],
        outputs,
        lock_time: height,
        claim_address: None,
        claim_signature: None,
    }
}
fn spend(u: &Utxo, byte: u8, outputs: Vec<TxOutput>) -> Transaction {
    let mut tx = transaction(TxType::Standard, outputs, 0);
    tx.inputs.push(TxInput {
        prev_txid: u.txid,
        prev_vout: u.vout,
        script_sig: vec![],
        sequence: u32::MAX,
    });
    sign_input(&mut tx, u, byte);
    tx
}

fn sign_input(tx: &mut Transaction, u: &Utxo, byte: u8) {
    let secp = Secp256k1::new();
    let mut sig = secp
        .sign_ecdsa(
            &Message::from_digest(tx.sighash(0, &u.script_pubkey)),
            &key(byte),
        )
        .serialize_der()
        .to_vec();
    sig.push(1);
    let mut script = vtorrent_script::Script::new();
    script.push_data(&sig).unwrap();
    script
        .push_data(&PublicKey::from_secret_key(&secp, &key(byte)).serialize())
        .unwrap();
    tx.inputs[0].script_sig = script.as_bytes().to_vec();
}
fn block(
    chain: &Chain,
    parent: [u8; 32],
    height: u32,
    timestamp: u32,
    nonce: u32,
    extra: Vec<Transaction>,
) -> Block {
    let mut coinbase = transaction(TxType::Coinbase, vec![output(3, 1)], height);
    coinbase.inputs.push(TxInput {
        prev_txid: [0; 32],
        prev_vout: u32::MAX,
        script_sig: vec![1],
        sequence: u32::MAX,
    });
    let mut transactions = vec![coinbase];
    transactions.extend(extra);
    let mut b = Block {
        header: BlockHeader {
            version: 1,
            prev_block_hash: parent,
            merkle_root: [0; 32],
            utxo_root: [0; 32],
            timestamp,
            bits: crate::genesis::GENESIS_BITS,
            nonce,
            stake_modifier: compute_stake_modifier(chain.headers[&parent].stake_modifier, &parent),
        },
        transactions,
    };
    b.header.merkle_root = b.compute_merkle_root();
    b
}
fn fixture(active: bool) -> (Chain, Utxo, [u8; 8], Vec<Block>) {
    let mut chain = Chain::new_regtest().unwrap();
    if active {
        chain.governance_test_activation = Some(2);
    }
    let mut funding = block(
        &chain,
        chain.best_hash().unwrap(),
        1,
        FUNDING_TIME,
        1,
        vec![],
    );
    funding.transactions[0].outputs = vec![output(1, 1000 * COIN), output(2, 1000 * COIN)];
    funding.header.merkle_root = funding.compute_merkle_root();
    chain.add_block(funding.clone()).unwrap();
    let owner = chain.get_utxos_for_address(&address(1))[0].clone();
    let payer = chain.get_utxos_for_address(&address(2))[0].clone();
    let proposal = Proposal {
        param: ParamId::PosAnnualRateBps,
        new_value: 300,
        voting_end: 2 + MIN_VOTING_BLOCKS,
        activation_height: 2 + MIN_VOTING_BLOCKS + MIN_ACTIVATION_DELAY,
        deposit: MIN_PROPOSAL_DEPOSIT,
    };
    let mut script = vtorrent_script::Script::new();
    script.push_opcode(0x6a);
    script.push_data(&proposal.encode()).unwrap();
    let proposal_tx = spend(
        &payer,
        2,
        vec![
            TxOutput {
                value: proposal.deposit,
                script_pubkey: script.as_bytes().to_vec(),
            },
            output(2, 899 * COIN),
        ],
    );
    let proposed = block(
        &chain,
        chain.best_hash().unwrap(),
        2,
        FUNDING_TIME + 1,
        2,
        vec![proposal_tx],
    );
    chain.add_block(proposed.clone()).unwrap();
    let id = proposal_id_of(&proposal, 2);
    (chain, owner, id, vec![funding, proposed])
}
fn vote_transaction(chain: &Chain, owner: &Utxo, id: [u8; 8]) -> Transaction {
    let vote = SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (owner.txid, owner.vout),
        chain.height_index[0],
        &key(1),
    );
    let payer = chain.get_utxos_for_address(&address(2))[0].clone();
    spend(
        &payer,
        2,
        vec![
            TxOutput {
                value: 1,
                script_pubkey: vote.script().unwrap(),
            },
            output(2, payer.value - COIN),
        ],
    )
}
fn voted_fixture() -> (Chain, Utxo, [u8; 8], Vec<Block>) {
    let (mut chain, owner, id, mut blocks) = fixture(true);
    let vote = vote_transaction(&chain, &owner, id);
    let voted = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        3,
        vec![vote],
    );
    chain.add_block(voted.clone()).unwrap();
    blocks.push(voted);
    (chain, owner, id, blocks)
}

#[test]
fn governance_is_inert_without_active_deployment() {
    let (mut chain, owner, id, _) = fixture(false);
    assert!(!chain.governance_active(3));
    let vote = vote_transaction(&chain, &owner, id);
    let b = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        3,
        vec![vote],
    );
    chain.add_block(b).unwrap();
    assert!(chain.governance.proposals.is_empty());
    assert!(chain.governance.locked_utxos.is_empty());
    assert_eq!(chain.params(), crate::consensus::ConsensusParams::default());
}

#[test]
fn locked_spend_rejection_is_atomic_and_replay_restores_state() {
    let (mut chain, owner, id, blocks) = voted_fixture();
    let before = chain.governance.clone();
    let utxos = chain.utxo_set.clone();
    let tx = spend(&owner, 1, vec![output(1, owner.value - COIN)]);
    let rejected = block(
        &chain,
        chain.best_hash().unwrap(),
        4,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 2,
        4,
        vec![tx],
    );
    assert!(chain
        .add_block(rejected)
        .unwrap_err()
        .to_string()
        .contains("locked"));
    assert_eq!(chain.governance, before);
    assert_eq!(chain.utxo_set, utxos);
    assert_eq!(chain.governance.proposals[&id].yes, owner.value);
    let mut replay = Chain::new_regtest().unwrap();
    replay.governance_test_activation = Some(2);
    for b in blocks {
        replay.add_block(b).unwrap();
    }
    assert_eq!(replay.governance, chain.governance);
    assert_eq!(replay.utxo_set, chain.utxo_set);
    assert_eq!(replay.params, chain.params);
}

#[test]
fn forged_identity_and_duplicate_vote_blocks_are_rejected_atomically() {
    let (mut chain, owner, id, _) = fixture(true);
    let before = chain.governance.clone();
    let before_utxos = chain.utxo_set.clone();
    let payer = chain.get_utxos_for_address(&address(2))[0].clone();
    let forged = SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (owner.txid, owner.vout),
        chain.height_index[0],
        &key(2),
    );
    let mut tx = spend(
        &payer,
        2,
        vec![
            TxOutput {
                value: 1,
                script_pubkey: forged.script().unwrap(),
            },
            output(2, payer.value - COIN),
        ],
    );
    tx.claim_address = Some(address(1));
    sign_input(&mut tx, &payer, 2);
    let b = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        60,
        vec![tx],
    );
    assert!(chain
        .add_block(b)
        .unwrap_err()
        .to_string()
        .contains("does not authorize"));
    assert_eq!(chain.governance, before);
    assert_eq!(chain.utxo_set, before_utxos);
    let vote = SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (owner.txid, owner.vout),
        chain.height_index[0],
        &key(1),
    );
    let carrier = TxOutput {
        value: 1,
        script_pubkey: vote.script().unwrap(),
    };
    let duplicate = spend(
        &payer,
        2,
        vec![carrier.clone(), carrier, output(2, payer.value - COIN)],
    );
    let b = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        61,
        vec![duplicate],
    );
    assert!(chain
        .add_block(b)
        .unwrap_err()
        .to_string()
        .contains("already counted"));
    assert_eq!(chain.governance, before);
    assert_eq!(chain.utxo_set, before_utxos);
}

#[test]
fn stake_amount_activation_recounts_denominator_and_rollback_restores_it() {
    let (mut chain, _, id, _) = fixture(true);
    let record = chain.governance.proposals.get_mut(&id).unwrap();
    record.proposal.param = ParamId::MinStakeAmount;
    record.proposal.new_value = 900 * COIN;
    record.proposal.activation_height = 3;
    record.decided = true;
    record.passed = true;
    let before = chain.governance.clone();
    let before_staked = chain.total_staked;
    let b = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 60,
        62,
        vec![],
    );
    chain.add_block(b).unwrap();
    assert_eq!(chain.params.min_stake_amount, 900 * COIN);
    assert_eq!(chain.total_staked, 1000 * COIN);
    chain.rollback_one_block().unwrap();
    assert_eq!(chain.params.min_stake_amount, COIN);
    assert_eq!(chain.total_staked, before_staked);
    assert_eq!(chain.governance, before);
}

#[test]
fn activation_cannot_remove_every_eligible_stake() {
    for (param, value) in [
        (ParamId::MinStakeAmount, 1200 * COIN),
        (ParamId::MaxStakeAge, MIN_STAKE_AGE),
        (ParamId::MinStakeAge, MIN_STAKE_AGE * 2),
    ] {
        let (mut chain, _, id, _) = fixture(true);
        let record = chain.governance.proposals.get_mut(&id).unwrap();
        record.proposal.param = param;
        record.proposal.new_value = value;
        record.proposal.activation_height = 3;
        record.decided = true;
        record.passed = true;
        let before = chain.params();
        let timestamp = FUNDING_TIME + MIN_STAKE_AGE as u32 + 60;
        assert_eq!(
            chain.next_block_params(timestamp),
            before,
            "producer must predict the same rejected activation as the validator"
        );
        let b = block(&chain, chain.best_hash().unwrap(), 3, timestamp, 70, vec![]);
        chain.add_block(b).unwrap();
        assert_eq!(
            chain.params(),
            before,
            "{param:?} must not eliminate all eligible stake"
        );
        let record = &chain.governance.proposals[&id];
        assert!(record.applied && record.activation_failed);
    }
}

#[test]
fn successful_reorg_keeps_common_proposal_and_removes_abandoned_vote() {
    let (mut chain, owner, id, blocks) = voted_fixture();
    let parent = blocks[1].hash();
    let fork3 = block(
        &chain,
        parent,
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        30,
        vec![],
    );
    let fork_hash = fork3.hash();
    chain.add_block(fork3).unwrap();
    let fork4 = block(
        &chain,
        fork_hash,
        4,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 2,
        40,
        vec![],
    );
    assert!(matches!(
        chain.add_block(fork4).unwrap(),
        BlockAcceptance::Reorg { .. }
    ));
    assert_eq!(chain.governance.proposals[&id].yes, 0);
    assert!(!chain.is_vote_locked(&owner.txid, owner.vout, 4));
    assert!(chain.utxo_set.contains_key(&(owner.txid, owner.vout)));
}

#[test]
fn failed_reorg_restores_params_governance_and_utxos() {
    let (mut chain, owner, id, blocks) = fixture(true);
    // Place the already-counted proposal at an activation boundary to isolate rollback.
    let record = chain.governance.proposals.get_mut(&id).unwrap();
    record.proposal.voting_end = 4;
    record.proposal.activation_height = 4;
    let vote = vote_transaction(&chain, &owner, id);
    let voted = block(
        &chain,
        chain.best_hash().unwrap(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        3,
        vec![vote],
    );
    chain.add_block(voted).unwrap();
    let before_gov = chain.governance.clone();
    let before_params = chain.params;
    let before_utxos = chain.utxo_set.clone();
    let before_supply = chain.total_supply;
    let before_staked = chain.total_staked;
    let vote = vote_transaction_at_fork(&chain, &owner, id, &blocks[1]);
    let fork3 = block(
        &chain,
        blocks[1].hash(),
        3,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 1,
        31,
        vec![vote],
    );
    let fork_hash = fork3.hash();
    chain.add_block(fork3).unwrap();
    let mut bad_tx = transaction(TxType::Standard, vec![output(1, 1)], 0);
    bad_tx.inputs.push(TxInput {
        prev_txid: [254; 32],
        prev_vout: 0,
        script_sig: vec![],
        sequence: u32::MAX,
    });
    let fork4 = block(
        &chain,
        fork_hash,
        4,
        FUNDING_TIME + MIN_STAKE_AGE as u32 + 2,
        41,
        vec![bad_tx],
    );
    assert!(chain.add_block(fork4).is_err());
    assert_eq!(chain.params, before_params);
    assert_eq!(chain.governance, before_gov);
    assert_eq!(chain.utxo_set, before_utxos);
    assert_eq!(chain.total_supply, before_supply);
    assert_eq!(chain.total_staked, before_staked);
}

fn vote_transaction_at_fork(
    chain: &Chain,
    owner: &Utxo,
    id: [u8; 8],
    parent: &Block,
) -> Transaction {
    let tx = &parent.transactions[1];
    let payer = Utxo {
        txid: tx.txid(),
        vout: 1,
        value: tx.outputs[1].value,
        script_pubkey: tx.outputs[1].script_pubkey.clone(),
        height: 2,
        timestamp: parent.header.timestamp,
    };
    let vote = SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (owner.txid, owner.vout),
        chain.height_index[0],
        &key(1),
    );
    spend(
        &payer,
        2,
        vec![
            TxOutput {
                value: 1,
                script_pubkey: vote.script().unwrap(),
            },
            output(2, payer.value - COIN),
        ],
    )
}

#[test]
fn activation_and_stake_lock_roll_back_on_bad_root_then_accept_valid_stake() {
    let (mut chain, owner, id, _) = voted_fixture();
    let record = chain.governance.proposals.get_mut(&id).unwrap();
    record.proposal.voting_end = 4;
    record.proposal.activation_height = 4;
    let mut engine = StakingEngine::with_wif(
        address(1),
        vtorrent_core::keys::PrivateKey::from_bytes([1; 32], true)
            .unwrap()
            .to_wif(198),
    );
    engine.set_consensus_params(chain.next_block_params(FUNDING_TIME + MIN_STAKE_AGE as u32 + 10));
    let parent = chain.best_hash().unwrap();
    let modifier = chain.headers[&parent].stake_modifier;
    let candidate = (FUNDING_TIME + MIN_STAKE_AGE as u32 + 10..)
        .take(1000)
        .find_map(|timestamp| {
            engine.build_stake_block_with_proof(
                parent,
                modifier,
                4,
                timestamp,
                chain.total_staked,
                chain.utxo_set.values().cloned().collect(),
                vec![],
            )
        })
        .unwrap()
        .0;
    let before = chain.governance.clone();
    let before_utxos = chain.utxo_set.clone();
    let before_staked = chain.total_staked;
    let mut bad = candidate.clone();
    bad.header.utxo_root = [0; 32];
    assert!(chain
        .add_block(bad)
        .unwrap_err()
        .to_string()
        .contains("UTXO root"));
    assert_eq!(chain.params.pos_annual_rate_bps, 500);
    assert_eq!(chain.governance, before);
    assert_eq!(chain.utxo_set, before_utxos);
    assert_eq!(chain.total_staked, before_staked);
    let mut bad_sig = candidate.clone();
    bad_sig.transactions[0].inputs[0].script_sig.clear();
    bad_sig.header.merkle_root = bad_sig.compute_merkle_root();
    assert!(chain.add_block(bad_sig).is_err());
    assert_eq!(
        chain.utxo_set, before_utxos,
        "script failure must restore the removed input"
    );
    let successor = candidate.transactions[0].txid();
    chain.add_block(candidate).unwrap();
    assert_eq!(chain.params.pos_annual_rate_bps, 300);
    assert!(!chain.is_vote_locked(&owner.txid, owner.vout, 4));
    assert!(chain.is_vote_locked(&successor, 1, 4));
    chain.rollback_one_block().unwrap();
    assert_eq!(chain.params.pos_annual_rate_bps, 500);
    assert_eq!(chain.governance, before);
    assert_eq!(chain.utxo_set, before_utxos);
}
