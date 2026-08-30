use axum::{extract::State, Json};
use std::sync::Arc;

use super::{
    broadcast_btc, btc_txid_hex, now_secs, now_secs_mock, parse_hash32, require_swap_stage,
    utxo_select, verify_wallet_auth,
};
use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;
use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

pub async fn match_dex_order(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MatchOrderRequest>,
) -> RpcResult<Json<MatchOrderResponse>> {
    use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, MIN_HTLC_LOCKTIME};
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

    let now = now_secs() as u32;
    let remaining_locktime = order.expiry.saturating_sub(now);
    if remaining_locktime < MIN_HTLC_LOCKTIME {
        return Err(RpcError::BadRequest(
            "DEX order is too close to expiry to fund safely".into(),
        ));
    }
    let (preimage, hash_lock) = match (order.preimage, order.hash_lock) {
        (Some(preimage), Some(hash_lock)) => (preimage, hash_lock),
        _ => {
            let swap = AtomicSwap::new();
            (swap.preimage, swap.hash_lock)
        }
    };
    let htlc = Htlc::new(
        hash_lock,
        req.taker_address.clone(),
        order.maker_address.clone(),
        remaining_locktime,
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
    const FUNDING_FEE_SATOSHIS: u64 = 10_000;
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
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

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
    let maker_btc_address = order
        .maker_btc_address
        .clone()
        .ok_or_else(|| RpcError::BadRequest("Order has no maker BTC address".into()))?;

    // Lifecycle guard: the VTR leg must be funded first — locking BTC into an
    // HTLC for an unfunded order would strand the taker's BTC until refund.
    // The BTC HTLC must not already be funded, and a finished swap
    // (claimed/refunded) can never be funded again.
    {
        let swaps = state.swaps.read().await;
        let swap = swaps.get(&req.order_id).ok_or_else(|| {
            RpcError::BadRequest(
                "VTR leg not funded yet — call /api/v1/dex/match to fund the order first".into(),
            )
        })?;
        if swap.status != SwapStatus::VtrFunded {
            return Err(RpcError::BadRequest(format!(
                "Swap is in state {:?}; BTC funding requires VtrFunded",
                swap.status
            )));
        }
    }

    // The BTC amount the taker must lock is the order's target amount.
    let btc_amount = order.target_amount;
    if btc_amount == 0 {
        return Err(RpcError::BadRequest("Order target amount is zero".into()));
    }

    // Build the BTC HTLC: the maker is the recipient (claims with preimage),
    // the taker is the refund address.
    let btc_network = {
        let btc = state.btc_wallet.read().await;
        btc.as_ref()
            .map(|w| w.network())
            .unwrap_or(bitcoin::Network::Bitcoin)
    };
    let htlc = vtorrent_btc::htlc::BtcHtlc::new_with_network(
        hash_lock,
        maker_btc_address.clone(),
        req.btc_refund_address.clone(),
        vtorrent_btc::htlc::DEFAULT_HTLC_LOCKTIME,
        btc_amount,
        btc_network,
    )
    .map_err(|e| RpcError::BadRequest(format!("Unable to construct BTC HTLC: {}", e)))?;

    // Select a UTXO from the local BTC wallet to fund the HTLC.
    const FUNDING_FEE_SATOSHIS: u64 = 1_000;
    let (funding_utxo, funder_wif, change_address) = {
        let btc = state.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
        let utxos = w.list_utxos();
        let selected = utxo_select(&utxos, btc_amount, FUNDING_FEE_SATOSHIS)
            .ok_or_else(|| RpcError::BadRequest("Insufficient BTC funds".into()))?;
        // The funding tx has a single input; use the largest selected UTXO.
        let utxo = selected
            .into_iter()
            .max_by_key(|u| u.value)
            .ok_or_else(|| RpcError::BadRequest("No BTC UTXO available".into()))?;
        let wif = w
            .derive_wif(0)
            .map_err(|e| RpcError::Internal(e.to_string()))?;
        let change = w
            .current_address()
            .map_err(|e| RpcError::Internal(e.to_string()))?;
        (utxo, wif, change)
    };

    let funding_txid_bytes: [u8; 32] = {
        use bitcoin::hashes::Hash;
        funding_utxo
            .txid
            .parse::<bitcoin::Txid>()
            .map(|t| t.to_byte_array())
            .map_err(|e| RpcError::BadRequest(format!("Invalid UTXO txid: {}", e)))?
    };

    let unsigned = htlc
        .build_funding_tx(
            funding_txid_bytes,
            funding_utxo.vout,
            funding_utxo.value,
            FUNDING_FEE_SATOSHIS,
            &change_address,
        )
        .map_err(|e| RpcError::BadRequest(format!("Unable to build BTC funding tx: {}", e)))?;
    let signed = htlc
        .sign_funding_tx(unsigned, funding_utxo.value, &funder_wif)
        .map_err(|e| RpcError::BadRequest(format!("Unable to sign BTC funding tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let btc_funding_txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };

    // Broadcast to the Bitcoin network.
    broadcast_btc(&state, &raw).await?;

    // Record the swap state with the real funding txid.
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.btc_funding_txid = Some(btc_funding_txid);
    swap.maker_btc_address = Some(maker_btc_address);
    swap.taker_btc_refund_address = Some(req.btc_refund_address);
    swap.btc_amount = btc_amount;
    swap.btc_expiry = htlc.expiry;
    swap.status = SwapStatus::BtcFunded;

    Ok(Json(BtcFundResponse {
        order_id: req.order_id,
        btc_funding_txid: btc_txid_hex(&btc_funding_txid),
        status: "BtcFunded".to_string(),
    }))
}

/// POST /api/v1/swap/vtr-claim
///
/// The taker claims VTR by revealing the preimage.
pub async fn vtr_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VtrClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    use vtorrent_node::atomic_swap::{Htlc, SwapState, SwapStatus};
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

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

    // Lifecycle guard: a refunded swap cannot be claimed on the VTR leg.
    {
        let swaps = state.swaps.read().await;
        require_swap_stage(swaps.get(&req.order_id), &[SwapStatus::Refunded])?;
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

    // Reconstruct the HTLC: the taker is the recipient (claims with preimage),
    // the maker is the refund address. Use the exact funded expiry so the
    // script matches the funding output.
    let htlc = Htlc::with_expiry(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| RpcError::BadRequest(format!("Unable to reconstruct HTLC: {}", e)))?;

    const CLAIM_FEE_SATOSHIS: u64 = 10_000;
    let unsigned = htlc
        .build_claim_tx_unsigned(funding_txid, &preimage, CLAIM_FEE_SATOSHIS)
        .map_err(|e| RpcError::BadRequest(format!("Unable to build VTR claim tx: {}", e)))?;

    // Sign over the HTLC script (the funding output's scriptPubKey).
    let htlc_script = htlc
        .build_script()
        .map_err(|e| RpcError::BadRequest(format!("Invalid HTLC addresses: {}", e)))?;
    let (sig, pubkey) = sign_input_over_subscript(&unsigned, 0, &htlc_script, &req.taker_wif)
        .map_err(|e| RpcError::BadRequest(format!("Unable to sign VTR claim tx: {}", e)))?;

    // Build the scriptSig: <sig> <pubkey> <preimage> OP_1.
    let mut script_sig = Vec::new();
    script_sig.push(sig.len() as u8);
    script_sig.extend_from_slice(&sig);
    script_sig.push(pubkey.len() as u8);
    script_sig.extend_from_slice(&pubkey);
    script_sig.push(0x20);
    script_sig.extend_from_slice(&preimage);
    script_sig.push(0x51); // OP_1

    let mut claim_tx = unsigned;
    claim_tx.inputs[0].script_sig = script_sig;
    let claim_txid = claim_tx.txid();

    // Admit to the mempool and broadcast.
    {
        let mut mempool = state.mempool.lock().await;
        mempool
            .add_transaction_with_fee(claim_tx.clone(), CLAIM_FEE_SATOSHIS)
            .map_err(|e| RpcError::BadRequest(format!("Mempool rejected VTR claim tx: {}", e)))?;
    }
    if let Some(sender) = &state.tx_submit {
        let _ = sender.try_send(claim_tx);
    }

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.preimage = Some(preimage);
    swap.status = SwapStatus::Claimed;

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(claim_txid),
        status: "Claimed".to_string(),
    }))
}

/// POST /api/v1/swap/btc-claim
///
/// The maker claims BTC using the revealed preimage.
pub async fn btc_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    let (preimage, btc_funding_txid, maker_btc_address, btc_amount, btc_expiry, refund_address) = {
        let swaps = state.swaps.read().await;
        let swap = swaps
            .get(&req.order_id)
            .ok_or_else(|| RpcError::NotFound(format!("Swap {} not found", req.order_id)))?;
        // Lifecycle guard: the BTC leg can be claimed any time after it was
        // funded (typically after vtr-claim revealed the preimage) and never
        // after a refund.
        require_swap_stage(Some(swap), &[SwapStatus::Refunded])?;
        // The maker generated the preimage at order placement and holds it in
        // the order book. The swap state's preimage is only populated when the
        // taker reveals it via vtr_claim, so fall back to the order's preimage.
        let preimage = match swap.preimage {
            Some(p) => p,
            None => {
                let order_book = state.order_book.read().await;
                order_book
                    .get_order(&req.order_id)
                    .and_then(|o| o.preimage)
                    .ok_or_else(|| RpcError::BadRequest("Preimage not available".into()))?
            }
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

    // Reconstruct the HTLC and build/sign/broadcast the claim.
    let btc_network = *state.btc_network.read().await;
    let htlc = vtorrent_btc::htlc::BtcHtlc {
        hash_lock: {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(preimage);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        },
        recipient: maker_btc_address.clone(),
        refund_address,
        expiry: btc_expiry,
        amount: btc_amount,
        network: btc_network,
    };
    const CLAIM_FEE_SATOSHIS: u64 = 1_000;
    let unsigned = htlc
        .build_claim_tx(btc_funding_txid, &preimage, CLAIM_FEE_SATOSHIS)
        .map_err(|e| RpcError::BadRequest(format!("Unable to build BTC claim tx: {}", e)))?;

    // The maker's BTC key is derived from the wallet seed at index 0.
    let maker_wif = {
        let btc = state.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
        w.derive_wif(0)
            .map_err(|e| RpcError::Internal(e.to_string()))?
    };
    let signed = htlc
        .sign_claim_tx(unsigned, &preimage, &maker_wif)
        .map_err(|e| RpcError::BadRequest(format!("Unable to sign BTC claim tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    broadcast_btc(&state, &raw).await?;

    {
        let mut swaps = state.swaps.write().await;
        if let Some(swap) = swaps.get_mut(&req.order_id) {
            swap.status = SwapStatus::Claimed;
        }
    }

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: btc_txid_hex(&txid),
        status: "Claimed".to_string(),
    }))
}

/// POST /api/v1/swap/refund
///
/// Refund either side after expiry.
pub async fn swap_refund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwapRefundRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    use vtorrent_node::atomic_swap::{Htlc, SwapState, SwapStatus};
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?
    };
    let now = now_secs_mock(&state).await as u32;
    if now < order.expiry {
        return Err(RpcError::BadRequest("Swap has not expired yet".into()));
    }

    // Lifecycle guard: each leg refunds independently; a completed refund is
    // the only terminal state for this endpoint.
    {
        let swaps = state.swaps.read().await;
        require_swap_stage(swaps.get(&req.order_id), &[SwapStatus::Refunded])?;
    }

    // ── VTR-side refund (the maker reclaims their VTR) ──────────────────────
    let vtr_refund_txid = {
        let hash_lock = order.hash_lock;
        let funding_txid = order.funding_txid;
        let taker_address = order.taker_address.clone();
        match (hash_lock, funding_txid, taker_address) {
            (Some(hash_lock), Some(funding_txid), Some(taker_address)) => {
                // The maker is the refund address; the taker is the recipient.
                let htlc = Htlc::with_expiry(
                    hash_lock,
                    taker_address,
                    order.maker_address.clone(),
                    order.expiry,
                    order.vtr_amount,
                )
                .map_err(|e| RpcError::BadRequest(format!("Unable to reconstruct HTLC: {}", e)))?;

                const REFUND_FEE_SATOSHIS: u64 = 10_000;
                let unsigned = htlc
                    .build_refund_tx_unsigned(funding_txid, REFUND_FEE_SATOSHIS)
                    .map_err(|e| {
                        RpcError::BadRequest(format!("Unable to build VTR refund tx: {}", e))
                    })?;

                // The maker signs the refund (they are the refund address).
                let maker_wif = state
                    .wallet_wif
                    .read()
                    .await
                    .clone()
                    .ok_or_else(|| RpcError::BadRequest("Maker wallet not unlocked".into()))?;
                let htlc_script = htlc
                    .build_script()
                    .map_err(|e| RpcError::BadRequest(format!("Invalid HTLC addresses: {}", e)))?;
                let (sig, pubkey) =
                    sign_input_over_subscript(&unsigned, 0, &htlc_script, &maker_wif).map_err(
                        |e| RpcError::BadRequest(format!("Unable to sign VTR refund tx: {}", e)),
                    )?;

                // scriptSig: <sig> <pubkey> OP_0 (false branch).
                let mut script_sig = Vec::new();
                script_sig.push(sig.len() as u8);
                script_sig.extend_from_slice(&sig);
                script_sig.push(pubkey.len() as u8);
                script_sig.extend_from_slice(&pubkey);
                script_sig.push(0x00); // OP_0

                let mut refund_tx = unsigned;
                refund_tx.inputs[0].script_sig = script_sig;
                let refund_txid = refund_tx.txid();

                {
                    let mut mempool = state.mempool.lock().await;
                    mempool
                        .add_transaction_with_fee(refund_tx.clone(), REFUND_FEE_SATOSHIS)
                        .map_err(|e| {
                            RpcError::BadRequest(format!("Mempool rejected VTR refund tx: {}", e))
                        })?;
                }
                if let Some(sender) = &state.tx_submit {
                    let _ = sender.try_send(refund_tx);
                }
                Some(refund_txid)
            }
            _ => None,
        }
    };

    // ── BTC-side refund (the taker reclaims their BTC) ──────────────────────
    let btc_refund_txid = {
        let swaps = state.swaps.read().await;
        let swap = swaps.get(&req.order_id);
        match swap {
            Some(s) if s.btc_funding_txid.is_some() && s.btc_expiry > 0 => {
                let funding_txid = s.btc_funding_txid.unwrap();
                let refund_address = s.taker_btc_refund_address.clone().ok_or_else(|| {
                    RpcError::BadRequest("Taker BTC refund address not recorded".into())
                })?;
                let htlc = vtorrent_btc::htlc::BtcHtlc {
                    hash_lock: s.hash_lock,
                    recipient: s.maker_btc_address.clone().unwrap_or_default(),
                    refund_address,
                    expiry: s.btc_expiry,
                    amount: s.btc_amount,
                    network: *state.btc_network.read().await,
                };
                const REFUND_FEE_SATOSHIS: u64 = 1_000;
                let unsigned = htlc
                    .build_refund_tx_at(funding_txid, REFUND_FEE_SATOSHIS, now)
                    .map_err(|e| {
                        RpcError::BadRequest(format!("Unable to build BTC refund tx: {}", e))
                    })?;
                let refund_wif = {
                    let btc = state.btc_wallet.read().await;
                    let w = btc
                        .as_ref()
                        .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
                    w.derive_wif(0)
                        .map_err(|e| RpcError::Internal(e.to_string()))?
                };
                let signed = htlc.sign_refund_tx(unsigned, &refund_wif).map_err(|e| {
                    RpcError::BadRequest(format!("Unable to sign BTC refund tx: {}", e))
                })?;
                let raw = bitcoin::consensus::encode::serialize(&signed);
                let txid = {
                    use bitcoin::hashes::Hash;
                    signed.compute_txid().to_byte_array()
                };
                broadcast_btc(&state, &raw).await?;
                Some(txid)
            }
            _ => None,
        }
    };

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, order.hash_lock.unwrap_or([0u8; 32])));
    swap.status = SwapStatus::Refunded;

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: vtr_refund_txid
            .or_else(|| {
                btc_refund_txid.map(|mut t| {
                    t.reverse();
                    t
                })
            })
            .map(hex::encode)
            .unwrap_or_else(|| hex::encode(order.order_id)),
        status: "Refunded".to_string(),
    }))
}
