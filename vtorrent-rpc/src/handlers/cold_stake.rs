//! Cold-stake funding attestation endpoints.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::handlers::{now_secs_mock, parse_hash32};
use crate::models::FundingAttestationRequest;
use crate::state::AppState;
use vtorrent_core::attestation::FundingAttestation;

fn refusal_reason(refusal: crate::cold_stake_attestation::AttestationRefusal) -> &'static str {
    use crate::cold_stake_attestation::AttestationRefusal;
    match refusal {
        AttestationRefusal::UnknownTransaction => "transaction is unknown",
        AttestationRefusal::NotConfirmed => "transaction is only in the mempool",
        AttestationRefusal::MissingOutput => "output index does not exist",
        AttestationRefusal::NotUnspent => "output is not in the active unspent set",
        AttestationRefusal::MempoolSpend => "a mempool transaction spends the output",
        AttestationRefusal::TooFewConfirmations { .. } => "not enough confirmations",
    }
}

/// POST /api/v1/cold-stake/funding-attestation
pub async fn get_funding_attestation(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FundingAttestationRequest>,
) -> RpcResult<Json<FundingAttestation>> {
    let path = state.cold_stake_attestor_key_path.as_ref().ok_or_else(|| {
        RpcError::BadRequest("Cold-stake funding attestation is not configured on this node".into())
    })?;
    let txid = parse_hash32(&req.txid, "txid")?;
    let key = crate::cold_stake_attestor::load_or_create_attestor_key(path)
        .map_err(|error| RpcError::Internal(format!("Attestor key unavailable: {error}")))?;
    let network = state.network.clone();
    let now = now_secs_mock(&state).await;
    let mut attestation = {
        let chain = state.chain.lock().await;
        let mempool = state.mempool.lock().await;
        crate::cold_stake_attestation::build_attestation(
            &chain,
            &mempool,
            txid,
            req.vout,
            &network,
            now,
            crate::cold_stake_attestation::DEFAULT_MIN_CONFIRMATIONS,
        )
        .map_err(|refusal| {
            RpcError::BadRequest(format!(
                "Cannot attest {}:{} — {}",
                req.txid,
                req.vout,
                refusal_reason(refusal)
            ))
        })?
    };
    attestation
        .sign(&key)
        .map_err(|error| RpcError::Internal(format!("Attestation signing failed: {error}")))?;
    Ok(Json(attestation))
}

/// GET /api/v1/cold-stake/funding-attestation/pubkey
pub async fn get_funding_attestation_pubkey(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Value>> {
    let path = state.cold_stake_attestor_key_path.as_ref().ok_or_else(|| {
        RpcError::BadRequest("Cold-stake funding attestation is not configured on this node".into())
    })?;
    let key = crate::cold_stake_attestor::load_or_create_attestor_key(path)
        .map_err(|error| RpcError::Internal(format!("Attestor key unavailable: {error}")))?;
    Ok(Json(json!({
        "pubkey": crate::cold_stake_attestor::attestor_pubkey_hex(&key)
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_node::chain::Chain;

    const ADDRESS: &str = "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k";

    fn funded_state() -> (AppState, [u8; 32]) {
        let mut chain = Chain::new_regtest().unwrap();
        let txid = chain.mint_to_address(ADDRESS, 100_000_000).unwrap();
        for _ in 0..5 {
            chain.mint_to_address(ADDRESS, 1).unwrap();
        }
        let state = AppState::new();
        *state.chain.try_lock().unwrap() = chain;
        (state, txid)
    }

    fn temp_path() -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "vtr-attestation-endpoint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("cold-stake-attestor.key"))
    }

    #[tokio::test]
    async fn the_endpoint_refuses_when_no_attestor_path_is_configured() {
        let (state, txid) = funded_state();
        let error = get_funding_attestation(
            State(Arc::new(state)),
            Json(FundingAttestationRequest {
                txid: hex::encode(txid),
                vout: 0,
            }),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not configured"));
    }

    #[tokio::test]
    async fn the_endpoint_signs_and_the_pubkey_endpoint_matches() {
        let (mut state, txid) = funded_state();
        let (dir, path) = temp_path();
        state.cold_stake_attestor_key_path = Some(path);
        let state = Arc::new(state);

        let Json(attestation) = get_funding_attestation(
            State(Arc::clone(&state)),
            Json(FundingAttestationRequest {
                txid: hex::encode(txid),
                vout: 0,
            }),
        )
        .await
        .unwrap();
        let Json(pubkey) = get_funding_attestation_pubkey(State(Arc::clone(&state)))
            .await
            .unwrap();
        let pinned = pubkey["pubkey"].as_str().unwrap();
        attestation.verify_signature(pinned).unwrap();
        assert_eq!(attestation.txid, hex::encode(txid));
        assert_eq!(attestation.confirmations, 6);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn the_endpoint_reports_a_refusal_reason() {
        let (mut state, _) = funded_state();
        let (dir, path) = temp_path();
        state.cold_stake_attestor_key_path = Some(path);
        let error = get_funding_attestation(
            State(Arc::new(state)),
            Json(FundingAttestationRequest {
                txid: hex::encode([9u8; 32]),
                vout: 0,
            }),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("transaction is unknown"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
