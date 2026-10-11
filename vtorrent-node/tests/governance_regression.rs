use secp256k1::{PublicKey, Secp256k1, SecretKey};
use std::collections::BTreeMap;
use vtorrent_node::{
    block::{Transaction, TxInput, TxOutput, TxType},
    chain::Utxo,
    consensus::{ConsensusParams, COIN, MIN_STAKE_AGE},
    governance::{
        proposal_id_of, GovernanceState, ParamId, Proposal, SignedVote, Vote, VoteChoice,
        DEFAULT_LOCK_GRACE_BLOCKS, MIN_ACTIVATION_DELAY, MIN_PROPOSAL_DEPOSIT, MIN_VOTING_BLOCKS,
    },
};

const GENESIS: [u8; 32] = [9; 32];
const NOW: u32 = 1_700_000_000;

fn key(byte: u8) -> SecretKey {
    SecretKey::from_slice(&[byte; 32]).unwrap()
}
fn pk_hash(byte: u8) -> [u8; 20] {
    vtorrent_core::crypto::hash160(
        &PublicKey::from_secret_key(&Secp256k1::new(), &key(byte)).serialize(),
    )
}
fn utxo(byte: u8) -> Utxo {
    Utxo {
        txid: [byte; 32],
        vout: 0,
        value: 100 * COIN,
        height: 1,
        timestamp: NOW - MIN_STAKE_AGE as u32,
        script_pubkey: vtorrent_script::standard::build_p2pkh(&pk_hash(byte))
            .unwrap()
            .as_bytes()
            .to_vec(),
    }
}
fn carrier(data: &[u8]) -> Vec<u8> {
    let mut script = vtorrent_script::Script::new();
    script.push_opcode(0x6a);
    script.push_data(data).unwrap();
    script.as_bytes().to_vec()
}
fn transaction(script: Vec<u8>, value: u64) -> Transaction {
    Transaction {
        version: 1,
        tx_type: TxType::Standard,
        inputs: vec![],
        outputs: vec![TxOutput {
            script_pubkey: script,
            value,
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    }
}
fn proposal(end: u32, value: u64) -> Proposal {
    Proposal {
        param: ParamId::PosAnnualRateBps,
        new_value: value,
        voting_end: end,
        activation_height: end + MIN_ACTIVATION_DELAY,
        deposit: MIN_PROPOSAL_DEPOSIT,
    }
}
fn propose(g: &mut GovernanceState, p: &Proposal) -> [u8; 8] {
    g.process_transaction(
        &transaction(carrier(&p.encode()), p.deposit),
        1,
        NOW,
        &BTreeMap::new(),
        ConsensusParams::default(),
        100 * COIN,
        GENESIS,
    )
    .unwrap();
    proposal_id_of(p, 1)
}
fn signed(id: [u8; 8], u: &Utxo, byte: u8) -> SignedVote {
    SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (u.txid, u.vout),
        GENESIS,
        &key(byte),
    )
}
fn cast(
    g: &mut GovernanceState,
    vote: &SignedVote,
    u: &Utxo,
    timestamp: u32,
) -> vtorrent_node::error::Result<()> {
    g.process_transaction(
        &transaction(vote.script().unwrap(), 1),
        2,
        timestamp,
        &BTreeMap::from([((u.txid, u.vout), u.clone())]),
        ConsensusParams::default(),
        100 * COIN,
        GENESIS,
    )
}

#[test]
fn duplicate_vote_cannot_inflate_weight() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let u = utxo(2);
    let vote = signed(id, &u, 2);
    cast(&mut g, &vote, &u, NOW).unwrap();
    assert!(cast(&mut g, &vote, &u, NOW)
        .unwrap_err()
        .to_string()
        .contains("already counted"));
    assert_eq!(g.proposals[&id].yes, u.value);
}

#[test]
fn overlapping_vote_keeps_longest_lock() {
    let mut g = GovernanceState::new();
    let short = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let long = propose(&mut g, &proposal(100 + MIN_VOTING_BLOCKS, 400));
    let u = utxo(3);
    cast(&mut g, &signed(short, &u, 3), &u, NOW).unwrap();
    cast(&mut g, &signed(long, &u, 3), &u, NOW).unwrap();
    assert!(g.is_utxo_locked(
        &u.txid,
        0,
        1 + MIN_VOTING_BLOCKS + DEFAULT_LOCK_GRACE_BLOCKS
    ));
    assert!(!g.is_utxo_locked(
        &u.txid,
        0,
        100 + MIN_VOTING_BLOCKS + DEFAULT_LOCK_GRACE_BLOCKS
    ));
}

#[test]
fn activation_is_deterministic_and_applied_once() {
    let end = 1 + MIN_VOTING_BLOCKS;
    let p = proposal(end, 300);
    let q = proposal(end, 400);
    let mut results = Vec::new();
    for proposals in [[p.clone(), q.clone()], [q.clone(), p.clone()]] {
        let mut g = GovernanceState::new();
        let u = utxo(4);
        for prop in proposals {
            let id = propose(&mut g, &prop);
            cast(&mut g, &signed(id, &u, 4), &u, NOW).unwrap();
        }
        let mut params = ConsensusParams::default();
        g.begin_block(end, &mut params);
        assert_eq!(params.pos_annual_rate_bps, 500);
        g.begin_block(p.activation_height, &mut params);
        results.push(params.pos_annual_rate_bps);
        assert!(g
            .proposals
            .values()
            .all(|r| r.applied && !r.activation_failed));
        params.pos_annual_rate_bps = 200;
        g.begin_block(p.activation_height + 1, &mut params);
        assert_eq!(
            params.pos_annual_rate_bps, 200,
            "old activations must not overwrite later values"
        );
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn forged_identity_unsigned_vote_and_network_replay_are_rejected() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let u = utxo(5);
    let mut forged = transaction(signed(id, &u, 6).script().unwrap(), 1);
    forged.claim_address = Some("victim".into());
    let utxos = BTreeMap::from([((u.txid, u.vout), u.clone())]);
    assert!(g
        .process_transaction(
            &forged,
            2,
            NOW,
            &utxos,
            ConsensusParams::default(),
            u.value,
            GENESIS
        )
        .unwrap_err()
        .to_string()
        .contains("does not authorize"));
    let unsigned = transaction(
        carrier(
            &Vote {
                proposal_id: id,
                choice: VoteChoice::Yes,
            }
            .encode(),
        ),
        1,
    );
    assert!(g
        .process_transaction(
            &unsigned,
            2,
            NOW,
            &utxos,
            ConsensusParams::default(),
            u.value,
            GENESIS
        )
        .is_err());
    let replay = SignedVote::sign(
        Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        },
        (u.txid, u.vout),
        [8; 32],
        &key(5),
    );
    assert!(cast(&mut g, &replay, &u, NOW).is_err());
    assert_eq!(g.proposals[&id].yes, 0);
    assert!(g.locked_utxos.is_empty());
}

#[test]
fn maturity_is_seconds_and_dust_does_not_block_the_selected_coin() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let u = utxo(7);
    let vote = signed(id, &u, 7);
    assert!(cast(&mut g, &vote, &u, NOW - 1).is_err());
    let mut dust = utxo(8);
    dust.value = 1;
    dust.timestamp = NOW;
    let utxos = BTreeMap::from([((u.txid, u.vout), u.clone()), ((dust.txid, 0), dust)]);
    g.process_transaction(
        &transaction(vote.script().unwrap(), 1),
        2,
        NOW,
        &utxos,
        ConsensusParams::default(),
        u.value,
        GENESIS,
    )
    .unwrap();
    assert_eq!(g.proposals[&id].yes, u.value);
}

#[test]
fn cold_stake_owner_votes_not_operator() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let mut u = utxo(9);
    u.script_pubkey = vtorrent_script::standard::build_p2cs(&pk_hash(10), &pk_hash(9), 0)
        .unwrap()
        .as_bytes()
        .to_vec();
    assert!(cast(&mut g, &signed(id, &u, 10), &u, NOW).is_err());
    cast(&mut g, &signed(id, &u, 9), &u, NOW).unwrap();
}

#[test]
fn legacy_uncompressed_owner_can_authorize_a_vote() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let mut u = utxo(15);
    let pk = PublicKey::from_secret_key(&Secp256k1::new(), &key(15));
    let hash = vtorrent_core::crypto::hash160(&pk.serialize_uncompressed());
    u.script_pubkey = vtorrent_script::standard::build_p2pkh(&hash)
        .unwrap()
        .as_bytes()
        .to_vec();
    cast(&mut g, &signed(id, &u, 15), &u, NOW).unwrap();
    assert_eq!(g.proposals[&id].yes, u.value);
}

#[test]
fn staking_carries_locks_and_cannot_revote_or_redirect() {
    let mut g = GovernanceState::new();
    let id = propose(&mut g, &proposal(1 + MIN_VOTING_BLOCKS, 300));
    let u = utxo(11);
    cast(&mut g, &signed(id, &u, 11), &u, NOW).unwrap();
    let mut stake = transaction(u.script_pubkey.clone(), u.value + 100);
    stake.tx_type = TxType::Coinstake;
    stake.inputs.push(TxInput {
        prev_txid: u.txid,
        prev_vout: 0,
        script_sig: vec![],
        sequence: u32::MAX,
    });
    let mut redirect = stake.clone();
    redirect.outputs[0].script_pubkey = utxo(12).script_pubkey;
    assert!(g.transfer_stake_lock(&redirect, &u, 2).is_err());
    g.transfer_stake_lock(&stake, &u, 2).unwrap();
    assert!(!g.is_utxo_locked(&u.txid, 0, 2));
    assert!(g.is_utxo_locked(&stake.txid(), 0, 2));
    let mut successor = u.clone();
    successor.txid = stake.txid();
    successor.value += 100;
    assert!(cast(&mut g, &signed(id, &successor, 11), &successor, NOW)
        .unwrap_err()
        .to_string()
        .contains("already counted"));
    assert_eq!(g.proposals[&id].yes, u.value);
}

#[test]
fn invalid_proposals_and_parameter_conflicts_do_not_mutate_params() {
    let mut params = ConsensusParams::default();
    let before = params;
    for (param, value) in [
        (ParamId::PosAnnualRateBps, u64::MAX),
        (ParamId::TargetBlockTime, 0),
        (ParamId::MinStakeAge, before.max_stake_age + 1),
    ] {
        assert!(param.apply(&mut params, value).is_err());
        assert_eq!(params, before);
    }
    let mut g = GovernanceState::new();
    let p = proposal(1 + MIN_VOTING_BLOCKS, 300);
    let mut bad = p.clone();
    bad.activation_height = bad.voting_end;
    assert!(g
        .process_transaction(
            &transaction(carrier(&bad.encode()), bad.deposit),
            1,
            NOW,
            &BTreeMap::new(),
            params,
            100 * COIN,
            GENESIS
        )
        .is_err());
    assert!(g
        .process_transaction(
            &transaction(carrier(&p.encode()), 1),
            1,
            NOW,
            &BTreeMap::new(),
            params,
            100 * COIN,
            GENESIS
        )
        .is_err());
    assert!(g.proposals.is_empty());
    let mut far_future = p.clone();
    far_future.activation_height = u32::MAX;
    assert!(g
        .process_transaction(
            &transaction(carrier(&far_future.encode()), far_future.deposit),
            1,
            NOW,
            &BTreeMap::new(),
            params,
            100 * COIN,
            GENESIS,
        )
        .is_err());
}

#[test]
fn closed_window_rejects_votes_and_quorum_is_fixed_at_creation() {
    let mut g = GovernanceState::new();
    let p = proposal(1 + MIN_VOTING_BLOCKS, 300);
    let id = propose(&mut g, &p);
    let u = utxo(13);
    let vote = signed(id, &u, 13);
    assert!(g
        .process_transaction(
            &transaction(vote.script().unwrap(), 1),
            p.voting_end,
            NOW,
            &BTreeMap::from([((u.txid, u.vout), u.clone())]),
            ConsensusParams::default(),
            u.value,
            GENESIS
        )
        .is_err());
    cast(&mut g, &vote, &u, NOW).unwrap();
    let mut params = ConsensusParams::default();
    g.begin_block(p.activation_height, &mut params);
    assert_eq!(params.pos_annual_rate_bps, 300);
    assert_eq!(g.proposals[&id].quorum_supply, 100 * COIN);
}

#[test]
fn codecs_and_quorum_threshold_boundaries() {
    use vtorrent_node::governance::{tally, ProposalOutcome};
    let p = proposal(1 + MIN_VOTING_BLOCKS, 300);
    assert_eq!(Proposal::decode(&p.encode()), Some(p.clone()));
    assert!(Proposal::decode(b"VTRG1").is_none());
    let u = utxo(14);
    let vote = signed([1; 8], &u, 14);
    assert_eq!(SignedVote::decode(&vote.encode()), Some(vote));
    assert!(SignedVote::decode(b"VTRV2").is_none());
    assert_eq!(
        tally(&p, 32, 0, 0, 100, p.voting_end),
        ProposalOutcome::Failed
    );
    assert!(matches!(
        tally(&p, 33, 0, 0, 100, p.voting_end),
        ProposalOutcome::Passed { .. }
    ));
    assert_eq!(
        tally(&p, 65, 35, 0, 100, p.voting_end),
        ProposalOutcome::Failed
    );
    assert!(matches!(
        tally(&p, 66, 34, 0, 100, p.voting_end),
        ProposalOutcome::Passed { .. }
    ));
    assert!(matches!(
        tally(&p, 20, 5, 15, 100, p.voting_end),
        ProposalOutcome::Passed { .. }
    ));
    assert_eq!(
        tally(&p, 100, 0, 0, 100, p.voting_end - 1),
        ProposalOutcome::Open
    );
}
