//! Authenticated, outpoint-weighted governance and lock-preserving staking.

use crate::{
    block::Transaction,
    chain::Utxo,
    consensus::{is_stakeable, is_stakeable_with, ConsensusParams, COIN, MAX_SUPPLY},
    error::{NodeError, Result},
};
use secp256k1::{ecdsa::RecoverableSignature, ecdsa::RecoveryId, Message, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PROPOSAL_PREFIX: &[u8; 5] = b"VTRG1";
pub const VOTE_PREFIX: &[u8; 5] = b"VTRV1";
pub const SIGNED_VOTE_PREFIX: &[u8; 5] = b"VTRV2";
pub const DEFAULT_QUORUM_PCT: u64 = 33;
pub const DEFAULT_THRESHOLD_PCT: u64 = 66;
pub const DEFAULT_LOCK_GRACE_BLOCKS: u32 = 6;
pub const MIN_VOTING_BLOCKS: u32 = 2880;
pub const MAX_VOTING_BLOCKS: u32 = 10080;
pub const MIN_ACTIVATION_DELAY: u32 = 2016;
pub const MAX_ACTIVATION_DELAY: u32 = 20160;
pub const MIN_PROPOSAL_DEPOSIT: u64 = 100 * COIN;

pub type OutPoint = ([u8; 32], u32);

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

    /// Validate the candidate parameter set before changing the live values.
    pub fn apply(self, params: &mut ConsensusParams, value: u64) -> Result<()> {
        let mut next = *params;
        match self {
            Self::PosAnnualRateBps if (1..=1000).contains(&value) => {
                next.pos_annual_rate_bps = value as u32
            }
            Self::RewardAgeCap if (60..=365 * 86400).contains(&value) => {
                next.reward_age_cap = value
            }
            Self::MinStakeAge if (60..=365 * 86400).contains(&value) => next.min_stake_age = value,
            Self::MaxStakeAge if (60..=365 * 86400).contains(&value) => next.max_stake_age = value,
            Self::MinStakeAmount if (COIN..=MAX_SUPPLY).contains(&value) => {
                next.min_stake_amount = value
            }
            Self::TargetBlockTime if (30..=600).contains(&value) => next.target_block_time = value,
            _ => return Err(invalid("governance parameter outside allowed range")),
        }
        if next.min_stake_age > next.max_stake_age {
            return Err(invalid("minimum stake age exceeds maximum stake age"));
        }
        *params = next;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub param: ParamId,
    pub new_value: u64,
    pub activation_height: u32,
    pub voting_end: u32,
    /// Permanently burned in the proposal's OP_RETURN output.
    pub deposit: u64,
}

impl Proposal {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = PROPOSAL_PREFIX.to_vec();
        b.push(self.param as u8);
        b.extend_from_slice(&self.new_value.to_le_bytes());
        b.extend_from_slice(&self.activation_height.to_le_bytes());
        b.extend_from_slice(&self.voting_end.to_le_bytes());
        b.extend_from_slice(&self.deposit.to_le_bytes());
        b
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != 30 || !data.starts_with(PROPOSAL_PREFIX) {
            return None;
        }
        Some(Self {
            param: ParamId::from_u8(data[5])?,
            new_value: u64::from_le_bytes(data[6..14].try_into().ok()?),
            activation_height: u32::from_le_bytes(data[14..18].try_into().ok()?),
            voting_end: u32::from_le_bytes(data[18..22].try_into().ok()?),
            deposit: u64::from_le_bytes(data[22..30].try_into().ok()?),
        })
    }

    fn validate(&self, height: u32, burned: u64, params: ConsensusParams) -> Result<()> {
        let duration = self
            .voting_end
            .checked_sub(height)
            .ok_or_else(|| invalid("voting end is in the past"))?;
        let delay = self
            .activation_height
            .checked_sub(self.voting_end)
            .ok_or_else(|| invalid("activation precedes voting end"))?;
        if !(MIN_VOTING_BLOCKS..=MAX_VOTING_BLOCKS).contains(&duration)
            || !(MIN_ACTIVATION_DELAY..=MAX_ACTIVATION_DELAY).contains(&delay)
            || self
                .voting_end
                .checked_add(DEFAULT_LOCK_GRACE_BLOCKS)
                .is_none()
            || self.deposit < MIN_PROPOSAL_DEPOSIT
            || burned != self.deposit
        {
            return Err(invalid(
                "invalid proposal window, activation delay, or burned deposit",
            ));
        }
        let mut candidate = params;
        self.param.apply(&mut candidate, self.new_value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteChoice {
    Yes = 1,
    No = 2,
    Abstain = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    pub proposal_id: [u8; 8],
    pub choice: VoteChoice,
}

impl Vote {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = VOTE_PREFIX.to_vec();
        b.extend_from_slice(&self.proposal_id);
        b.push(self.choice as u8);
        b
    }
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != 14 || !data.starts_with(VOTE_PREFIX) {
            return None;
        }
        Some(Self {
            proposal_id: data[5..13].try_into().ok()?,
            choice: match data[13] {
                1 => VoteChoice::Yes,
                2 => VoteChoice::No,
                3 => VoteChoice::Abstain,
                _ => return None,
            },
        })
    }
}

/// A vote signed by the spending authority of one explicit, unspent outpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedVote {
    pub vote: Vote,
    pub outpoint: OutPoint,
    pub signature: Vec<u8>,
}

impl SignedVote {
    fn unsigned_bytes(&self) -> Vec<u8> {
        let mut bytes = SIGNED_VOTE_PREFIX.to_vec();
        bytes.extend_from_slice(&self.vote.proposal_id);
        bytes.push(self.vote.choice as u8);
        bytes.extend_from_slice(&self.outpoint.0);
        bytes.extend_from_slice(&self.outpoint.1.to_le_bytes());
        bytes
    }
    fn digest(&self, genesis: [u8; 32]) -> [u8; 32] {
        let mut bytes = b"vTorrent governance vote v2\0".to_vec();
        bytes.extend_from_slice(&genesis);
        bytes.extend_from_slice(&self.unsigned_bytes());
        vtorrent_core::crypto::sha256d(&bytes)
    }
    pub fn sign(vote: Vote, outpoint: OutPoint, genesis: [u8; 32], key: &SecretKey) -> Self {
        let mut result = Self {
            vote,
            outpoint,
            signature: Vec::new(),
        };
        let (id, sig) = Secp256k1::new()
            .sign_ecdsa_recoverable(&Message::from_digest(result.digest(genesis)), key)
            .serialize_compact();
        result.signature.push(id.to_i32() as u8);
        result.signature.extend_from_slice(&sig);
        result
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.unsigned_bytes();
        bytes.extend_from_slice(&self.signature);
        bytes
    }
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != 115 || !data.starts_with(SIGNED_VOTE_PREFIX) {
            return None;
        }
        let mut legacy = VOTE_PREFIX.to_vec();
        legacy.extend_from_slice(&data[5..14]);
        Some(Self {
            vote: Vote::decode(&legacy)?,
            outpoint: (
                data[14..46].try_into().ok()?,
                u32::from_le_bytes(data[46..50].try_into().ok()?),
            ),
            signature: data[50..].to_vec(),
        })
    }
    /// Governance votes exceed the generic 80-byte data-carrier builder limit.
    pub fn script(&self) -> Result<Vec<u8>> {
        let mut s = vtorrent_script::Script::new();
        s.push_opcode(0x6a);
        s.push_data(&self.encode())
            .map_err(|_| invalid("invalid governance vote payload"))?;
        Ok(s.as_bytes().to_vec())
    }
    fn owner(&self, utxo: &Utxo, genesis: [u8; 32]) -> Result<String> {
        if self.signature.len() != 65 {
            return Err(invalid("invalid vote signature length"));
        }
        let id = RecoveryId::from_i32(self.signature[0] as i32)
            .map_err(|_| invalid("invalid vote recovery id"))?;
        let sig = RecoverableSignature::from_compact(&self.signature[1..], id)
            .map_err(|_| invalid("invalid vote signature"))?;
        let pk = Secp256k1::new()
            .recover_ecdsa(&Message::from_digest(self.digest(genesis)), &sig)
            .map_err(|_| invalid("invalid vote signature"))?;
        let script = vtorrent_script::Script::from_bytes(utxo.script_pubkey.clone())
            .map_err(|_| invalid("invalid voting script"))?;
        let owner_hash: [u8; 20] = match vtorrent_script::classify_script(&script) {
            vtorrent_script::ScriptType::P2PKH => utxo.script_pubkey[3..23].try_into().unwrap(),
            vtorrent_script::ScriptType::P2CS { spending_hash, .. } => spending_hash,
            _ => return Err(invalid("unsupported voting script")),
        };
        let compressed = owner_hash == vtorrent_core::crypto::hash160(&pk.serialize());
        if !compressed && owner_hash != vtorrent_core::crypto::hash160(&pk.serialize_uncompressed())
        {
            return Err(invalid(
                "vote signature does not authorize the voting outpoint",
            ));
        }
        Ok(vtorrent_core::address::Address::from_pubkey(&pk, compressed, 70).to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalOutcome {
    Open,
    Failed,
    Passed {
        param: ParamId,
        new_value: u64,
        activation_height: u32,
    },
}

pub fn tally(
    p: &Proposal,
    yes: u64,
    no: u64,
    abstain: u64,
    total_staked: u64,
    height: u32,
) -> ProposalOutcome {
    if height < p.voting_end {
        return ProposalOutcome::Open;
    }
    let turnout = yes as u128 + no as u128 + abstain as u128;
    let decisive = yes as u128 + no as u128;
    if total_staked == 0
        || turnout * 100 < total_staked as u128 * DEFAULT_QUORUM_PCT as u128
        || decisive == 0
        || yes as u128 * 100 < decisive * DEFAULT_THRESHOLD_PCT as u128
    {
        return ProposalOutcome::Failed;
    }
    ProposalOutcome::Passed {
        param: p.param,
        new_value: p.new_value,
        activation_height: p.activation_height,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub proposal: Proposal,
    pub created_height: u32,
    pub yes: u64,
    pub no: u64,
    pub abstain: u64,
    pub decided: bool,
    pub passed: bool,
    pub applied: bool,
    pub activation_failed: bool,
    pub quorum_supply: u64,
    pub voted_outpoints: BTreeSet<OutPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteLock {
    pub txid: [u8; 32],
    pub vout: u32,
    pub unlock_height: u32,
    pub voter: String,
    pub proposal_id: [u8; 8],
    pub voting_origin: OutPoint,
    pub voting_value: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GovernanceState {
    pub proposals: BTreeMap<[u8; 8], ProposalRecord>,
    pub locked_utxos: BTreeMap<OutPoint, VoteLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProposalUndo {
    Added([u8; 8]),
    Removed([u8; 8], Box<ProposalRecord>),
    Updated {
        id: [u8; 8],
        metadata: Box<ProposalRecord>,
        added_votes: Vec<OutPoint>,
        removed_votes: Vec<OutPoint>,
    },
}

/// Governance changes needed to restore one block's pre-state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GovernanceUndo {
    proposals: Vec<ProposalUndo>,
    locks: Vec<(OutPoint, Option<VoteLock>)>,
}

fn proposal_metadata(record: &ProposalRecord) -> ProposalRecord {
    ProposalRecord {
        proposal: record.proposal.clone(),
        created_height: record.created_height,
        yes: record.yes,
        no: record.no,
        abstain: record.abstain,
        decided: record.decided,
        passed: record.passed,
        applied: record.applied,
        activation_failed: record.activation_failed,
        quorum_supply: record.quorum_supply,
        voted_outpoints: BTreeSet::new(),
    }
}

impl GovernanceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a compact undo record after a tentative block transition.
    pub fn undo_since(&self, before: &Self) -> GovernanceUndo {
        let mut undo = GovernanceUndo::default();
        for (id, record) in &self.proposals {
            match before.proposals.get(id) {
                None => undo.proposals.push(ProposalUndo::Added(*id)),
                Some(old) if old != record => undo.proposals.push(ProposalUndo::Updated {
                    id: *id,
                    metadata: Box::new(proposal_metadata(old)),
                    added_votes: record
                        .voted_outpoints
                        .difference(&old.voted_outpoints)
                        .copied()
                        .collect(),
                    removed_votes: old
                        .voted_outpoints
                        .difference(&record.voted_outpoints)
                        .copied()
                        .collect(),
                }),
                _ => {}
            }
        }
        for (id, old) in &before.proposals {
            if !self.proposals.contains_key(id) {
                undo.proposals
                    .push(ProposalUndo::Removed(*id, Box::new(old.clone())));
            }
        }
        for (key, old) in &before.locked_utxos {
            if self.locked_utxos.get(key) != Some(old) {
                undo.locks.push((*key, Some(old.clone())));
            }
        }
        for key in self.locked_utxos.keys() {
            if !before.locked_utxos.contains_key(key) {
                undo.locks.push((*key, None));
            }
        }
        undo
    }

    /// Restore a transition using its journaled changes.
    pub fn restore_undo(&mut self, undo: &GovernanceUndo) {
        for change in undo.proposals.iter().rev() {
            match change {
                ProposalUndo::Added(id) => {
                    self.proposals.remove(id);
                }
                ProposalUndo::Removed(id, record) => {
                    self.proposals.insert(*id, record.as_ref().clone());
                }
                ProposalUndo::Updated {
                    id,
                    metadata,
                    added_votes,
                    removed_votes,
                } => {
                    let record = self
                        .proposals
                        .get_mut(id)
                        .expect("journaled proposal must exist");
                    let mut votes = std::mem::take(&mut record.voted_outpoints);
                    for vote in added_votes {
                        votes.remove(vote);
                    }
                    votes.extend(removed_votes.iter().copied());
                    *record = metadata.as_ref().clone();
                    record.voted_outpoints = votes;
                }
            }
        }
        for (key, old) in undo.locks.iter().rev() {
            match old {
                Some(lock) => {
                    self.locked_utxos.insert(*key, lock.clone());
                }
                None => {
                    self.locked_utxos.remove(key);
                }
            }
        }
    }

    /// Capture the governance entries a tentative transaction can change.
    pub(crate) fn transaction_undo(&self, tx: &Transaction, height: u32) -> GovernanceUndo {
        let mut proposals: BTreeMap<[u8; 8], BTreeSet<OutPoint>> = BTreeMap::new();
        let mut locks = BTreeMap::new();
        for output in &tx.outputs {
            let Some(data) = op_return_data(&output.script_pubkey) else {
                continue;
            };
            if let Some(proposal) = Proposal::decode(data) {
                proposals
                    .entry(proposal_id_of(&proposal, height))
                    .or_default();
            } else if let Some(signed) = SignedVote::decode(data) {
                let origin = self
                    .locked_utxos
                    .get(&signed.outpoint)
                    .map(|lock| lock.voting_origin)
                    .unwrap_or(signed.outpoint);
                let votes = proposals.entry(signed.vote.proposal_id).or_default();
                if self
                    .proposals
                    .get(&signed.vote.proposal_id)
                    .is_some_and(|record| !record.voted_outpoints.contains(&origin))
                {
                    votes.insert(origin);
                }
                locks
                    .entry(signed.outpoint)
                    .or_insert_with(|| self.locked_utxos.get(&signed.outpoint).cloned());
            }
        }
        GovernanceUndo {
            proposals: proposals
                .into_iter()
                .map(|(id, votes)| match self.proposals.get(&id) {
                    Some(record) => ProposalUndo::Updated {
                        id,
                        metadata: Box::new(proposal_metadata(record)),
                        added_votes: votes.into_iter().collect(),
                        removed_votes: Vec::new(),
                    },
                    None => ProposalUndo::Added(id),
                })
                .collect(),
            locks: locks.into_iter().collect(),
        }
    }
    pub fn is_utxo_locked(&self, txid: &[u8; 32], vout: u32, height: u32) -> bool {
        self.locked_utxos
            .get(&(*txid, vout))
            .is_some_and(|l| height < l.unlock_height)
    }
    /// Apply each activation once, ordered by activation height, creation height, then id.
    pub fn begin_block(
        &mut self,
        height: u32,
        params: &mut ConsensusParams,
        utxos: &BTreeMap<OutPoint, Utxo>,
        timestamp: u32,
    ) {
        self.locked_utxos
            .retain(|_, lock| height < lock.unlock_height);
        let mut pending = Vec::new();
        for (id, rec) in &mut self.proposals {
            if !rec.decided && height >= rec.proposal.voting_end {
                rec.decided = true;
                rec.passed = matches!(
                    tally(
                        &rec.proposal,
                        rec.yes,
                        rec.no,
                        rec.abstain,
                        rec.quorum_supply,
                        height
                    ),
                    ProposalOutcome::Passed { .. }
                );
            }
            if rec.decided && height >= rec.proposal.voting_end {
                rec.voted_outpoints.clear();
            }
            if rec.passed && !rec.applied && height >= rec.proposal.activation_height {
                pending.push((rec.proposal.activation_height, rec.created_height, *id));
            }
        }
        pending.sort_unstable();
        for (_, _, id) in pending {
            let rec = self.proposals.get_mut(&id).unwrap();
            let mut candidate = *params;
            rec.activation_failed = rec
                .proposal
                .param
                .apply(&mut candidate, rec.proposal.new_value)
                .is_err()
                || !utxos.values().any(|u| {
                    is_stakeable_with(&candidate, u)
                        && timestamp.checked_sub(u.timestamp).is_some_and(|age| {
                            u64::from(age) >= candidate.min_stake_age
                                && u64::from(age) <= candidate.max_stake_age
                        })
                });
            if !rec.activation_failed {
                *params = candidate;
            }
            rec.applied = true;
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn process_transaction(
        &mut self,
        tx: &Transaction,
        height: u32,
        timestamp: u32,
        utxos: &BTreeMap<OutPoint, Utxo>,
        params: ConsensusParams,
        total_staked: u64,
        genesis: [u8; 32],
    ) -> Result<()> {
        for output in &tx.outputs {
            let Some(data) = op_return_data(&output.script_pubkey) else {
                continue;
            };
            if data.starts_with(PROPOSAL_PREFIX) {
                let proposal = Proposal::decode(data)
                    .ok_or_else(|| invalid("malformed governance proposal"))?;
                proposal.validate(height, output.value, params)?;
                let id = proposal_id_of(&proposal, height);
                if self.proposals.contains_key(&id) {
                    return Err(invalid("duplicate governance proposal"));
                }
                self.proposals.insert(
                    id,
                    ProposalRecord {
                        proposal,
                        created_height: height,
                        yes: 0,
                        no: 0,
                        abstain: 0,
                        decided: false,
                        passed: false,
                        applied: false,
                        activation_failed: false,
                        quorum_supply: total_staked,
                        voted_outpoints: BTreeSet::new(),
                    },
                );
            } else if data.starts_with(SIGNED_VOTE_PREFIX) {
                let signed =
                    SignedVote::decode(data).ok_or_else(|| invalid("malformed governance vote"))?;
                let utxo = utxos
                    .get(&signed.outpoint)
                    .ok_or_else(|| invalid("voting outpoint is not unspent"))?;
                if !is_stakeable(utxo)
                    || utxo.value < params.min_stake_amount
                    || utxo.timestamp > timestamp
                    || ((timestamp - utxo.timestamp) as u64) < params.min_stake_age
                {
                    return Err(invalid("voting outpoint is not mature stake"));
                }
                let voter = signed.owner(utxo, genesis)?;
                let (origin, stake) = self
                    .locked_utxos
                    .get(&signed.outpoint)
                    .map(|l| (l.voting_origin, l.voting_value.min(utxo.value)))
                    .unwrap_or((signed.outpoint, utxo.value));
                let rec = self
                    .proposals
                    .get_mut(&signed.vote.proposal_id)
                    .ok_or_else(|| invalid("unknown governance proposal"))?;
                if height <= rec.created_height || height >= rec.proposal.voting_end || rec.decided
                {
                    return Err(invalid("governance voting window is closed"));
                }
                if !rec.voted_outpoints.insert(origin) {
                    return Err(invalid("voting outpoint already counted for this proposal"));
                }
                let tally = match signed.vote.choice {
                    VoteChoice::Yes => &mut rec.yes,
                    VoteChoice::No => &mut rec.no,
                    VoteChoice::Abstain => &mut rec.abstain,
                };
                *tally = tally
                    .checked_add(stake)
                    .ok_or_else(|| invalid("vote tally overflow"))?;
                let unlock = rec
                    .proposal
                    .voting_end
                    .checked_add(DEFAULT_LOCK_GRACE_BLOCKS)
                    .ok_or_else(|| invalid("vote unlock overflow"))?;
                let lock = self
                    .locked_utxos
                    .entry(signed.outpoint)
                    .or_insert(VoteLock {
                        txid: utxo.txid,
                        vout: utxo.vout,
                        unlock_height: unlock,
                        voter,
                        proposal_id: signed.vote.proposal_id,
                        voting_origin: origin,
                        voting_value: stake,
                    });
                if unlock > lock.unlock_height {
                    lock.unlock_height = unlock;
                    lock.proposal_id = signed.vote.proposal_id;
                }
            } else if data.starts_with(VOTE_PREFIX) {
                return Err(invalid("unsigned governance votes are not authorized"));
            }
        }
        Ok(())
    }
    /// Move a locked stake to its sole value-bearing output without changing voting identity.
    pub fn transfer_stake_lock(
        &mut self,
        tx: &Transaction,
        utxo: &Utxo,
        height: u32,
    ) -> Result<()> {
        let key = (utxo.txid, utxo.vout);
        if !self.is_utxo_locked(&utxo.txid, utxo.vout, height) {
            return Ok(());
        }
        let outputs: Vec<_> = tx
            .outputs
            .iter()
            .enumerate()
            .filter(|(_, o)| o.value > 0)
            .collect();
        if !tx.is_coinstake()
            || outputs.len() != 1
            || outputs[0].1.script_pubkey != utxo.script_pubkey
            || outputs[0].1.value < utxo.value
        {
            return Err(invalid(
                "vote-locked stake must re-lock all value to the same script",
            ));
        }
        let mut lock = self.locked_utxos.remove(&key).unwrap();
        lock.txid = tx.txid();
        lock.vout = outputs[0].0 as u32;
        self.locked_utxos.insert((lock.txid, lock.vout), lock);
        Ok(())
    }
}

pub fn proposal_id_of(p: &Proposal, height: u32) -> [u8; 8] {
    let mut bytes = p.encode();
    bytes.extend_from_slice(&height.to_le_bytes());
    vtorrent_core::crypto::sha256(&bytes)[..8]
        .try_into()
        .unwrap()
}

fn op_return_data(script: &[u8]) -> Option<&[u8]> {
    if script.first() != Some(&0x6a) {
        return None;
    }
    let (offset, len) = match *script.get(1)? {
        n @ 1..=75 => (2, n as usize),
        0x4c => (3, *script.get(2)? as usize),
        _ => return None,
    };
    if script.len() != offset + len {
        return None;
    }
    script.get(offset..)
}

fn invalid(message: &str) -> NodeError {
    NodeError::InvalidTransaction(message.into())
}
