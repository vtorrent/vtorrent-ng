use crate::error::{RpcError, RpcResult};
use crate::state::AppState;
use axum::{extract::State, Json};
use bitcoin::hashes::Hash;
use std::{str::FromStr, sync::Arc};
use vtorrent_btc::{
    htlc::BtcHtlc,
    sync::{SwapAnchor, SwapScan},
};
use vtorrent_node::atomic_swap::{
    BtcSettlementState, BtcSwapObservation, SwapConfirmation, SwapOrder, SwapState,
};

pub async fn reconcile_btc_swap(
    State(state): State<Arc<AppState>>,
    Json(req): Json<crate::models::BtcClaimRequest>,
) -> RpcResult<Json<BtcSwapObservation>> {
    reconcile(&state, &req.order_id).await.map(Json)
}

/// Request a fresh BTC scan. This neither signs transactions nor releases input reservations.
pub async fn reconcile(state: &AppState, order_id: &str) -> RpcResult<BtcSwapObservation> {
    reconcile_with_scan(state, order_id, async |htlc, txid, previous| {
        let scan = async {
            let peers = if let Some(host) = state.btc_peer.read().await.clone() {
                tokio::net::lookup_host(host)
                    .await
                    .map_err(|e| RpcError::BadRequest(format!("BTC peer resolution failed: {e}")))?
                    .collect::<Vec<_>>()
            } else if htlc.network == bitcoin::Network::Bitcoin {
                vtorrent_btc::sync::resolve_seeds()
                    .await
                    .map_err(|e| RpcError::BadRequest(e.to_string()))?
            } else {
                return Err(RpcError::BadRequest(
                    "Configure a BTC peer for this network".into(),
                ));
            };
            let btc = state.btc_wallet.read().await;
            btc.as_ref()
                .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?
                .observe_swap(
                    &htlc,
                    txid,
                    &peers,
                    crate::handlers::now_secs_mock(state).await,
                    &previous,
                )
                .await
                .map_err(|e| RpcError::BadRequest(format!("BTC settlement scan failed: {e}")))
        };
        tokio::time::timeout(std::time::Duration::from_secs(120), scan)
            .await
            .map_err(|_| RpcError::BadRequest("BTC settlement scan timed out".into()))?
    })
    .await
}

fn contract(
    order: &SwapOrder,
    swap: &SwapState,
    network: bitcoin::Network,
) -> RpcResult<(BtcHtlc, [u8; 32])> {
    if order.order_id != swap.order_id
        || order.hash_lock != Some(swap.hash_lock)
        || order.funding_txid != swap.vtr_funding_txid
        || order.maker_btc_address != swap.maker_btc_address
        || order.target_asset != "BTC"
        || order.target_amount != swap.btc_amount
        || swap.btc_amount == 0
        || swap.btc_expiry < 500_000_000
        || swap
            .btc_expiry
            .checked_add(vtorrent_wallet_service::swap_policy::SWAP_CLAIM_SAFETY_MARGIN)
            .is_none_or(|expiry| expiry > order.expiry)
    {
        return Err(RpcError::BadRequest(
            "BTC recovery terms do not match the order".into(),
        ));
    }
    let missing = || RpcError::BadRequest("BTC funding contract metadata missing".into());
    let htlc = BtcHtlc {
        hash_lock: swap.hash_lock,
        recipient: swap.maker_btc_address.clone().ok_or_else(missing)?,
        refund_address: swap.taker_btc_refund_address.clone().ok_or_else(missing)?,
        expiry: swap.btc_expiry,
        amount: swap.btc_amount,
        network,
    };
    htlc.build_script()
        .map_err(|e| RpcError::BadRequest(e.to_string()))?;
    Ok((htlc, swap.btc_funding_txid.ok_or_else(missing)?))
}

fn previous_anchors(swap: &SwapState, network: bitcoin::Network) -> RpcResult<Vec<SwapAnchor>> {
    let Some(old) = &swap.btc_observation else {
        return Ok(vec![]);
    };
    if old.network != network.to_string() {
        return Err(RpcError::BadRequest(
            "BTC observation network mismatch".into(),
        ));
    }
    [&old.funding, &old.spend]
        .into_iter()
        .flatten()
        .map(|anchor| {
            Ok(SwapAnchor {
                txid: bitcoin::Txid::from_str(&anchor.txid)
                    .map_err(|_| RpcError::BadRequest("Invalid BTC transaction anchor".into()))?
                    .to_byte_array(),
                block_hash: bitcoin::BlockHash::from_str(&anchor.block_hash)
                    .map_err(|_| RpcError::BadRequest("Invalid BTC block anchor".into()))?
                    .to_byte_array(),
                height: anchor.height,
            })
        })
        .collect()
}

pub(crate) async fn reconcile_with_scan(
    state: &AppState,
    order_id: &str,
    scan: impl AsyncFnOnce(BtcHtlc, [u8; 32], Vec<SwapAnchor>) -> RpcResult<SwapScan>,
) -> RpcResult<BtcSwapObservation> {
    let _scan_guard = state
        .btc_swap_scan_lock
        .try_lock()
        .map_err(|_| RpcError::BadRequest("A BTC settlement scan is already running".into()))?;
    if state.swap_recovery_dir.is_some() && !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    let wallet_identity = state.wallet_wif.read().await.clone();
    if state.swap_recovery_dir.is_some() && wallet_identity.is_none() {
        return Err(RpcError::WalletLocked);
    }
    let order = state
        .order_book
        .read()
        .await
        .get_order(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order not found".into()))?;
    let original = state
        .swaps
        .read()
        .await
        .get(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    let network = *state.btc_network.read().await;
    let (htlc, txid) = contract(&order, &original, network)?;
    let previous = previous_anchors(&original, network)?;
    let evidence = scan(htlc.clone(), txid, previous).await?;

    let order = state
        .order_book
        .read()
        .await
        .get_order(order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order disappeared".into()))?;
    let mut swaps = state.swaps.write().await;
    let current = swaps
        .get_mut(order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state disappeared".into()))?;
    let (fresh, fresh_txid) = contract(&order, current, *state.btc_network.read().await)?;
    if fresh != htlc || fresh_txid != txid || current.btc_observation != original.btc_observation {
        return Err(RpcError::BadRequest(
            "BTC recovery terms changed during scan; retry".into(),
        ));
    }
    let observation = observe(
        &evidence,
        current,
        network,
        crate::handlers::now_secs_mock(state).await,
    );
    let mut updated = current.clone();
    updated.btc_observation = Some(observation.clone());
    crate::swap_recovery::persist_for_wallet(
        state,
        &order,
        Some(&updated),
        wallet_identity.as_ref().map(|wif| wif.as_str()),
    )
    .await?;
    current.btc_observation = Some(observation.clone());
    Ok(observation)
}

fn observe(
    scan: &SwapScan,
    swap: &SwapState,
    network: bitcoin::Network,
    now: u64,
) -> BtcSwapObservation {
    use BtcSettlementState::*;
    let anchor = |a: &SwapAnchor| SwapConfirmation {
        txid: bitcoin::Txid::from_byte_array(a.txid).to_string(),
        block_hash: bitcoin::BlockHash::from_byte_array(a.block_hash).to_string(),
        height: a.height,
        confirmations: scan.tip_height.saturating_sub(a.height).saturating_add(1),
    };
    let funding = scan.funding.as_ref().map(anchor);
    let spend = scan.spend.as_ref().map(anchor);
    let settled = spend.as_ref().is_some_and(|a| a.confirmations >= 6);
    let state = if scan.invalid_funding {
        InvalidFunding
    } else if funding.is_none() {
        FundingNotObserved
    } else if let Some(spend) = &scan.spend {
        if Some(spend.txid) == swap.btc_claim_txid {
            if settled {
                Claimed
            } else {
                ClaimConfirming
            }
        } else if Some(spend.txid) == swap.btc_refund_txid {
            if settled {
                Refunded
            } else {
                RefundConfirming
            }
        } else {
            SpentElsewhere
        }
    } else if scan.coinbase && funding.as_ref().is_some_and(|a| a.confirmations < 100) {
        FundingImmature
    } else if funding.as_ref().is_some_and(|a| a.confirmations >= 6) {
        Funded
    } else {
        FundingConfirming
    };
    BtcSwapObservation {
        network: network.to_string(),
        observed_at: now,
        tip_hash: bitcoin::BlockHash::from_byte_array(scan.tip_hash).to_string(),
        tip_height: scan.tip_height,
        scan_start: scan.scan_start,
        state,
        funding,
        spend,
        reorg_count: swap
            .btc_observation
            .as_ref()
            .map(|o| o.reorg_count)
            .unwrap_or(0)
            .saturating_add(u32::from(scan.invalidated_anchor)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btc_observation_classifies_depth_maturity_and_unknown_spends() {
        use BtcSettlementState::*;
        let mut swap = SwapState::new([1; 32], [2; 32]);
        swap.btc_claim_txid = Some([3; 32]);
        let mut scan = SwapScan {
            tip_hash: [8; 32],
            tip_height: 20,
            scan_start: 1,
            funding: Some(SwapAnchor {
                txid: [4; 32],
                block_hash: [5; 32],
                height: 16,
            }),
            spend: None,
            invalid_funding: false,
            coinbase: false,
            invalidated_anchor: false,
        };
        let observe =
            |scan: &SwapScan| super::observe(scan, &swap, bitcoin::Network::Regtest, 1_800_000_000);
        assert_eq!(observe(&scan).state, FundingConfirming);
        scan.tip_height = 21;
        assert_eq!(observe(&scan).state, Funded);
        scan.coinbase = true;
        assert_eq!(observe(&scan).state, FundingImmature);
        scan.tip_height = 115;
        assert_eq!(observe(&scan).state, Funded);
        scan.invalid_funding = true;
        assert_eq!(observe(&scan).state, InvalidFunding);
        scan.invalid_funding = false;
        scan.spend = Some(SwapAnchor {
            txid: [3; 32],
            block_hash: [6; 32],
            height: 115,
        });
        assert_eq!(observe(&scan).state, ClaimConfirming);
        scan.tip_height = 120;
        assert_eq!(observe(&scan).state, Claimed);
        scan.spend.as_mut().unwrap().txid = [7; 32];
        assert_eq!(observe(&scan).state, SpentElsewhere);
        scan.funding = None;
        assert_eq!(observe(&scan).state, FundingNotObserved);
        assert!(observe(&scan).spend.is_some());
    }
}
