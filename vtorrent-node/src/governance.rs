//! On-chain governance: proposals, stake-weighted votes, and vote-locking
//! (Steps 1–3).
//!
//! Proposals and votes are OP_RETURN data carriers with a domain prefix, so
//! they are permanent, censorship-resistant, and visible to every node.
//! Voting power is counted from coins **locked at vote time** with a minimum
//! stake age (Option B from `docs/governance-step3-design.md`).
//!
//! This module is the **pure core**: encoding/decoding the OP_RETURN payloads,
//! tallying votes, and managing vote-locks. Chain integration (scanning,
//! lock enforcement) builds on it.

use crate::consensus::ConsensusParams;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Proposal OP_RETURN prefix: `VTRG1`.
pub const PROPOSAL_PREFIX: &[u8; 5] = b"VTRG1";
/// Vote OP_RETURN prefix: `VTRV1`.
pub const VOTE_PREFIX: &[u8; 5] = b"VTRV1";

/// Default lock grace period (blocks) added to a proposal's `voting_end` to
/// produce the unlock height. Prevents vote-then-spend in the same block.
pub const DEFAULT_LOCK_GRACE_BLOCKS: u32 = 6;

/// The governance-changeable parameters (a stable id per parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamId {
    PosAnnualRateBps = 0,
    RewardAgeCap = 1,
    MinStakeAge = 2,
    MaxStakeAge = 3,
    MinStakeAmount = 4,
    TargetBlockTime = 5,
}

impl ParamId {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::PosAnnualRateBps,
            1 => Self::RewardAgeCap,
            2 => Self::MinStakeAge,
            3 => Self::MaxStakeAge,
            4 => Self::MinStakeAmount,
            5 => Self::TargetBlockTime,
            _ => return None,
        })
    }

    /// Apply this parameter's new value to a `ConsensusParams`.
    pub fn apply(self, params: &mut ConsensusParams, value: u64) {
        match self {
            Self::PosAnnualRateBps => params.pos_annual_rate_bps = value as u32,
            Self::RewardAgeCap => params.reward_age_cap = value,
            Self::MinStakeAge => params.min_stake_age = value,
            Self::MaxStakeAge => params.max_stake_age = value,
            Self::MinStakeAmount => params.min_stake_amount = value,
            Self::TargetBlockTime => params.target_block_time = value,
        }
    }
}

/// A governance proposal (encoded in an OP_RETURN).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub param: ParamId,
    pub new_value: u64,
    /// Height at which the new value takes effect if passed.
    pub activation_height: u32,
    /// Height at which voting closes.
    pub voting_end: u32,
    /// Deposit (satoshis) — burned on fail, refunded on pass.
    pub deposit: u64,
}

impl Proposal {
    /// Encode to OP_RETURN data bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(5 + 1 + 8 + 4 + 4 + 8);
        b.extend_from_slice(PROPOSAL_PREFIX);
        b.push(self.param as u8);
        b.extend_from_slice(&self.new_value.to_le_bytes());
        b.extend_from_slice(&self.activation_height.to_le_bytes());
        b.extend_from_slice(&self.voting_end.to_le_bytes());
        b.extend_from_slice(&self.deposit.to_le_bytes());
        b
    }

    /// Decode from OP_RETURN data bytes, or `None` if not a valid proposal.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != 5 + 1 + 8 + 4 + 4 + 8 || &data[..5] != PROPOSAL_PREFIX {
            return None;
        }
        let param = ParamId::from_u8(data[5])?;
        let new_value = u64::from_le_bytes(data[6..14].try_into().ok()?);
        let activation_height = u32::from_le_bytes(data[14..18].try_into().ok()?);
        let voting_end = u32::from_le_bytes(data[18..22].try_into().ok()?);
        let deposit = u64::from_le_bytes(data[22..30].try_into().ok()?);
        Some(Self {
            param,
            new_value,
            activation_height,
            voting_end,
            deposit,
        })
    }
}

/// A vote on a proposal (encoded in an OP_RETURN).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    /// Proposal id (the txid of the proposal, first 8 bytes).
    pub proposal_id: [u8; 8],
    pub choice: VoteChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteChoice {
    Yes = 1,
    No = 2,
    Abstain = 3,
}

impl Vote {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(5 + 8 + 1);
        b.extend_from_slice(VOTE_PREFIX);
        b.extend_from_slice(&self.proposal_id);
        b.push(self.choice as u8);
        b
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != 5 + 8 + 1 || &data[..5] != VOTE_PREFIX {
            return None;
        }
        let mut proposal_id = [0u8; 8];
        proposal_id.copy_from_slice(&data[5..13]);
        let choice = match data[13] {
            1 => VoteChoice::Yes,
            2 => VoteChoice::No,
            3 => VoteChoice::Abstain,
            _ => return None,
        };
        Some(Self {
            proposal_id,
            choice,
        })
    }
}

/// Default quorum (percent of staked supply that must vote).
pub const DEFAULT_QUORUM_PCT: u64 = 33;
/// Default pass threshold (percent of non-abstain votes that must be Yes).
pub const DEFAULT_THRESHOLD_PCT: u64 = 66;

/// The outcome of tallying a proposal's votes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalOutcome {
    /// Voting is still open.
    Open,
    /// Quorum not met, or below threshold.
    Failed,
    /// Passed: apply `param = new_value` at `activation_height`.
    Passed {
        param: ParamId,
        new_value: u64,
        activation_height: u32,
    },
}

/// Tally stake-weighted votes.
///
/// `yes`/`no`/`abstain` are the summed stake (satoshis) of the respective
/// votes; `total_staked` is the staked supply at the proposal's snapshot.
pub fn tally(
    proposal: &Proposal,
    yes: u64,
    no: u64,
    abstain: u64,
    total_staked: u64,
    tip_height: u32,
) -> ProposalOutcome {
    if tip_height < proposal.voting_end {
        return ProposalOutcome::Open;
    }
    let turnout = yes.saturating_add(no).saturating_add(abstain);
    // Quorum: turnout must reach the threshold of the staked supply.
    if total_staked == 0
        || turnout.saturating_mul(100) < total_staked.saturating_mul(DEFAULT_QUORUM_PCT)
    {
        return ProposalOutcome::Failed;
    }
    // Threshold: of the decisive (non-abstain) votes, Yes must reach the bar.
    let decisive = yes.saturating_add(no);
    if decisive == 0 || yes.saturating_mul(100) < decisive.saturating_mul(DEFAULT_THRESHOLD_PCT) {
        return ProposalOutcome::Failed;
    }
    ProposalOutcome::Passed {
        param: proposal.param,
        new_value: proposal.new_value,
        activation_height: proposal.activation_height,
    }
}

/// A recorded proposal with its running vote tally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub proposal: Proposal,
    /// Height at which the proposal was created (snapshot point).
    pub created_height: u32,
    /// Summed stake (satoshis) voting Yes/No/Abstain.
    pub yes: u64,
    pub no: u64,
    pub abstain: u64,
    /// Whether the proposal's outcome has been decided.
    pub decided: bool,
    /// Whether the decided outcome was `Passed`.
    pub passed: bool,
}

/// A UTXO locked by a vote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteLock {
    /// The txid:vout of the locked UTXO.
    pub txid: [u8; 32],
    pub vout: u32,
    /// Block height at which the lock expires (proposal voting_end + grace).
    pub unlock_height: u32,
    /// The voter address (for display/query).
    pub voter: String,
    /// The proposal this lock is associated with.
    pub proposal_id: [u8; 8],
}

/// Deterministic governance state, rebuilt from the chain.
///
/// `process_block` scans a block's OP_RETURN outputs for proposals and votes,
/// locks the voter's UTXOs (with min-stake-age validation), and applies any
/// passed parameter at its activation height. Because it is a pure function of
/// the block sequence, it is **reorg-safe by rebuild**: on a reorg, replay the
/// new chain from the fork point.
#[derive(Debug, Clone, Default)]
pub struct GovernanceState {
    pub proposals: HashMap<[u8; 8], ProposalRecord>,
    pub params: ConsensusParams,
    /// Total staked supply used for quorum (set by the caller per block).
    pub total_staked: u64,
    /// UTXOs locked by votes, keyed by (txid, vout).
    pub locked_utxos: HashMap<([u8; 32], u32), VoteLock>,
    /// Minimum stake age (blocks/seconds) for a UTXO to be vote-eligible.
    /// Set by the caller (should match ConsensusParams.min_stake_age).
    pub min_stake_age: u64,
}

impl GovernanceState {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            params: ConsensusParams::default(),
            total_staked: 0,
            locked_utxos: HashMap::new(),
            min_stake_age: crate::consensus::MIN_STAKE_AGE,
        }
    }

    /// Returns `true` if the given UTXO is currently locked at `height`.
    pub fn is_utxo_locked(&self, txid: &[u8; 32], vout: u32, height: u32) -> bool {
        if let Some(lock) = self.locked_utxos.get(&(*txid, vout)) {
            return height < lock.unlock_height;
        }
        false
    }

    /// Remove all locks that have expired at or before `height`.
    pub fn prune_expired_locks(&mut self, height: u32) {
        self.locked_utxos
            .retain(|_, lock| height < lock.unlock_height);
    }

    /// Extract the OP_RETURN data payload from a scriptPubKey, if any.
    fn op_return_data(script: &[u8]) -> Option<&[u8]> {
        // OP_RETURN <pushdata>
        if script.first() != Some(&0x6a) || script.len() < 2 {
            return None;
        }
        let len = script[1] as usize;
        if script.len() < 2 + len {
            return None;
        }
        Some(&script[2..2 + len])
    }

    /// Process one block's governance outputs at `height`, then apply any
    /// parameter whose activation height is reached.
    ///
    /// `stake_utxos_of` maps a voter address to its stakeable UTXOs:
    /// `Vec<(txid, vout, value_sats, creation_height)>`. The caller supplies
    /// this from the chain's UTXO set.
    ///
    /// When a vote is processed:
    /// 1. Each UTXO must be ≥ `min_stake_age` blocks old (anti-flash-stake).
    /// 2. Each UTXO is locked until `proposal.voting_end + grace_blocks`.
    /// 3. If any UTXO fails the age check, the vote is rejected.
    pub fn process_block<US>(
        &mut self,
        height: u32,
        outputs: &[(Vec<u8>, String)],
        stake_utxos_of: US,
    ) where
        US: Fn(&str) -> Vec<([u8; 32], u32, u64, u32)>,
    {
        let grace = DEFAULT_LOCK_GRACE_BLOCKS;
        for (script, voter) in outputs {
            let Some(data) = Self::op_return_data(script) else {
                continue;
            };
            if let Some(p) = Proposal::decode(data) {
                let id = proposal_id_of(&p, height);
                self.proposals.entry(id).or_insert(ProposalRecord {
                    proposal: p,
                    created_height: height,
                    yes: 0,
                    no: 0,
                    abstain: 0,
                    decided: false,
                    passed: false,
                });
            } else if let Some(v) = Vote::decode(data) {
                if let Some(rec) = self.proposals.get(&v.proposal_id) {
                    if height > rec.proposal.voting_end {
                        continue;
                    }
                    let unlock_height = rec.proposal.voting_end.saturating_add(grace);
                    let utxos = stake_utxos_of(voter);
                    // Anti-flash-stake: all stakeable UTXOs must be ≥ min_stake_age.
                    let too_young = utxos.iter().any(|(_, _, _, created)| {
                        let age = height.saturating_sub(*created);
                        (age as u64) < self.min_stake_age
                    });
                    if too_young {
                        continue; // vote silently rejected
                    }
                    let stake: u64 = utxos.iter().map(|(_, _, val, _)| val).sum();
                    // Lock each UTXO.
                    for (txid, vout, _, _) in &utxos {
                        self.locked_utxos.entry((*txid, *vout)).or_insert(VoteLock {
                            txid: *txid,
                            vout: *vout,
                            unlock_height,
                            voter: voter.clone(),
                            proposal_id: v.proposal_id,
                        });
                    }
                    if let Some(rec) = self.proposals.get_mut(&v.proposal_id) {
                        match v.choice {
                            VoteChoice::Yes => rec.yes = rec.yes.saturating_add(stake),
                            VoteChoice::No => rec.no = rec.no.saturating_add(stake),
                            VoteChoice::Abstain => rec.abstain = rec.abstain.saturating_add(stake),
                        }
                    }
                }
            }
        }
        self.prune_expired_locks(height);
        self.apply_activations(height);
    }

    /// Decide closed proposals and apply passed parameters at activation.
    ///
    /// A passed proposal's parameter is applied on the first block at or after
    /// its `activation_height` — whether it was decided in this call or an
    /// earlier one — so a proposal that decided at height 10 still activates at
    /// height 20.
    fn apply_activations(&mut self, height: u32) {
        let total_staked = self.total_staked;
        let mut to_apply: Vec<(ParamId, u64)> = Vec::new();
        for rec in self.proposals.values_mut() {
            if !rec.decided {
                match tally(
                    &rec.proposal,
                    rec.yes,
                    rec.no,
                    rec.abstain,
                    total_staked,
                    height,
                ) {
                    ProposalOutcome::Open => continue,
                    ProposalOutcome::Failed => {
                        rec.decided = true;
                        rec.passed = false;
                        continue;
                    }
                    ProposalOutcome::Passed { .. } => {
                        rec.decided = true;
                        rec.passed = true;
                    }
                }
            }
            // Decided and passed: apply at/after the activation height.
            if rec.passed && height >= rec.proposal.activation_height {
                to_apply.push((rec.proposal.param, rec.proposal.new_value));
            }
        }
        for (param, value) in to_apply {
            param.apply(&mut self.params, value);
        }
    }
}

/// Derive a stable proposal id from its encoded bytes (first 8 bytes of a
/// hash of the encoding + creation height).
fn proposal_id_of(p: &Proposal, height: u32) -> [u8; 8] {
    let mut buf = p.encode();
    buf.extend_from_slice(&height.to_le_bytes());
    let h = vtorrent_core::crypto::sha256(&buf);
    let mut id = [0u8; 8];
    id.copy_from_slice(&h[..8]);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal() -> Proposal {
        Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 1000,
            voting_end: 500,
            deposit: 100_000_000,
        }
    }

    #[test]
    fn proposal_roundtrip() {
        let p = proposal();
        assert_eq!(Proposal::decode(&p.encode()), Some(p));
    }

    #[test]
    fn vote_roundtrip() {
        let v = Vote {
            proposal_id: [7u8; 8],
            choice: VoteChoice::Yes,
        };
        assert_eq!(Vote::decode(&v.encode()), Some(v));
    }

    #[test]
    fn decode_rejects_wrong_prefix_and_length() {
        assert!(Proposal::decode(b"XXXXX").is_none());
        assert!(Proposal::decode(&[0u8; 10]).is_none());
        assert!(Vote::decode(b"VTRG1").is_none()); // proposal prefix, not vote
    }

    #[test]
    fn tally_open_before_end() {
        let p = proposal();
        assert_eq!(tally(&p, 1, 0, 0, 100, 499), ProposalOutcome::Open);
    }

    #[test]
    fn tally_passes_with_quorum_and_threshold() {
        let p = proposal();
        // 100 staked; 40 vote (40% ≥ 33% quorum), 30 yes / 10 no (75% ≥ 66%).
        assert_eq!(
            tally(&p, 30, 10, 0, 100, 500),
            ProposalOutcome::Passed {
                param: ParamId::PosAnnualRateBps,
                new_value: 400,
                activation_height: 1000,
            }
        );
    }

    #[test]
    fn tally_fails_without_quorum() {
        let p = proposal();
        // Only 20% turnout < 33%.
        assert_eq!(tally(&p, 15, 5, 0, 100, 500), ProposalOutcome::Failed);
    }

    #[test]
    fn tally_fails_below_threshold() {
        let p = proposal();
        // 50% turnout, but 50/50 yes/no < 66%.
        assert_eq!(tally(&p, 25, 25, 0, 100, 500), ProposalOutcome::Failed);
    }

    #[test]
    fn tally_abstain_counts_for_quorum_not_threshold() {
        let p = proposal();
        // 40% turnout (20 yes, 5 no, 15 abstain); decisive = 25, yes 20 = 80% ≥ 66%.
        assert!(matches!(
            tally(&p, 20, 5, 15, 100, 500),
            ProposalOutcome::Passed { .. }
        ));
    }

    #[test]
    fn param_apply() {
        let mut params = ConsensusParams::default();
        ParamId::PosAnnualRateBps.apply(&mut params, 400);
        assert_eq!(params.pos_annual_rate_bps, 400);
        ParamId::MinStakeAmount.apply(&mut params, 2_000_000);
        assert_eq!(params.min_stake_amount, 2_000_000);
    }
    #[test]
    fn governance_state_machine_propose_vote_activate() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        // Use a very small min_stake_age so the test UTXOs (created at
        // height 0) are old enough for every vote.
        g.min_stake_age = 1;

        // A proposal to set the annual rate to 400 bps, activating at height 20.
        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 20,
            voting_end: 10,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        let prop_out = vec![(op_return_script(&p.encode()), "proposer".to_string())];
        g.process_block(1, &prop_out, |_| vec![]);

        // Votes: 400 yes, 100 no (500/1000 = 50% quorum; 80% yes).
        let vote_yes = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        let vote_no = Vote {
            proposal_id: id,
            choice: VoteChoice::No,
        };
        // Each voter has one UTXO created at height 0 (well beyond min_stake_age).
        let utxo_a = make_test_utxo(1, 400, 0);
        let utxo_b = make_test_utxo(2, 100, 0);
        g.process_block(
            2,
            &[(op_return_script(&vote_yes.encode()), "a".to_string())],
            move |addr| if addr == "a" { utxo_a.clone() } else { vec![] },
        );
        g.process_block(
            3,
            &[(op_return_script(&vote_no.encode()), "b".to_string())],
            move |addr| if addr == "b" { utxo_b.clone() } else { vec![] },
        );

        // Before voting_end: still open, param unchanged.
        assert_eq!(g.params.pos_annual_rate_bps, 500);

        // At voting_end the proposal decides (passed) but doesn't activate yet.
        g.process_block(10, &[], |_| vec![]);
        assert_eq!(
            g.params.pos_annual_rate_bps, 500,
            "not active before activation height"
        );

        // At activation height the param applies.
        g.process_block(20, &[], |_| vec![]);
        assert_eq!(
            g.params.pos_annual_rate_bps, 400,
            "param applied at activation"
        );
    }

    fn make_test_utxo(
        vout: u32,
        value: u64,
        created_height: u32,
    ) -> Vec<([u8; 32], u32, u64, u32)> {
        let txid = [vout as u8; 32];
        vec![(txid, vout, value, created_height)]
    }

    fn op_return_script(data: &[u8]) -> Vec<u8> {
        let mut s = vec![0x6a, data.len() as u8];
        s.extend_from_slice(data);
        s
    }

    #[test]
    fn vote_locks_utxos() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        g.min_stake_age = 1;

        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 100,
            voting_end: 50,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        g.process_block(
            1,
            &[(op_return_script(&p.encode()), "proposer".into())],
            |_| vec![],
        );

        let vote = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        let utxo_txid = [0xAAu8; 32];
        g.process_block(
            2,
            &[(op_return_script(&vote.encode()), "voter".into())],
            move |addr| {
                if addr == "voter" {
                    vec![(utxo_txid, 0, 500, 0)]
                } else {
                    vec![]
                }
            },
        );

        // UTXO should be locked.
        assert!(g.is_utxo_locked(&utxo_txid, 0, 2));
        assert!(g.is_utxo_locked(&utxo_txid, 0, 50)); // still locked at voting_end
        assert!(!g.is_utxo_locked(&utxo_txid, 0, 50 + DEFAULT_LOCK_GRACE_BLOCKS)); // unlocked
        assert_eq!(g.locked_utxos.len(), 1);
        // Vote should be counted.
        let rec = g.proposals.get(&id).unwrap();
        assert_eq!(rec.yes, 500);
    }

    #[test]
    fn vote_rejected_if_utxo_too_young() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        g.min_stake_age = 10; // require 10 blocks age

        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 100,
            voting_end: 50,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        g.process_block(
            1,
            &[(op_return_script(&p.encode()), "proposer".into())],
            |_| vec![],
        );

        let vote = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        // UTXO created at height 5, voted at height 8 → age 3 < min_stake_age 10.
        g.process_block(
            8,
            &[(op_return_script(&vote.encode()), "voter".into())],
            move |addr| {
                if addr == "voter" {
                    vec![([0xBBu8; 32], 0, 500, 5)]
                } else {
                    vec![]
                }
            },
        );

        // Vote should be rejected: no locks, no tally.
        assert!(g.locked_utxos.is_empty());
        let rec = g.proposals.get(&id).unwrap();
        assert_eq!(rec.yes, 0);
    }

    #[test]
    fn param_activation_at_height() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        g.min_stake_age = 1;

        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 300,
            activation_height: 20,
            voting_end: 10,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        g.process_block(
            1,
            &[(op_return_script(&p.encode()), "proposer".into())],
            |_| vec![],
        );

        // Pass the proposal with overwhelming vote.
        let vote = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        g.process_block(
            2,
            &[(op_return_script(&vote.encode()), "whale".into())],
            move |addr| {
                if addr == "whale" {
                    vec![([0xCCu8; 32], 0, 900, 0)]
                } else {
                    vec![]
                }
            },
        );

        // Before voting_end: param unchanged.
        g.process_block(9, &[], |_| vec![]);
        assert_eq!(g.params.pos_annual_rate_bps, 500);

        // At voting_end: decided but not yet active.
        g.process_block(10, &[], |_| vec![]);
        assert_eq!(g.params.pos_annual_rate_bps, 500);

        // At activation_height: param applied.
        g.process_block(20, &[], |_| vec![]);
        assert_eq!(g.params.pos_annual_rate_bps, 300);
    }

    #[test]
    fn lock_expires_and_unlocks() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        g.min_stake_age = 1;

        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 100,
            voting_end: 10,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        g.process_block(
            1,
            &[(op_return_script(&p.encode()), "proposer".into())],
            |_| vec![],
        );

        let vote = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        let utxo_txid = [0xDDu8; 32];
        g.process_block(
            2,
            &[(op_return_script(&vote.encode()), "voter".into())],
            move |addr| {
                if addr == "voter" {
                    vec![(utxo_txid, 0, 500, 0)]
                } else {
                    vec![]
                }
            },
        );

        let unlock = 10 + DEFAULT_LOCK_GRACE_BLOCKS;
        assert!(g.is_utxo_locked(&utxo_txid, 0, unlock - 1));
        assert!(!g.is_utxo_locked(&utxo_txid, 0, unlock));
        // After processing a block at unlock height, the lock is pruned.
        g.process_block(unlock, &[], |_| vec![]);
        assert!(g.locked_utxos.is_empty());
    }

    #[test]
    fn vote_after_voting_end_is_ignored() {
        let mut g = GovernanceState::new();
        g.total_staked = 1000;
        g.min_stake_age = 1;

        let p = Proposal {
            param: ParamId::PosAnnualRateBps,
            new_value: 400,
            activation_height: 100,
            voting_end: 10,
            deposit: 0,
        };
        let id = proposal_id_of(&p, 1);
        g.process_block(
            1,
            &[(op_return_script(&p.encode()), "proposer".into())],
            |_| vec![],
        );

        // Vote at height 11 (after voting_end=10).
        let vote = Vote {
            proposal_id: id,
            choice: VoteChoice::Yes,
        };
        g.process_block(
            11,
            &[(op_return_script(&vote.encode()), "late_voter".into())],
            move |addr| {
                if addr == "late_voter" {
                    vec![([0xEEu8; 32], 0, 500, 0)]
                } else {
                    vec![]
                }
            },
        );

        // No locks, no tally.
        assert!(g.locked_utxos.is_empty());
        let rec = g.proposals.get(&id).unwrap();
        assert_eq!(rec.yes, 0);
    }
}
