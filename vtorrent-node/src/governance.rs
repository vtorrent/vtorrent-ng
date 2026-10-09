//! On-chain governance: proposals and stake-weighted votes (Steps 1–2).
//!
//! Proposals and votes are OP_RETURN data carriers with a domain prefix, so
//! they are permanent, censorship-resistant, and visible to every node.
//! Voting power is snapshotted at proposal creation and coins are locked
//! through the voting window. See `docs/governance-implementation-plan.md`.
//!
//! This module is the **pure core**: encoding/decoding the OP_RETURN payloads
//! and tallying votes. Chain integration (scanning, snapshot, lock enforcement)
//! builds on it.

use crate::consensus::ConsensusParams;
use serde::{Deserialize, Serialize};

/// Proposal OP_RETURN prefix: `VTRG1`.
pub const PROPOSAL_PREFIX: &[u8; 5] = b"VTRG1";
/// Vote OP_RETURN prefix: `VTRV1`.
pub const VOTE_PREFIX: &[u8; 5] = b"VTRV1";

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
}
