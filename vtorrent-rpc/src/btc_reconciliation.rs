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
    // Record the preimage revealed by a confirmed maker claim in the swap
    // state (never in the observation, which must stay secret-free). This is
    // how a taker on a different node learns the secret and can then call
    // `vtr-claim` without supplying it out of band.
    if observation.state == vtorrent_node::atomic_swap::BtcSettlementState::Claimed {
        if let Some(preimage) = evidence.preimage {
            updated.preimage = Some(preimage);
        }
    }
    crate::swap_recovery::persist_for_wallet(
        state,
        &order,
        Some(&updated),
        wallet_identity.as_ref().map(|wif| wif.as_str()),
    )
    .await?;
    current.btc_observation = Some(observation.clone());
    current.preimage = updated.preimage;
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

/// Whether a persisted BTC reservation is a release candidate.
///
/// Requires a tracked input and an expired HTLC. Confirmation status is checked
/// separately against a fresh scan.
fn is_release_candidate(contract: &vtorrent_btc::utxo::SwapContract, now: u64) -> bool {
    contract.input_txid.is_some() && u64::from(contract.expiry) < now
}

/// Whether a fresh scan proves the funding transaction is not confirmed, so the
/// reservation can be released.
fn scan_proves_unfunded(scan: &SwapScan) -> bool {
    scan.funding.is_none()
}

/// Release BTC input reservations whose funding transaction can no longer be
/// claimed.
///
/// A reservation is only released when **both** hold:
/// 1. The HTLC has expired (`now > contract.expiry`). After expiry the maker
///    can no longer claim, so a funding tx that later confirms can only be
///    refunded by us — it can never be claimed out from under a new swap.
/// 2. A fresh scan reports the funding tx as not observed (`FundingNotObserved`
///    or `InvalidFunding`), so it is not confirmed in the chain.
///
/// The scan is BIP-158 (confirmed-only), so condition 2 does not prove absence
/// from a mempool; condition 1 is what makes release safe regardless.
///
/// Returns the number of reservations released. Failures on individual
/// contracts are logged and skipped; they are retried on the next call.
pub async fn release_expired_reservations(state: &AppState) -> usize {
    let now = crate::handlers::now_secs_mock(state).await;
    let network = *state.btc_network.read().await;
    let candidates: Vec<vtorrent_btc::utxo::SwapContract> = {
        let btc = state.btc_wallet.read().await;
        let Some(wallet) = btc.as_ref() else {
            return 0;
        };
        wallet
            .swap_contracts()
            .into_iter()
            .filter(|c| is_release_candidate(c, now))
            .collect()
    };
    let mut released = 0;
    for contract in candidates {
        let htlc = BtcHtlc {
            hash_lock: contract.hash_lock,
            recipient: contract.recipient.clone(),
            refund_address: contract.refund_address.clone(),
            expiry: contract.expiry,
            amount: contract.amount,
            network,
        };
        let funding_txid = match bitcoin::Txid::from_str(&contract.funding_txid) {
            Ok(txid) => txid.to_byte_array(),
            Err(_) => {
                tracing::warn!(
                    order_id = %contract.order_id,
                    "BTC reservation has an invalid funding txid; not releasing"
                );
                continue;
            }
        };
        let scan = match scan_contract(state, &htlc, funding_txid).await {
            Ok(scan) => scan,
            Err(error) => {
                tracing::debug!(
                    order_id = %contract.order_id,
                    error = %error,
                    "BTC reservation release scan failed; will retry"
                );
                continue;
            }
        };
        // Only release when the funding tx is provably not confirmed.
        if !scan_proves_unfunded(&scan) {
            tracing::info!(
                order_id = %contract.order_id,
                "BTC funding is confirmed; keeping reservation for refund"
            );
            continue;
        }
        let btc = state.btc_wallet.read().await;
        let Some(wallet) = btc.as_ref() else {
            break;
        };
        match wallet.release_swap_reservation(&contract.order_id) {
            Ok(true) => {
                released += 1;
                tracing::info!(
                    order_id = %contract.order_id,
                    "Released expired BTC input reservation"
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    order_id = %contract.order_id,
                    error = %error,
                    "Could not persist BTC reservation release"
                );
            }
        }
    }
    released
}

/// Run a BTC settlement scan for a contract, resolving peers the same way the
/// RPC reconcile path does.
async fn scan_contract(
    state: &AppState,
    htlc: &BtcHtlc,
    funding_txid: [u8; 32],
) -> RpcResult<SwapScan> {
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
    let wallet = btc
        .as_ref()
        .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
    let scan = wallet
        .observe_swap(
            htlc,
            funding_txid,
            &peers,
            crate::handlers::now_secs_mock(state).await,
            &[],
        )
        .await
        .map_err(|e| RpcError::BadRequest(format!("BTC settlement scan failed: {e}")))?;
    Ok(scan)
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
            preimage: None,
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

    fn contract(expiry: u32, input: Option<&str>) -> vtorrent_btc::utxo::SwapContract {
        vtorrent_btc::utxo::SwapContract {
            order_id: "order".into(),
            funding_txid: "ab".repeat(32),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry,
            amount: 1000,
            refund_raw: None,
            input_txid: input.map(str::to_string),
            input_vout: input.map(|_| 0),
        }
    }

    #[test]
    fn release_candidate_requires_expiry_and_tracked_input() {
        // Expired + tracked input: a candidate.
        assert!(is_release_candidate(&contract(100, Some("cd")), 101));
        // Not yet expired: never released.
        assert!(!is_release_candidate(&contract(100, Some("cd")), 100));
        assert!(!is_release_candidate(&contract(100, Some("cd")), 99));
        // No tracked input (legacy contract): never released.
        assert!(!is_release_candidate(&contract(100, None), 101));
    }

    #[test]
    fn release_requires_unconfirmed_funding() {
        let mut scan = SwapScan {
            tip_hash: [8; 32],
            tip_height: 20,
            scan_start: 1,
            funding: None,
            spend: None,
            invalid_funding: false,
            coinbase: false,
            invalidated_anchor: false,
            preimage: None,
        };
        // Funding not observed: safe to release.
        assert!(scan_proves_unfunded(&scan));
        // Funding confirmed: must keep the reservation for refund.
        scan.funding = Some(SwapAnchor {
            txid: [4; 32],
            block_hash: [5; 32],
            height: 16,
        });
        assert!(!scan_proves_unfunded(&scan));
    }
}
