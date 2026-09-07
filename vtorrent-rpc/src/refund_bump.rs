use crate::error::{RpcError, RpcResult};
use crate::models::{VtrRefundBumpRequest, VtrRefundBumpResponse};
use crate::state::AppState;
use axum::{extract::State, Json};
use std::sync::Arc;
use vtorrent_node::{
    atomic_swap::{SwapState, VtrRefundReplacement},
    block::Transaction,
};

pub async fn bump_vtr_refund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VtrRefundBumpRequest>,
) -> RpcResult<Json<VtrRefundBumpResponse>> {
    bump(&state, req).await.map(Json)
}

pub async fn get_vtr_refund_history(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(order_id): axum::extract::Path<String>,
) -> RpcResult<Json<crate::models::VtrRefundHistoryResponse>> {
    history(&state, &order_id).await.map(Json)
}

/// Read fee approval history without returning signed transactions or secret material.
pub async fn history(
    state: &AppState,
    order_id: &str,
) -> RpcResult<crate::models::VtrRefundHistoryResponse> {
    use crate::models::{VtrRefundHistoryResponse, VtrRefundVersion};
    let order = state
        .order_book
        .read()
        .await
        .get_order(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order not found".into()))?;
    let swaps = state.swaps.read().await;
    let swap = swaps
        .get(order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    crate::refund_lineage::validate(&order, swap)?;
    let tx = swap
        .vtr_refund_tx
        .as_ref()
        .ok_or_else(|| RpcError::NotFound("No prepared VTR refund".into()))?;
    let fee = tx
        .outputs
        .first()
        .filter(|_| tx.outputs.len() == 1)
        .and_then(|output| order.vtr_amount.checked_sub(output.value))
        .ok_or_else(|| RpcError::BadRequest("Invalid original refund amount".into()))?;
    let mut versions = vec![VtrRefundVersion {
        txid: hex::encode(tx.txid()),
        replaces_txid: None,
        total_fee_satoshis: fee,
        approved_at: None,
    }];
    versions.extend(
        swap.vtr_refund_replacements
            .iter()
            .map(|r| VtrRefundVersion {
                txid: hex::encode(r.transaction.txid()),
                replaces_txid: Some(hex::encode(r.replaces_txid)),
                total_fee_satoshis: r.total_fee_satoshis,
                approved_at: Some(r.approved_at),
            }),
    );
    Ok(VtrRefundHistoryResponse {
        order_id: order_id.into(),
        versions,
    })
}

fn check_conflicts(mempool: &vtorrent_node::mempool::Mempool, swap: &SwapState) -> RpcResult<()> {
    let funding = swap
        .vtr_funding_txid
        .ok_or_else(|| RpcError::BadRequest("Missing VTR funding".into()))?;
    for entry in mempool.get_entries() {
        for input in &entry.tx.inputs {
            if input.prev_txid == funding
                && input.prev_vout == 0
                && !swap.is_vtr_refund(&entry.tx.txid())
            {
                return Err(RpcError::BadRequest(
                    "A competing transaction spends the VTR contract".into(),
                ));
            }
            if swap.is_vtr_refund(&input.prev_txid) {
                return Err(RpcError::BadRequest(
                    "A pending transaction spends this refund; replacement is unsafe".into(),
                ));
            }
        }
    }
    Ok(())
}

fn confirmed_refund(chain: &vtorrent_node::chain::Chain, swap: &SwapState) -> Option<[u8; 32]> {
    swap.vtr_refund_txid
        .into_iter()
        .chain(
            swap.vtr_refund_replacements
                .iter()
                .map(|r| r.transaction.txid()),
        )
        .find(|id| chain.get_transaction(id).is_some())
}

/// Retry the latest approved refund, or recognize an earlier version that confirmed.
pub(crate) async fn submit_latest(state: &AppState, swap: &SwapState) -> RpcResult<[u8; 32]> {
    let chain = state.chain.lock().await;
    if let Some(txid) = confirmed_refund(&chain, swap) {
        return Ok(txid);
    }
    let tx = swap
        .latest_vtr_refund()
        .ok_or_else(|| RpcError::BadRequest("Signed VTR refund missing".into()))?;
    let mut mempool = state.mempool.lock().await;
    check_conflicts(&mempool, swap)?;
    mempool
        .admit_with_chain_fee(&chain, tx.clone())
        .map_err(|e| RpcError::BadRequest(e.to_string()))?;
    if let Some(sender) = &state.tx_submit {
        sender.try_send(tx.clone()).map_err(|_| RpcError::BadRequest(
            "Refund saved and admitted locally, but relay is unavailable; retry the identical request".into()
        ))?;
    }
    Ok(tx.txid())
}

async fn preflight(
    state: &AppState,
    swap: &SwapState,
    tx: &Transaction,
    approved_fee: u64,
) -> RpcResult<()> {
    let chain = state.chain.lock().await;
    if let Some(txid) = confirmed_refund(&chain, swap) {
        return Err(RpcError::BadRequest(format!(
            "A refund already confirmed: {}",
            hex::encode(txid)
        )));
    }
    let fee = chain
        .compute_tx_fee(tx)
        .ok_or_else(|| RpcError::BadRequest("Refund funding is missing or spent".into()))?;
    if fee != approved_fee {
        return Err(RpcError::BadRequest(
            "Chain-verified refund fee differs from the approved total".into(),
        ));
    }
    let rate = fee / tx.serialized_size().max(1) as u64;
    for previous in swap
        .vtr_refund_tx
        .iter()
        .chain(swap.vtr_refund_replacements.iter().map(|r| &r.transaction))
    {
        let old_fee = chain
            .compute_tx_fee(previous)
            .ok_or_else(|| RpcError::BadRequest("Previous refund inputs are invalid".into()))?;
        let required = (old_fee / previous.serialized_size().max(1) as u64)
            .saturating_add(vtorrent_node::mempool::MIN_RBF_FEE_BUMP);
        if rate < required {
            return Err(RpcError::BadRequest(
                "Approved fee does not increase the fee rate enough to replace saved refunds"
                    .into(),
            ));
        }
    }
    let mempool = state.mempool.lock().await;
    check_conflicts(&mempool, swap)?;
    mempool
        .clone()
        .admit_with_chain_fee(&chain, tx.clone())
        .map_err(|e| RpcError::BadRequest(e.to_string()))?;
    Ok(())
}

/// Append a fee-approved VTR refund replacement before local admission or relay.
pub async fn bump(state: &AppState, req: VtrRefundBumpRequest) -> RpcResult<VtrRefundBumpResponse> {
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
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    let maker_wif = state
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or(RpcError::WalletLocked)?;
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
    if now < u64::from(order.expiry)
        || swap.vtr_claim_txid.is_some()
        || order.order_id != swap.order_id
        || order.hash_lock != Some(swap.hash_lock)
        || order.funding_txid != swap.vtr_funding_txid
    {
        return Err(RpcError::BadRequest(
            "VTR refund is ineligible or its metadata disagrees".into(),
        ));
    }
    crate::refund_lineage::validate(&order, swap)?;
    let retry = swap
        .vtr_refund_replacements
        .iter()
        .find(|r| r.replaces_txid == parent && r.total_fee_satoshis == req.total_fee_satoshis);
    if let Some(retry) = retry {
        if swap.latest_vtr_refund().map(Transaction::txid) != Some(retry.transaction.txid()) {
            return Err(RpcError::BadRequest(
                "This replacement has already been superseded".into(),
            ));
        }
    } else {
        let latest = swap.latest_vtr_refund().ok_or_else(|| {
            RpcError::BadRequest("Prepare a VTR refund before bumping its fee".into())
        })?;
        if latest.txid() != parent
            || swap.vtr_refund_replacements.len() >= crate::refund_lineage::MAX_REPLACEMENTS
        {
            return Err(RpcError::BadRequest(
                "Stale refund ID or replacement limit reached".into(),
            ));
        }
        let previous_fee = latest
            .outputs
            .first()
            .filter(|_| latest.outputs.len() == 1)
            .and_then(|output| order.vtr_amount.checked_sub(output.value))
            .ok_or_else(|| RpcError::BadRequest("Invalid original refund amount".into()))?;
        if req.total_fee_satoshis <= previous_fee {
            return Err(RpcError::BadRequest(
                "Replacement total fee must strictly increase".into(),
            ));
        }
        let tx = vtorrent_wallet_service::build_vtr_htlc_refund_replacement(
            vtorrent_wallet_service::VtrRefundParams {
                hash_lock: swap.hash_lock,
                taker_address: order
                    .taker_address
                    .as_deref()
                    .ok_or_else(|| RpcError::BadRequest("Missing taker".into()))?,
                maker_address: &order.maker_address,
                expiry: order.expiry,
                vtr_amount: order.vtr_amount,
                funding_txid: swap
                    .vtr_funding_txid
                    .ok_or_else(|| RpcError::BadRequest("Missing funding".into()))?,
                maker_wif: &maker_wif,
            },
            req.total_fee_satoshis,
        )
        .map_err(RpcError::BadRequest)?;
        preflight(state, swap, &tx, req.total_fee_satoshis).await?;
        let mut updated = swap.clone();
        updated.vtr_refund_replacements.push(VtrRefundReplacement {
            replaces_txid: parent,
            total_fee_satoshis: req.total_fee_satoshis,
            approved_at: now,
            transaction: tx,
        });
        crate::swap_recovery::persist_for_wallet(state, &order, Some(&updated), Some(&maker_wif))
            .await?;
        *swap = updated;
    }
    let txid = submit_latest(state, swap).await?;
    let actual = swap
        .vtr_refund_replacements
        .iter()
        .find(|r| r.transaction.txid() == txid)
        .ok_or_else(|| {
            RpcError::BadRequest(format!(
                "The original refund confirmed: {}",
                hex::encode(txid)
            ))
        })?;
    Ok(VtrRefundBumpResponse {
        order_id: req.order_id,
        txid: hex::encode(txid),
        replaces_txid: hex::encode(actual.replaces_txid),
        total_fee_satoshis: actual.total_fee_satoshis,
        status: "VtrRefundRecordedOrSubmitted".into(),
    })
}
