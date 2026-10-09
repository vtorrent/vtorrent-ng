//! Governance: read-only proposals, votes, and current parameters.
//!
//! Scans the chain for governance OP_RETURNs (like the explorer/indexer
//! pattern) and reports the current parameters. Read-only; no consensus wiring.
//! See `docs/governance-implementation-plan.md` Step 4.

use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::error::RpcResult;
use crate::state::AppState;
use vtorrent_node::governance::GovernanceState;

#[derive(Debug, Serialize)]
pub struct ParamsResponse {
    pub pos_annual_rate_bps: u32,
    pub reward_age_cap: u64,
    pub max_stake_age: u64,
    pub min_stake_age: u64,
    pub min_stake_amount: u64,
    pub target_block_time: u64,
}

#[derive(Debug, Serialize)]
pub struct ProposalView {
    pub id: String,
    pub param: String,
    pub new_value: u64,
    pub activation_height: u32,
    pub voting_end: u32,
    pub created_height: u32,
    pub yes: u64,
    pub no: u64,
    pub abstain: u64,
    pub decided: bool,
    pub passed: bool,
}

#[derive(Debug, Serialize)]
pub struct ProposalsResponse {
    pub tip_height: u64,
    pub proposals: Vec<ProposalView>,
}

/// GET /api/v1/governance/params
pub async fn get_governance_params(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<ParamsResponse>> {
    let p = state.chain.lock().await.params();
    Ok(Json(ParamsResponse {
        pos_annual_rate_bps: p.pos_annual_rate_bps,
        reward_age_cap: p.reward_age_cap,
        max_stake_age: p.max_stake_age,
        min_stake_age: p.min_stake_age,
        min_stake_amount: p.min_stake_amount,
        target_block_time: p.target_block_time,
    }))
}

/// GET /api/v1/governance/proposals
///
/// Scans the chain's governance OP_RETURNs and tallies votes. Read-only; the
/// scan is bounded by `limit` blocks from the tip.
pub async fn get_governance_proposals(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<ProposalsResponse>> {
    let chain = state.chain.lock().await;
    let tip = chain.best_height();
    let total_staked = chain.total_staked();

    let mut gov = GovernanceState::new();
    gov.total_staked = total_staked;

    // Bound the scan (a full address index is the proper long-term fix).
    const MAX_SCAN_BLOCKS: u32 = 200_000;
    let start = tip.saturating_sub(MAX_SCAN_BLOCKS);
    for h in start..=tip {
        let Some(block) = chain.get_block_at_height(h) else {
            continue;
        };
        // Collect governance OP_RETURN outputs (script, voter address).
        let mut outputs: Vec<(Vec<u8>, String)> = Vec::new();
        for tx in &block.transactions {
            let voter = tx.claim_address.clone().unwrap_or_default();
            for out in &tx.outputs {
                if out.script_pubkey.first() == Some(&0x6a) {
                    outputs.push((out.script_pubkey.clone(), voter.clone()));
                }
            }
        }
        // Voting power: the voter's current stakeable UTXOs (a simplification;
        // the design snapshots at proposal creation — a documented follow-up).
        gov.process_block(h, &outputs, |addr| {
            if addr.is_empty() {
                0
            } else {
                chain
                    .get_utxos_for_address(addr)
                    .iter()
                    .map(|u| u.value)
                    .sum()
            }
        });
    }

    let mut proposals: Vec<ProposalView> = gov
        .proposals
        .iter()
        .map(|(id, rec)| ProposalView {
            id: hex::encode(id),
            param: format!("{:?}", rec.proposal.param),
            new_value: rec.proposal.new_value,
            activation_height: rec.proposal.activation_height,
            voting_end: rec.proposal.voting_end,
            created_height: rec.created_height,
            yes: rec.yes,
            no: rec.no,
            abstain: rec.abstain,
            decided: rec.decided,
            passed: rec.passed,
        })
        .collect();
    proposals.sort_by_key(|p| p.created_height);

    Ok(Json(ProposalsResponse {
        tip_height: tip as u64,
        proposals,
    }))
}

#[cfg(test)]
mod tests {
    use vtorrent_node::governance::{Proposal, Vote};

    #[test]
    fn proposal_and_vote_decode_via_handler_types() {
        // Sanity: the handler's types round-trip through the node encoders.
        let p = Proposal {
            param: vtorrent_node::governance::ParamId::TargetBlockTime,
            new_value: 30,
            activation_height: 100,
            voting_end: 50,
            deposit: 0,
        };
        assert_eq!(Proposal::decode(&p.encode()), Some(p));
        let v = Vote {
            proposal_id: [1u8; 8],
            choice: vtorrent_node::governance::VoteChoice::Yes,
        };
        assert_eq!(Vote::decode(&v.encode()), Some(v));
    }
}
