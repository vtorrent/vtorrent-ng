//! Consensus deployment status (BIP-9 versionbits). Read-only.
//! See `docs/network-upgrade-design.md`.

use axum::{extract::State, Json};
use serde::Serialize;
use std::sync::Arc;

use crate::error::RpcResult;
use crate::state::AppState;
use vtorrent_node::deployments::{
    deployment_state, Deployment, DeploymentState, DEFAULT_PERIOD, DEFAULT_THRESHOLD_PCT,
};

#[derive(Debug, Serialize)]
pub struct DeploymentStatus {
    pub name: String,
    pub bit: u8,
    pub start_height: u32,
    pub timeout_height: u32,
    pub state: DeploymentState,
    /// Signalling count in the current (in-progress) period.
    pub signals_current_period: u32,
    pub period: u32,
    pub threshold_pct: u32,
}

#[derive(Debug, Serialize)]
pub struct DeploymentsResponse {
    pub tip_height: u64,
    pub deployments: Vec<DeploymentStatus>,
}

/// The deployments this node knows about. Empty until a deployment is scheduled
/// (a fresh genesis / coordinated upgrade defines them).
fn known_deployments() -> Vec<Deployment> {
    // No deployments are scheduled on the current chain. When the consensus
    // batch is activated, add entries here (name, bit, start, timeout).
    Vec::new()
}

/// GET /api/v1/deployments
pub async fn get_deployments(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<DeploymentsResponse>> {
    let chain = state.chain.lock().await;
    let tip = chain.best_height();
    let deployments = known_deployments()
        .into_iter()
        .map(|d| {
            let signals = chain.count_signals(d.bit, DEFAULT_PERIOD);
            let st = deployment_state(&d, DEFAULT_PERIOD, DEFAULT_THRESHOLD_PCT, tip, &signals);
            let current_period = (tip / DEFAULT_PERIOD) as usize;
            DeploymentStatus {
                name: d.name,
                bit: d.bit,
                start_height: d.start_height,
                timeout_height: d.timeout_height,
                state: st,
                signals_current_period: signals.get(current_period).copied().unwrap_or(0),
                period: DEFAULT_PERIOD,
                threshold_pct: DEFAULT_THRESHOLD_PCT,
            }
        })
        .collect();
    Ok(Json(DeploymentsResponse {
        tip_height: tip as u64,
        deployments,
    }))
}
