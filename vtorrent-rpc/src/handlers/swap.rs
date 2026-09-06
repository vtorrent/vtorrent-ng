use axum::{extract::State, Json};
use std::sync::Arc;

use super::{broadcast_btc, btc_txid_hex, now_secs_mock, parse_hash32, verify_wallet_auth};
use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;
use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

pub async fn match_dex_order(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MatchOrderRequest>,
) -> RpcResult<Json<MatchOrderResponse>> {
    use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, MAX_HTLC_LOCKTIME, MIN_HTLC_LOCKTIME};
    use vtorrent_wallet::tx_builder::sign_custom_transaction;

    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.taker_address.trim().is_empty() {
        return Err(RpcError::BadRequest("Taker address is required".into()));
    }
    // Validate before any order state changes; the HTLC recipient must be a real
    // P2PKH-capable VTR address.
    vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&req.taker_address)
        .map_err(|e| RpcError::BadRequest(format!("Invalid taker address: {}", e)))?;

    // Re-verify the passphrase (and TOTP if 2FA is enabled) before signing.
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    let wallet_address = state
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcError::Internal("Change address not set".into()))?;

    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .filter(|order| matches!(order.status, vtorrent_node::atomic_swap::OrderStatus::Open))
            .cloned()
            .ok_or_else(|| {
                RpcError::NotFound(format!("Order {} not found or not open", req.order_id))
            })?
    };
    if order.maker_address != wallet_address {
        return Err(RpcError::Unauthorized(
            "Only the maker's imported wallet may fund this order".into(),
        ));
    }

    // Mock-clock aware (mirrors swap_refund): regtest tests set mock_time
    // and expect expiry math consistent with it.
    let now = now_secs_mock(&state).await as u32;
    let remaining_locktime = order.expiry.saturating_sub(now);
    if remaining_locktime < MIN_HTLC_LOCKTIME {
        return Err(RpcError::BadRequest(
            "DEX order is too close to expiry to fund safely".into(),
        ));
    }
    if remaining_locktime > MAX_HTLC_LOCKTIME {
        return Err(RpcError::BadRequest(
            "DEX order expiry exceeds maximum locktime".into(),
        ));
    }
    let (preimage, hash_lock) = match (order.preimage, order.hash_lock) {
        (Some(preimage), Some(hash_lock)) => (preimage, hash_lock),
        _ => {
            let swap = AtomicSwap::new();
            (swap.preimage, swap.hash_lock)
        }
    };
    let htlc = Htlc::with_expiry(
        hash_lock,
        req.taker_address.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| RpcError::BadRequest(format!("Unable to construct HTLC: {}", e)))?;

    // Reserve the order BEFORE building/signing the funding transaction: two
    // concurrent match calls would otherwise both select the same UTXO and
    // both admit competing funding txs to the mempool. Reserving first means
    // the loser exits here without touching the mempool.
    let reserved = state.order_book.write().await.begin_funding(&req.order_id);
    if reserved.is_none() {
        return Err(RpcError::NotFound(format!(
            "Order {} is no longer open",
            req.order_id
        )));
    }

    // Use a verified wallet UTXO large enough to fund this single-input HTLC.
    // A fixed 10,000-satoshi fee is intentionally conservative for the custom
    // script size and is recorded as an authoritative local mempool fee.
    const FUNDING_FEE_SATOSHIS: u64 = vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
    let funding_utxo = {
        let chain = state.chain.lock().await;
        chain
            .get_utxos_for_address(&wallet_address)
            .into_iter()
            .filter(|utxo| utxo.value >= order.vtr_amount.saturating_add(FUNDING_FEE_SATOSHIS))
            .max_by_key(|utxo| utxo.value)
    };
    let Some(funding_utxo) = funding_utxo else {
        state
            .order_book
            .write()
            .await
            .release_funding(&req.order_id);
        return Err(RpcError::BadRequest(
            "No single wallet UTXO can fund this HTLC".into(),
        ));
    };
    let unsigned_funding = htlc.build_funding_tx(
        funding_utxo.txid,
        funding_utxo.vout,
        funding_utxo.value,
        FUNDING_FEE_SATOSHIS,
    );
    let unsigned_funding = match unsigned_funding {
        Ok(tx) => tx,
        Err(e) => {
            state
                .order_book
                .write()
                .await
                .release_funding(&req.order_id);
            return Err(RpcError::BadRequest(format!(
                "Unable to build HTLC funding transaction: {}",
                e
            )));
        }
    };
    let funding_tx =
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), &wif);
    let funding_tx = match funding_tx {
        Ok(tx) => tx,
        Err(e) => {
            state
                .order_book
                .write()
                .await
                .release_funding(&req.order_id);
            return Err(RpcError::BadRequest(format!(
                "Unable to sign HTLC funding transaction: {}",
                e
            )));
        }
    };
    let funding_txid = funding_tx.txid();

    let admission = {
        let mut mempool = state.mempool.lock().await;
        mempool.add_transaction_with_fee(funding_tx.clone(), FUNDING_FEE_SATOSHIS)
    };
    if let Err(e) = admission {
        state
            .order_book
            .write()
            .await
            .release_funding(&req.order_id);
        return Err(RpcError::BadRequest(format!(
            "Mempool rejected HTLC funding transaction: {}",
            e
        )));
    }

    let matched = match state.order_book.write().await.fund_and_match_order(
        &req.order_id,
        req.taker_address,
        preimage,
        hash_lock,
        funding_txid,
    ) {
        Some(m) => m,
        None => {
            state
                .order_book
                .write()
                .await
                .release_funding(&req.order_id);
            return Err(RpcError::Internal(
                "Funding reservation disappeared before order completion".into(),
            ));
        }
    };

    let relayed = match &state.tx_submit {
        Some(sender) => sender.try_send(funding_tx).is_ok(),
        None => false,
    };
    tracing::info!(
        order_id = %req.order_id,
        funding_txid = %hex::encode(funding_txid),
        relayed,
        "DEX maker HTLC funding transaction accepted"
    );

    // Materialize the swap state in VtrFunded stage so lifecycle guards on
    // btc-fund / claims / refunds operate from a known baseline.
    {
        let mut swaps = state.swaps.write().await;
        let swap = swaps
            .entry(hex::encode(matched.order.order_id))
            .or_insert_with(|| SwapState::new(matched.order.order_id, matched.hash_lock));
        if swap.vtr_funding_txid.is_none() {
            swap.vtr_funding_txid = Some(funding_txid);
            swap.status = SwapStatus::VtrFunded;
        }
    }

    Ok(Json(MatchOrderResponse {
        order_id: hex::encode(matched.order.order_id),
        maker_address: matched.order.maker_address,
        vtr_amount: matched.order.vtr_amount,
        target_asset: matched.order.target_asset,
        target_amount: matched.order.target_amount,
        hash_lock: hex::encode(matched.hash_lock),
        expiry: matched.order.expiry,
        funding_txid: hex::encode(funding_txid),
    }))
}

pub async fn btc_fund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcFundRequest>,
) -> RpcResult<Json<BtcFundResponse>> {
    btc_fund_with_state(&state, req).await.map(Json)
}

pub async fn btc_fund_with_state(
    state: &AppState,
    req: BtcFundRequest,
) -> RpcResult<BtcFundResponse> {
    fund_btc_with_broadcast(state, req, async |raw: &[u8]| {
        broadcast_btc(state, raw).await
    })
    .await
}

async fn fund_btc_with_broadcast(
    state: &AppState,
    req: BtcFundRequest,
    broadcast: impl AsyncFnOnce(&[u8]) -> RpcResult<[u8; 32]>,
) -> RpcResult<BtcFundResponse> {
    let order = state
        .order_book
        .read()
        .await
        .get_order(&req.order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?;
    let maker_btc_address = order
        .maker_btc_address
        .clone()
        .ok_or_else(|| RpcError::BadRequest("Order has no maker BTC address".into()))?;
    let mut swaps = state.swaps.write().await;
    let swap = swaps.get_mut(&req.order_id).ok_or_else(|| {
        RpcError::BadRequest("VTR leg not funded yet — fund the order first".into())
    })?;
    if swap.btc_funding_raw.is_some()
        && swap.taker_btc_refund_address.as_deref() != Some(&req.btc_refund_address)
    {
        return Err(RpcError::BadRequest(
            "BTC refund address differs from the reserved funding transaction".into(),
        ));
    }
    if swap.status == SwapStatus::BtcFunded {
        let txid = swap
            .btc_funding_txid
            .ok_or_else(|| RpcError::Internal("BTC funding txid missing".into()))?;
        return Ok(BtcFundResponse {
            order_id: req.order_id,
            btc_funding_txid: btc_txid_hex(&txid),
            status: "BtcFunded".into(),
        });
    }
    if swap.status != SwapStatus::VtrFunded && swap.status != SwapStatus::BtcFunding {
        return Err(RpcError::BadRequest(format!(
            "Cannot fund BTC in swap state {:?}",
            swap.status
        )));
    }
    if swap.vtr_funding_txid != order.funding_txid || order.hash_lock != Some(swap.hash_lock) {
        return Err(RpcError::BadRequest(
            "VTR order and swap funding disagree".into(),
        ));
    }
    let btc = state.btc_wallet.read().await;
    let wallet = btc
        .as_ref()
        .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
    let now = now_secs_mock(state).await;
    let terms = {
        let chain = state.chain.lock().await;
        vtorrent_wallet_service::swap_policy::verify_vtr_swap_funding(&chain, &order, now)
            .map_err(RpcError::BadRequest)?
    };
    if let Some(raw) = &swap.btc_funding_raw {
        if u64::from(swap.btc_expiry)
            < now.saturating_add(u64::from(
                vtorrent_wallet_service::swap_policy::MIN_BTC_SWAP_WINDOW,
            ))
        {
            return Err(RpcError::BadRequest("Reserved BTC funding window has elapsed; reconcile the recorded transaction before recovery".into()));
        }
        broadcast(raw).await?;
        swap.status = SwapStatus::BtcFunded;
        let txid = swap
            .btc_funding_txid
            .ok_or_else(|| RpcError::Internal("Reserved BTC txid missing".into()))?;
        return Ok(BtcFundResponse {
            order_id: req.order_id,
            btc_funding_txid: btc_txid_hex(&txid),
            status: "BtcFunded".into(),
        });
    }
    let expiry = terms.btc_expiry();
    let (txid, _) = vtorrent_wallet_service::build_btc_htlc_funding(
        wallet,
        terms,
        &req.btc_refund_address,
        async |raw: &[u8]| {
            use bitcoin::hashes::Hash;
            let tx: bitcoin::Transaction =
                bitcoin::consensus::deserialize(raw).map_err(|e| e.to_string())?;
            swap.btc_funding_txid = Some(tx.compute_txid().to_byte_array());
            swap.btc_funding_raw = Some(raw.to_vec());
            swap.maker_btc_address = Some(maker_btc_address);
            swap.taker_btc_refund_address = Some(req.btc_refund_address.clone());
            swap.btc_amount = order.target_amount;
            swap.btc_expiry = expiry;
            swap.status = SwapStatus::BtcFunding;
            broadcast(raw).await.map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(RpcError::BadRequest)?;
    swap.status = SwapStatus::BtcFunded;
    Ok(BtcFundResponse {
        order_id: req.order_id,
        btc_funding_txid: btc_txid_hex(&txid),
        status: "BtcFunded".into(),
    })
}

/// POST /api/v1/swap/vtr-claim
///
/// The taker claims VTR by revealing the preimage.
pub async fn vtr_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VtrClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    vtr_claim_with_state(&state, req).await.map(Json)
}

pub async fn vtr_claim_with_state(
    state: &AppState,
    req: VtrClaimRequest,
) -> RpcResult<SwapActionResponse> {
    let preimage = parse_hash32(&req.preimage, "preimage")?;
    if req.taker_wif.is_empty() {
        return Err(RpcError::BadRequest("Taker WIF is required".into()));
    }

    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| RpcError::BadRequest("Order has no hash lock".into()))?;
    let funding_txid = order
        .funding_txid
        .ok_or_else(|| RpcError::BadRequest("Order has no VTR funding txid".into()))?;
    let taker_address = order
        .taker_address
        .clone()
        .ok_or_else(|| RpcError::BadRequest("Order has no taker address".into()))?;

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    if swap.vtr_refund_txid.is_some() {
        return Err(RpcError::BadRequest("VTR refund already submitted".into()));
    }
    if let Some(txid) = swap.vtr_claim_txid {
        return Ok(SwapActionResponse {
            order_id: req.order_id,
            txid: hex::encode(txid),
            status: "VtrClaimSubmitted".into(),
        });
    }

    // Verify the preimage matches the hash lock.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(preimage);
    let digest = hasher.finalize();
    if digest.as_slice() != hash_lock {
        return Err(RpcError::BadRequest(
            "Preimage does not match hash lock".into(),
        ));
    }

    // Build and sign the claim via the shared service path (same builder as
    // the Tauri frontend). Uses the exact funded expiry so the script matches
    // the funding output.
    let claim_tx =
        vtorrent_wallet_service::build_vtr_htlc_claim(vtorrent_wallet_service::VtrClaimParams {
            hash_lock,
            taker_address: &taker_address,
            maker_address: &order.maker_address,
            expiry: order.expiry,
            vtr_amount: order.vtr_amount,
            funding_txid,
            preimage,
            taker_wif: &req.taker_wif,
        })
        .map_err(RpcError::BadRequest)?;
    let claim_txid = claim_tx.txid();

    // Admit to the mempool and broadcast.
    {
        let chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, claim_tx.clone())
            .map_err(|e| RpcError::BadRequest(format!("Mempool rejected VTR claim tx: {}", e)))?;
    }
    if let Some(sender) = &state.tx_submit {
        let _ = sender.try_send(claim_tx);
    }

    swap.preimage = Some(preimage);
    swap.vtr_claim_txid = Some(claim_txid);
    swap.refresh_status();

    Ok(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(claim_txid),
        status: "VtrClaimSubmitted".to_string(),
    })
}

/// POST /api/v1/swap/btc-claim
///
/// The maker reveals the preimage by claiming BTC.
pub async fn btc_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    btc_claim_with_state(&state, req).await.map(Json)
}

pub async fn btc_claim_with_state(
    state: &AppState,
    req: BtcClaimRequest,
) -> RpcResult<SwapActionResponse> {
    let order_preimage = state
        .order_book
        .read()
        .await
        .get_order(&req.order_id)
        .and_then(|order| order.preimage);
    let mut swaps = state.swaps.write().await;
    let (preimage, btc_funding_txid, maker_btc_address, btc_amount, btc_expiry, refund_address) = {
        let swap = swaps
            .get(&req.order_id)
            .ok_or_else(|| RpcError::NotFound(format!("Swap {} not found", req.order_id)))?;
        if swap.btc_refund_txid.is_some() {
            return Err(RpcError::BadRequest("BTC refund already submitted".into()));
        }
        if swap.vtr_refund_txid.is_some() {
            return Err(RpcError::BadRequest(
                "VTR refund already submitted; refusing to reveal the swap secret".into(),
            ));
        }
        if now_secs_mock(state).await.saturating_add(u64::from(
            vtorrent_wallet_service::swap_policy::MIN_BTC_SWAP_WINDOW,
        )) >= u64::from(swap.btc_expiry)
        {
            return Err(RpcError::BadRequest(
                "BTC claim window is too close to refund eligibility to reveal the secret safely"
                    .into(),
            ));
        }
        // Maker-created orders hold the preimage before its first on-chain revelation.
        let preimage = match swap.preimage {
            Some(p) => p,
            None => order_preimage
                .ok_or_else(|| RpcError::BadRequest("Preimage not available".into()))?,
        };
        let btc_funding_txid = swap
            .btc_funding_txid
            .ok_or_else(|| RpcError::BadRequest("BTC funding txid not recorded".into()))?;
        let maker_btc_address = swap
            .maker_btc_address
            .clone()
            .ok_or_else(|| RpcError::BadRequest("Maker BTC address not recorded".into()))?;
        // The witness script embeds the taker's refund address, so it must be
        // reconstructed exactly as it was funded.
        let refund_address = swap
            .taker_btc_refund_address
            .clone()
            .ok_or_else(|| RpcError::BadRequest("Taker BTC refund address not recorded".into()))?;
        (
            preimage,
            btc_funding_txid,
            maker_btc_address,
            swap.btc_amount,
            swap.btc_expiry,
            refund_address,
        )
    };

    // The shared builder finds the key for the recorded maker BTC address.
    let (raw, txid) = {
        let btc = state.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
        vtorrent_wallet_service::build_btc_htlc_claim(
            w,
            vtorrent_wallet_service::BtcClaimParams {
                funding_txid: btc_funding_txid,
                preimage,
                maker_btc_address: &maker_btc_address,
                refund_address: &refund_address,
                expiry: btc_expiry,
                amount: btc_amount,
                network: *state.btc_network.read().await,
            },
        )
        .map_err(RpcError::BadRequest)?
    };
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::Internal("Swap state disappeared".into()))?;
    swap.btc_claim_txid = Some(txid);
    swap.refresh_status();
    broadcast_btc(state, &raw).await?;

    Ok(SwapActionResponse {
        order_id: req.order_id,
        txid: btc_txid_hex(&txid),
        status: "BtcClaimSubmitted".to_string(),
    })
}

/// POST /api/v1/swap/refund
///
/// Refund either side after expiry.
pub async fn swap_refund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwapRefundRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    swap_refund_with_state(&state, req).await.map(Json)
}

pub async fn swap_refund_with_state(
    state: &AppState,
    req: SwapRefundRequest,
) -> RpcResult<SwapActionResponse> {
    refund_with_broadcast(state, req, async |raw: &[u8]| {
        broadcast_btc(state, raw).await
    })
    .await
}

async fn refund_with_broadcast(
    state: &AppState,
    req: SwapRefundRequest,
    broadcast: impl AsyncFnOnce(&[u8]) -> RpcResult<[u8; 32]>,
) -> RpcResult<SwapActionResponse> {
    restore_btc_swap(state, &req.order_id).await?;
    let order = state
        .order_book
        .read()
        .await
        .get_order(&req.order_id)
        .cloned();
    let now = u32::try_from(now_secs_mock(state).await)
        .map_err(|_| RpcError::BadRequest("Swap clock exceeds u32::MAX".into()))?;
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::NotFound(format!("Swap {} not found", req.order_id)))?;
    let leg = match req.leg {
        Some(leg) => leg,
        None => {
            let vtr_ready = order.as_ref().is_some_and(|order| now >= order.expiry)
                && swap.vtr_funding_txid.is_some()
                && swap.vtr_claim_txid.is_none()
                && swap.vtr_refund_txid.is_none();
            let btc_ready = now >= swap.btc_expiry
                && swap.btc_expiry > 0
                && swap.btc_funding_txid.is_some()
                && swap.btc_claim_txid.is_none()
                && swap.btc_refund_txid.is_none();
            match (vtr_ready, btc_ready) {
                (true, false) => SwapLeg::Vtr,
                (false, true) => SwapLeg::Btc,
                _ => {
                    return Err(RpcError::BadRequest(
                        "Specify the refund leg as vtr or btc".into(),
                    ))
                }
            }
        }
    };

    let txid = match leg {
        SwapLeg::Vtr => {
            if swap.vtr_claim_txid.is_some() {
                return Err(RpcError::BadRequest("VTR claim already submitted".into()));
            }
            if let Some(txid) = swap.vtr_refund_txid {
                return Ok(SwapActionResponse {
                    order_id: req.order_id,
                    txid: hex::encode(txid),
                    status: "VtrRefundSubmitted".into(),
                });
            }
            let order =
                order.ok_or_else(|| RpcError::NotFound("VTR order metadata missing".into()))?;
            if now < order.expiry {
                return Err(RpcError::BadRequest("VTR HTLC has not expired yet".into()));
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
            let funding_txid = swap
                .vtr_funding_txid
                .ok_or_else(|| RpcError::BadRequest("VTR funding txid not recorded".into()))?;
            if order.funding_txid != Some(funding_txid) || order.hash_lock != Some(swap.hash_lock) {
                return Err(RpcError::BadRequest(
                    "VTR order and swap funding disagree".into(),
                ));
            }
            let taker = order
                .taker_address
                .as_deref()
                .ok_or_else(|| RpcError::BadRequest("Taker address not recorded".into()))?;
            let refund = vtorrent_wallet_service::build_vtr_htlc_refund(
                vtorrent_wallet_service::VtrRefundParams {
                    hash_lock: swap.hash_lock,
                    taker_address: taker,
                    maker_address: &order.maker_address,
                    expiry: order.expiry,
                    vtr_amount: order.vtr_amount,
                    funding_txid,
                    maker_wif: &maker_wif,
                },
            )
            .map_err(RpcError::BadRequest)?;
            let txid = refund.txid();
            {
                let chain = state.chain.lock().await;
                let mut mempool = state.mempool.lock().await;
                mempool
                    .admit_with_chain_fee(&chain, refund.clone())
                    .map_err(|e| RpcError::BadRequest(format!("VTR refund rejected: {e}")))?;
            }
            swap.vtr_refund_txid = Some(txid);
            swap.refresh_status();
            if let Some(sender) = &state.tx_submit {
                let _ = sender.try_send(refund);
            }
            hex::encode(txid)
        }
        SwapLeg::Btc => {
            if swap.btc_claim_txid.is_some() {
                return Err(RpcError::BadRequest("BTC claim already submitted".into()));
            }
            if now < swap.btc_expiry || swap.btc_expiry == 0 {
                return Err(RpcError::BadRequest("BTC HTLC has not expired yet".into()));
            }
            let funding_txid = swap
                .btc_funding_txid
                .ok_or_else(|| RpcError::BadRequest("BTC funding txid not recorded".into()))?;
            let refund_address = swap
                .taker_btc_refund_address
                .as_ref()
                .ok_or_else(|| RpcError::BadRequest("BTC refund address not recorded".into()))?;
            let recipient = swap
                .maker_btc_address
                .as_ref()
                .ok_or_else(|| RpcError::BadRequest("Maker BTC address not recorded".into()))?;
            let raw = if let Some(raw) = &swap.btc_refund_raw {
                raw.clone()
            } else {
                let btc = state.btc_wallet.read().await;
                let wallet = btc
                    .as_ref()
                    .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
                let htlc = vtorrent_btc::htlc::BtcHtlc {
                    hash_lock: swap.hash_lock,
                    recipient: recipient.clone(),
                    refund_address: refund_address.clone(),
                    expiry: swap.btc_expiry,
                    amount: swap.btc_amount,
                    network: wallet.network(),
                };
                let unsigned = htlc
                    .build_refund_tx_at(
                        funding_txid,
                        vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS,
                        now,
                    )
                    .map_err(|e| RpcError::BadRequest(e.to_string()))?;
                let wif = zeroize::Zeroizing::new(
                    wallet
                        .derive_wif_for_address(refund_address)
                        .map_err(|e| RpcError::BadRequest(e.to_string()))?,
                );
                let signed = htlc
                    .sign_refund_tx(unsigned, &wif)
                    .map_err(|e| RpcError::BadRequest(e.to_string()))?;
                bitcoin::consensus::serialize(&signed)
            };
            use bitcoin::hashes::Hash;
            let transaction: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw)
                .map_err(|_| RpcError::Internal("Recorded BTC refund is corrupted".into()))?;
            let txid = transaction.compute_txid().to_byte_array();
            {
                let btc = state.btc_wallet.read().await;
                let wallet = btc
                    .as_ref()
                    .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
                wallet
                    .record_swap_refund(&req.order_id, &raw)
                    .map_err(|e| RpcError::BadRequest(e.to_string()))?;
            }
            swap.btc_refund_txid = Some(txid);
            swap.btc_refund_raw = Some(raw.clone());
            swap.refresh_status();
            broadcast(&raw).await?;
            btc_txid_hex(&txid)
        }
    };
    Ok(SwapActionResponse {
        order_id: req.order_id,
        txid,
        status: match leg {
            SwapLeg::Vtr => "VtrRefundSubmitted".into(),
            SwapLeg::Btc => "BtcRefundSubmitted".into(),
        },
    })
}

async fn restore_btc_swap(state: &AppState, order_id: &str) -> RpcResult<()> {
    let saved = state
        .btc_wallet
        .read()
        .await
        .as_ref()
        .and_then(|wallet| wallet.swap_contract(order_id));
    let Some(saved) = saved else {
        return Ok(());
    };
    use bitcoin::hashes::Hash;
    let funding_txid = saved
        .funding_txid
        .parse::<bitcoin::Txid>()
        .map_err(|_| RpcError::Internal("Persisted BTC funding txid is malformed".into()))?
        .to_byte_array();
    let mut recovered = SwapState::new(parse_hash32(order_id, "order_id")?, saved.hash_lock);
    recovered.btc_funding_txid = Some(funding_txid);
    recovered.maker_btc_address = Some(saved.recipient);
    recovered.taker_btc_refund_address = Some(saved.refund_address);
    recovered.btc_expiry = saved.expiry;
    recovered.btc_amount = saved.amount;
    recovered.btc_funding_raw = state.btc_wallet.read().await.as_ref().and_then(|wallet| {
        wallet
            .pending_swap_transactions()
            .get(&saved.funding_txid)
            .cloned()
    });
    if let Some(raw) = saved.refund_raw {
        let refund: bitcoin::Transaction = bitcoin::consensus::deserialize(&raw)
            .map_err(|_| RpcError::Internal("Persisted BTC refund is malformed".into()))?;
        recovered.btc_refund_txid = Some(refund.compute_txid().to_byte_array());
        recovered.btc_refund_raw = Some(raw);
    }
    recovered.refresh_status();
    if recovered.btc_refund_txid.is_none() {
        recovered.status = SwapStatus::BtcFunding;
    }
    state
        .swaps
        .write()
        .await
        .entry(order_id.to_owned())
        .or_insert(recovered);
    Ok(())
}

#[cfg(test)]
mod tests;
