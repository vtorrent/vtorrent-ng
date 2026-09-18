//! BTC claim fee bumping (RBF).
//!
//! The BTC HTLC claim reveals the preimage, so if it stalls the maker must be
//! able to replace it with a higher fee before the taker's refund becomes
//! eligible. The claim is built with `Sequence::ENABLE_RBF_NO_LOCKTIME`, so a
//! replacement is accepted by BIP-125; the refund uses
//! `ENABLE_LOCKTIME_NO_RBF` (it needs CLTV) and therefore cannot replace the
//! claim.

use crate::error::{RpcError, RpcResult};
use crate::models::{BtcClaimBumpRequest, BtcClaimBumpResponse};
use crate::state::AppState;
use axum::{extract::State, Json};
use std::sync::Arc;
use vtorrent_node::atomic_swap::BtcClaimReplacement;

/// Maximum number of claim replacements per swap. Bounds state growth and the
/// fee-escalation loop.
pub const MAX_BTC_CLAIM_REPLACEMENTS: usize = 8;

pub async fn bump_btc_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcClaimBumpRequest>,
) -> RpcResult<Json<BtcClaimBumpResponse>> {
    bump_with_broadcast(&state, req, async |raw: &[u8]| {
        crate::handlers::broadcast_btc(&state, raw).await
    })
    .await
    .map(Json)
}

pub async fn bump(state: &AppState, req: BtcClaimBumpRequest) -> RpcResult<BtcClaimBumpResponse> {
    bump_with_broadcast(state, req, async |raw: &[u8]| {
        crate::handlers::broadcast_btc(state, raw).await
    })
    .await
}

pub async fn bump_with_broadcast(
    state: &AppState,
    req: BtcClaimBumpRequest,
    broadcast: impl AsyncFnOnce(&[u8]) -> RpcResult<[u8; 32]>,
) -> RpcResult<BtcClaimBumpResponse> {
    if !req.approve {
        return Err(RpcError::BadRequest(
            "Explicit approval of the total fee is required".into(),
        ));
    }
    if state.swap_recovery_dir.is_none() {
        return Err(RpcError::BadRequest(
            "Durable encrypted swap recovery must be enabled".into(),
        ));
    }
    let parent = crate::handlers::parse_hash32(&req.replaces_txid, "replaces_txid")?;
    let order = state
        .order_book
        .read()
        .await
        .get_order(&req.order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order not found".into()))?;
    let now = crate::handlers::now_secs_mock(state).await;
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;

    // Only a claim that has actually been submitted can be bumped, and only
    // while the claim window is still safely open — a replacement after the
    // refund becomes eligible cannot win the race.
    if swap.btc_claim_txid.is_none()
        || swap.btc_refund_txid.is_some()
        || swap.vtr_refund_txid.is_some()
        || order.hash_lock != Some(swap.hash_lock)
    {
        return Err(RpcError::BadRequest(
            "BTC claim is ineligible for replacement".into(),
        ));
    }
    if now.saturating_add(u64::from(
        vtorrent_wallet_service::swap_policy::MIN_BTC_SWAP_WINDOW,
    )) >= u64::from(swap.btc_expiry)
    {
        return Err(RpcError::BadRequest(
            "BTC claim window is too close to refund eligibility to replace safely".into(),
        ));
    }

    // Idempotent retry: the same replacement for the same parent and fee is
    // returned as-is rather than rebuilt.
    if let Some(retry) = swap
        .btc_claim_replacements
        .iter()
        .find(|r| r.replaces_txid == parent && r.total_fee_satoshis == req.total_fee_satoshis)
    {
        if swap.latest_btc_claim().map(|r| r.replaces_txid) != Some(retry.replaces_txid) {
            return Err(RpcError::BadRequest(
                "This replacement has already been superseded".into(),
            ));
        }
        return Ok(BtcClaimBumpResponse {
            order_id: req.order_id,
            txid: hex::encode(retry.replaces_txid),
            replaces_txid: hex::encode(parent),
            total_fee_satoshis: retry.total_fee_satoshis,
            status: "BtcClaimReplaced".into(),
        });
    }

    // The parent must be the current tip of the replacement chain.
    let current_txid = swap
        .btc_claim_replacements
        .last()
        .map(|r| r.replaces_txid)
        .or(swap.btc_claim_txid)
        .ok_or_else(|| RpcError::BadRequest("No BTC claim to replace".into()))?;
    if current_txid != parent {
        return Err(RpcError::BadRequest("Stale claim txid".into()));
    }
    if swap.btc_claim_replacements.len() >= MAX_BTC_CLAIM_REPLACEMENTS {
        return Err(RpcError::BadRequest(
            "BTC claim replacement limit reached".into(),
        ));
    }

    // BIP-125 rule 4: the replacement must pay a strictly higher absolute fee.
    let previous_fee = previous_claim_fee(swap)?;
    if req.total_fee_satoshis <= previous_fee {
        return Err(RpcError::BadRequest(
            "Replacement total fee must strictly increase".into(),
        ));
    }

    let preimage = swap
        .preimage
        .or(order.preimage)
        .ok_or_else(|| RpcError::BadRequest("Preimage not available".into()))?;
    let funding_txid = swap
        .btc_funding_txid
        .ok_or_else(|| RpcError::BadRequest("BTC funding txid not recorded".into()))?;
    let maker_btc_address = swap
        .maker_btc_address
        .clone()
        .ok_or_else(|| RpcError::BadRequest("Maker BTC address not recorded".into()))?;
    let refund_address = swap
        .taker_btc_refund_address
        .clone()
        .ok_or_else(|| RpcError::BadRequest("Taker BTC refund address not recorded".into()))?;
    let (btc_amount, btc_expiry) = (swap.btc_amount, swap.btc_expiry);
    let network = *state.btc_network.read().await;

    let (raw, txid) = {
        let btc = state.btc_wallet.read().await;
        let wallet = btc
            .as_ref()
            .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
        vtorrent_wallet_service::build_btc_htlc_claim_with_fee(
            wallet,
            vtorrent_wallet_service::BtcClaimParams {
                funding_txid,
                preimage,
                maker_btc_address: &maker_btc_address,
                refund_address: &refund_address,
                expiry: btc_expiry,
                amount: btc_amount,
                network,
            },
            req.total_fee_satoshis,
        )
        .map_err(RpcError::BadRequest)?
    };

    swap.btc_claim_replacements.push(BtcClaimReplacement {
        replaces_txid: txid,
        total_fee_satoshis: req.total_fee_satoshis,
        approved_at: now,
        raw: raw.clone(),
    });
    swap.btc_claim_raw = Some(raw.clone());
    swap.btc_claim_txid = Some(txid);
    swap.refresh_status();
    crate::swap_recovery::persist(state, &order, Some(swap)).await?;

    broadcast(&raw).await?;

    Ok(BtcClaimBumpResponse {
        order_id: req.order_id,
        txid: hex::encode(txid),
        replaces_txid: hex::encode(parent),
        total_fee_satoshis: req.total_fee_satoshis,
        status: "BtcClaimReplaced".into(),
    })
}

/// Recover the fee paid by the current BTC claim from its single output.
fn previous_claim_fee(swap: &vtorrent_node::atomic_swap::SwapState) -> RpcResult<u64> {
    let raw = swap
        .btc_claim_raw
        .as_ref()
        .ok_or_else(|| RpcError::BadRequest("No recorded BTC claim to price".into()))?;
    let tx: bitcoin::Transaction = bitcoin::consensus::deserialize(raw)
        .map_err(|_| RpcError::Internal("Recorded BTC claim is corrupted".into()))?;
    let output = tx
        .output
        .first()
        .filter(|_| tx.output.len() == 1)
        .ok_or_else(|| RpcError::BadRequest("Invalid BTC claim shape".into()))?;
    swap.btc_amount
        .checked_sub(output.value.to_sat())
        .ok_or_else(|| RpcError::BadRequest("Invalid BTC claim amount".into()))
}
