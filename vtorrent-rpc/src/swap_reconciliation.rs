use crate::error::{RpcError, RpcResult};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use std::sync::Arc;
use vtorrent_node::atomic_swap::{
    Htlc, SwapConfirmation, SwapOrder, SwapState, VtrSettlementState, VtrSwapObservation,
};
use vtorrent_node::chain::Chain;
use vtorrent_node::mempool::Mempool;

#[derive(serde::Serialize)]
pub struct SwapStatusResponse {
    pub order_id: String,
    pub vtr: VtrSwapObservation,
    pub btc_reconciled: bool,
    pub btc: Option<vtorrent_node::atomic_swap::BtcSwapObservation>,
}

pub async fn get_swap_status(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> RpcResult<Json<SwapStatusResponse>> {
    let vtr = status(&state, &order_id).await?;
    Ok(Json(SwapStatusResponse {
        btc: state
            .swaps
            .read()
            .await
            .get(&order_id)
            .and_then(|swap| swap.btc_observation.clone()),
        order_id,
        vtr,
        btc_reconciled: false,
    }))
}

pub async fn reconcile_swap(
    State(state): State<Arc<AppState>>,
    Json(req): Json<crate::models::BtcClaimRequest>,
) -> RpcResult<Json<SwapStatusResponse>> {
    let vtr = reconcile(&state, &req.order_id).await?;
    Ok(Json(SwapStatusResponse {
        btc: state
            .swaps
            .read()
            .await
            .get(&req.order_id)
            .and_then(|swap| swap.btc_observation.clone()),
        order_id: req.order_id,
        vtr,
        btc_reconciled: false,
    }))
}

pub const SETTLEMENT_CONFIRMATIONS: u32 = 6;

fn confirmation(chain: &Chain, txid: &[u8; 32]) -> Option<SwapConfirmation> {
    let (_, block_hash, height) = chain.get_transaction(txid)?;
    Some(SwapConfirmation {
        txid: hex::encode(txid),
        block_hash: hex::encode(block_hash),
        height,
        confirmations: chain.best_height().saturating_sub(height).saturating_add(1),
    })
}

fn observes_spend(tx: &vtorrent_node::block::Transaction, funding: &[u8; 32]) -> bool {
    tx.inputs
        .iter()
        .any(|input| input.prev_txid == *funding && input.prev_vout == 0)
}

/// Derive settlement only from the active chain and current mempool, never submission flags.
pub fn observe_vtr(
    chain: &Chain,
    mempool: &Mempool,
    order: &SwapOrder,
    swap: &SwapState,
) -> VtrSwapObservation {
    use VtrSettlementState::*;
    let previous = swap.vtr_observation.as_ref();
    let invalidated_anchor = previous.is_some_and(|old| {
        [&old.funding, &old.spend]
            .into_iter()
            .flatten()
            .any(|anchor| {
                chain
                    .block_hash_at_height(anchor.height)
                    .map(hex::encode)
                    .as_deref()
                    != Some(anchor.block_hash.as_str())
            })
    });
    let mut observation = VtrSwapObservation {
        tip_hash: chain.best_hash().map(hex::encode).unwrap_or_default(),
        tip_height: chain.best_height(),
        state: NotFunded,
        funding: None,
        spend: None,
        pending_spend_txid: None,
        reorg_count: previous
            .map(|old| old.reorg_count)
            .unwrap_or(0)
            .saturating_add(u32::from(invalidated_anchor)),
    };
    if order.order_id != swap.order_id
        || order.hash_lock != Some(swap.hash_lock)
        || order.funding_txid != swap.vtr_funding_txid
    {
        observation.state = InvalidFunding;
        return observation;
    }
    let Some(funding_txid) = swap.vtr_funding_txid else {
        return observation;
    };
    let script = order
        .hash_lock
        .zip(order.taker_address.as_ref())
        .and_then(|(hash, taker)| {
            Htlc::with_expiry(
                hash,
                taker.clone(),
                order.maker_address.clone(),
                order.expiry,
                order.vtr_amount,
            )
            .ok()?
            .build_script()
            .ok()
        });
    if script.is_none() {
        observation.state = InvalidFunding;
        return observation;
    }
    let transaction = chain
        .get_transaction(&funding_txid)
        .map(|(tx, _, _)| tx)
        .or_else(|| mempool.get_transaction(&funding_txid));
    let Some(transaction) = transaction else {
        observation.state = if swap.vtr_funding_tx.is_some() {
            FundingPrepared
        } else {
            FundingMissing
        };
        return observation;
    };
    if !transaction.outputs.first().is_some_and(|output| {
        output.value == order.vtr_amount && Some(&output.script_pubkey) == script.as_ref()
    }) {
        observation.state = InvalidFunding;
        return observation;
    }
    let Some(funding) = confirmation(chain, &funding_txid) else {
        observation.state = FundingPending;
        return observation;
    };
    observation.funding = Some(funding.clone());
    if chain.get_utxo(&funding_txid, 0).is_none() {
        let recorded_spend = [swap.vtr_claim_txid, swap.vtr_refund_txid]
            .into_iter()
            .flatten()
            .chain(
                swap.vtr_refund_replacements
                    .iter()
                    .map(|r| r.transaction.txid()),
            )
            .find(|txid| {
                chain
                    .get_transaction(txid)
                    .is_some_and(|(tx, _, _)| observes_spend(tx, &funding_txid))
            });
        let spend = recorded_spend.or_else(|| {
            (funding.height..=chain.best_height())
                .rev()
                .find_map(|height| {
                    chain
                        .get_block_at_height(height)?
                        .transactions
                        .iter()
                        .find(|tx| observes_spend(tx, &funding_txid))
                        .map(|tx| tx.txid())
                })
        });
        if let Some(txid) = spend {
            observation.spend = confirmation(chain, &txid);
            let settled = observation
                .spend
                .as_ref()
                .is_some_and(|c| c.confirmations >= SETTLEMENT_CONFIRMATIONS);
            observation.state = if Some(txid) == swap.vtr_claim_txid {
                if settled {
                    Claimed
                } else {
                    ClaimConfirming
                }
            } else if swap.is_vtr_refund(&txid) {
                if settled {
                    Refunded
                } else {
                    RefundConfirming
                }
            } else {
                SpentElsewhere
            };
        } else {
            observation.state = SpendUnknown;
        }
        return observation;
    }
    if let Some(tx) = mempool
        .get_transactions()
        .iter()
        .find(|tx| observes_spend(tx, &funding_txid))
    {
        let txid = tx.txid();
        observation.pending_spend_txid = Some(hex::encode(txid));
        observation.state = if Some(txid) == swap.vtr_claim_txid {
            ClaimPending
        } else if swap.is_vtr_refund(&txid) {
            RefundPending
        } else {
            CompetingSpendPending
        };
    } else if swap.vtr_claim_tx.is_some() {
        observation.state = ClaimPrepared;
    } else if swap.vtr_refund_tx.is_some() {
        observation.state = RefundPrepared;
    } else {
        observation.state = if funding.confirmations >= SETTLEMENT_CONFIRMATIONS {
            Funded
        } else {
            FundingConfirming
        };
    }
    observation
}

/// Read current chain evidence without transmitting transactions or changing the journal.
pub async fn status(state: &AppState, order_id: &str) -> RpcResult<VtrSwapObservation> {
    let order = state
        .order_book
        .read()
        .await
        .get_order(order_id)
        .cloned()
        .ok_or_else(|| {
            RpcError::NotFound(
                "Swap order not found; unlock its wallet to restore recovery records".into(),
            )
        })?;
    let swap = state
        .swaps
        .read()
        .await
        .get(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    let chain = state.chain.lock().await;
    let mempool = state.mempool.lock().await;
    Ok(observe_vtr(&chain, &mempool, &order, &swap))
}

/// Persist a fresh observation without changing the original signed recovery transactions.
pub async fn reconcile(state: &AppState, order_id: &str) -> RpcResult<VtrSwapObservation> {
    let order = state
        .order_book
        .read()
        .await
        .get_order(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order not found".into()))?;
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .get_mut(order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    if order.order_id != swap.order_id
        || order.hash_lock != Some(swap.hash_lock)
        || order.funding_txid != swap.vtr_funding_txid
    {
        return Err(RpcError::BadRequest(
            "Order and swap recovery metadata disagree; use the read-only status check".into(),
        ));
    }
    let observation = {
        let chain = state.chain.lock().await;
        let mempool = state.mempool.lock().await;
        observe_vtr(&chain, &mempool, &order, swap)
    };
    let mut updated = swap.clone();
    updated.vtr_observation = Some(observation.clone());
    let changed = swap.vtr_observation.as_ref().is_none_or(|old| {
        let anchor = |confirmation: &Option<SwapConfirmation>| {
            confirmation
                .as_ref()
                .map(|c| (c.txid.clone(), c.block_hash.clone(), c.height))
        };
        old.state != observation.state
            || old.reorg_count != observation.reorg_count
            || old.pending_spend_txid != observation.pending_spend_txid
            || anchor(&old.funding) != anchor(&observation.funding)
            || anchor(&old.spend) != anchor(&observation.spend)
    });
    if changed {
        crate::swap_recovery::persist(state, &order, Some(&updated)).await?;
    }
    swap.vtr_observation = Some(observation.clone());
    Ok(observation)
}

/// Refresh local VTR evidence while unlocked; this never signs or transmits funds.
pub async fn run_reconciler(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if !state.is_wallet_unlocked().await {
            continue;
        }
        let ids: Vec<_> = state.swaps.read().await.keys().cloned().collect();
        for id in ids {
            if !state.is_wallet_unlocked().await {
                break;
            }
            if let Err(error) = reconcile(&state, &id).await {
                tracing::warn!(order_id = %id, error = %error, "VTR swap reconciliation failed");
            }
        }
    }
}
