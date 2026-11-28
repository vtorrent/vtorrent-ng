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
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    match_dex_order_with_wif(&state, req, &wif).await.map(Json)
}

/// Shared maker funding path; the desktop supplies its already-unlocked signing key.
pub async fn match_dex_order_with_wif(
    state: &AppState,
    req: MatchOrderRequest,
    wif: &str,
) -> RpcResult<MatchOrderResponse> {
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

    {
        let swaps = state.swaps.write().await;
        if let Some(swap) = swaps.get(&req.order_id) {
            if let Some(tx) = &swap.vtr_funding_tx {
                if order.taker_address.as_deref() != Some(&req.taker_address) {
                    return Err(RpcError::BadRequest(
                        "Taker differs from prepared VTR funding".into(),
                    ));
                }
                let now = now_secs_mock(state).await;
                if u64::from(order.expiry) < now.saturating_add(u64::from(MIN_HTLC_LOCKTIME))
                    && state
                        .chain
                        .lock()
                        .await
                        .get_transaction_owned(&tx.txid())
                        .is_none()
                {
                    return Err(RpcError::BadRequest("Prepared VTR funding window has elapsed; retain the record for reconciliation/refund".into()));
                }
                crate::swap_recovery::persist(state, &order, Some(swap)).await?;
                crate::swap_recovery::submit_vtr(state, tx).await?;
                return Ok(match_response(&order));
            }
        }
    }
    if order.status != vtorrent_node::atomic_swap::OrderStatus::Open {
        return Err(RpcError::BadRequest("Order is not open".into()));
    }

    // Mock-clock aware (mirrors swap_refund): regtest tests set mock_time
    // and expect expiry math consistent with it.
    let now = now_secs_mock(state).await as u32;
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
        (Some(preimage), Some(hash_lock)) => {
            use sha2::{Digest, Sha256};
            if Sha256::digest(preimage).as_slice() != hash_lock {
                return Err(RpcError::BadRequest(
                    "Order preimage does not match its published hash lock".into(),
                ));
            }
            (preimage, hash_lock)
        }
        (None, Some(_)) => {
            return Err(RpcError::BadRequest(
                "Order has a published hash lock but no preimage; refusing to re-key it".into(),
            ))
        }
        (Some(_), None) => {
            return Err(RpcError::BadRequest(
                "Order has a preimage but no hash lock".into(),
            ))
        }
        (None, None) => {
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
    // Serialize selection + admission so two matches cannot pick the same
    // confirmed outpoint before either reaches the mempool.
    let _funding_guard = state.vtr_funding_lock.lock().await;

    // Use a verified wallet UTXO large enough to fund this single-input HTLC.
    // A fixed 10,000-satoshi fee is intentionally conservative for the custom
    // script size and is recorded as an authoritative local mempool fee.
    const FUNDING_FEE_SATOSHIS: u64 = vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let staking_address = state.staking_address.read().await.clone();
    let funding_utxo = {
        let chain = state.chain.lock().await;
        let mempool = state.mempool.lock().await;
        let candidates: Vec<_> = chain
            .get_utxos_for_address(&wallet_address)
            .into_iter()
            .filter(|utxo| !mempool.is_outpoint_spent(&utxo.txid, utxo.vout))
            .collect();
        let structural =
            vtorrent_wallet_service::structural_stakeable(&chain, staking_address.as_deref(), now);
        vtorrent_wallet_service::select_reserved_utxo(
            &candidates,
            &structural,
            order.vtr_amount.saturating_add(FUNDING_FEE_SATOSHIS),
        )
        .cloned()
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
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), wif);
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
    if let Err(error) = crate::swap_recovery::validate_vtr(state, &funding_tx).await {
        state
            .order_book
            .write()
            .await
            .release_funding(&req.order_id);
        return Err(error);
    }
    let mut prepared_order = order;
    prepared_order.preimage = Some(preimage);
    prepared_order.hash_lock = Some(hash_lock);
    prepared_order.funding_txid = Some(funding_txid);
    prepared_order.taker_address = Some(req.taker_address);
    prepared_order.status = vtorrent_node::atomic_swap::OrderStatus::Matched;
    state
        .order_book
        .write()
        .await
        .replace_order(prepared_order.clone());
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(prepared_order.order_id, hash_lock));
    swap.vtr_funding_txid = Some(funding_txid);
    swap.vtr_funding_tx = Some(funding_tx.clone());
    swap.status = SwapStatus::VtrFunded;
    crate::swap_recovery::persist(state, &prepared_order, Some(swap)).await?;
    // Drop before the failure path: `state.swaps` is a non-reentrant write
    // lock, and taking it again below would deadlock the whole swap subsystem.
    drop(swaps);
    if let Err(error) = crate::swap_recovery::submit_vtr(state, &funding_tx).await {
        // Remove the durable record before reverting the public order. If the
        // record cannot be removed, leaving the order Matched keeps memory and
        // disk consistent; reverting would let the next unlock resurrect the
        // swap from a record that still names this funding transaction.
        if let Err(remove_error) = crate::swap_recovery::remove(state, &req.order_id).await {
            tracing::warn!(
                order_id = %req.order_id,
                error = %remove_error,
                "Could not remove failed funding recovery record; keeping order matched"
            );
            return Err(RpcError::Internal(format!(
                "{error}; recovery record could not be removed ({remove_error}); order remains matched for manual recovery"
            )));
        }
        state.order_book.write().await.revert_match(&req.order_id);
        state.swaps.write().await.remove(&req.order_id);
        return Err(error);
    }
    Ok(match_response(&prepared_order))
}

fn match_response(order: &vtorrent_node::atomic_swap::SwapOrder) -> MatchOrderResponse {
    MatchOrderResponse {
        order_id: hex::encode(order.order_id),
        maker_address: order.maker_address.clone(),
        vtr_amount: order.vtr_amount,
        target_asset: order.target_asset.clone(),
        target_amount: order.target_amount,
        hash_lock: hex::encode(order.hash_lock.unwrap_or_default()),
        expiry: order.expiry,
        funding_txid: hex::encode(order.funding_txid.unwrap_or_default()),
        maker_btc_address: order.maker_btc_address.clone(),
        taker_address: order.taker_address.clone(),
    }
}

/// Import the maker's match response on the taker's own node.
///
/// The request is untrusted: every term is re-derived into the exact VTR HTLC
/// script and checked against the confirmed funding output before local swap
/// state is created. This lets a separate taker use `btc-fund`, observe the
/// BTC claim, and claim VTR automatically without sharing the maker's node.
pub async fn import_match(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ImportMatchRequest>,
) -> RpcResult<Json<MatchOrderResponse>> {
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    import_match_with_wif(&state, req, &wif).await.map(Json)
}

pub async fn import_match_with_wif(
    state: &AppState,
    req: ImportMatchRequest,
    wif: &str,
) -> RpcResult<MatchOrderResponse> {
    use vtorrent_node::atomic_swap::{OrderStatus, SwapOrder};
    use vtorrent_wallet_service::swap_policy::verify_vtr_swap_funding;

    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.target_asset != "BTC" {
        return Err(RpcError::BadRequest(
            "Only BTC-targeted swaps are supported".into(),
        ));
    }
    let order_id = parse_hash32(&req.order_id, "order_id")?;
    let canonical_id = hex::encode(order_id);
    let hash_lock = parse_hash32(&req.hash_lock, "hash_lock")?;
    let funding_txid = parse_hash32(&req.funding_txid, "funding_txid")?;

    let key = vtorrent_core::keys::PrivateKey::from_wif(wif)
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    let public = key
        .public_key()
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    let local_taker = vtorrent_core::address::Address::from_pubkey(
        &public,
        key.is_compressed(),
        vtorrent_core::network::mainnet::PUBKEY_ADDRESS_PREFIX,
    )
    .to_string();
    if local_taker != req.taker_address {
        return Err(RpcError::Unauthorized(
            "The unlocked wallet does not control the imported taker address".into(),
        ));
    }

    let existing = state
        .order_book
        .read()
        .await
        .get_order(&canonical_id)
        .cloned();
    if let Some(order) = &existing {
        if order.preimage.is_some() {
            return Err(RpcError::BadRequest(
                "The local order already holds the maker secret; import is for taker nodes".into(),
            ));
        }
        if order.maker_address != req.maker_address
            || order.vtr_amount != req.vtr_amount
            || order.target_asset != req.target_asset
            || order.target_amount != req.target_amount
            || order.expiry != req.expiry
            || order.hash_lock.is_some_and(|value| value != hash_lock)
            || order
                .funding_txid
                .is_some_and(|value| value != funding_txid)
            || order
                .taker_address
                .as_deref()
                .is_some_and(|value| value != req.taker_address)
        {
            return Err(RpcError::BadRequest(
                "Imported match terms disagree with the local order announcement".into(),
            ));
        }
    }
    let maker_btc_address = match (
        existing
            .as_ref()
            .and_then(|order| order.maker_btc_address.clone()),
        req.maker_btc_address.clone(),
    ) {
        (Some(local), Some(imported)) if local != imported => {
            return Err(RpcError::BadRequest(
                "Imported maker BTC address disagrees with the local order".into(),
            ))
        }
        (Some(local), _) => local,
        (None, Some(imported)) => imported,
        (None, None) => {
            return Err(RpcError::BadRequest(
                "Maker BTC address is required when the local order is unknown".into(),
            ))
        }
    };
    use std::str::FromStr as _;
    let btc_network = *state.btc_network.read().await;
    bitcoin::Address::from_str(&maker_btc_address)
        .map_err(|error| RpcError::BadRequest(format!("Invalid maker BTC address: {error}")))?
        .require_network(btc_network)
        .map_err(|error| {
            RpcError::BadRequest(format!(
                "Maker BTC address is not valid for the configured BTC network: {error}"
            ))
        })?;

    let mut order = existing.unwrap_or_else(|| {
        SwapOrder::new(
            req.maker_address.clone(),
            req.vtr_amount,
            req.target_asset.clone(),
            req.target_amount,
            req.expiry,
        )
    });
    order.order_id = order_id;
    order.maker_address = req.maker_address.clone();
    order.maker_btc_address = Some(maker_btc_address);
    order.vtr_amount = req.vtr_amount;
    order.target_asset = req.target_asset.clone();
    order.target_amount = req.target_amount;
    order.expiry = req.expiry;
    order.hash_lock = Some(hash_lock);
    order.funding_txid = Some(funding_txid);
    order.taker_address = Some(req.taker_address.clone());
    order.status = OrderStatus::Matched;
    order.preimage = None;

    let now = now_secs_mock(state).await;
    let expires_at = {
        let chain = state.chain.lock().await;
        verify_vtr_swap_funding(&chain, &order, now)
            .map_err(RpcError::BadRequest)?
            .btc_expiry()
    };

    let existing_swap = state.swaps.read().await.get(&canonical_id).cloned();
    let mut swap = existing_swap.unwrap_or_else(|| SwapState::new(order_id, hash_lock));
    if swap
        .vtr_funding_txid
        .is_some_and(|value| value != funding_txid)
        || swap.btc_funding_txid.is_some()
        || swap.vtr_claim_txid.is_some()
        || swap.vtr_refund_txid.is_some()
    {
        return Err(RpcError::BadRequest(
            "Local swap state already refers to a different or later stage".into(),
        ));
    }
    swap.hash_lock = hash_lock;
    swap.vtr_funding_txid = Some(funding_txid);
    swap.maker_btc_address = order.maker_btc_address.clone();
    swap.btc_amount = order.target_amount;
    swap.btc_expiry = expires_at;
    swap.status = SwapStatus::VtrFunded;
    swap.refresh_status();

    // Persist first: a rejected recovery write must not leave the in-memory
    // order overwritten with different terms.
    crate::swap_recovery::persist_for_wallet(state, &order, Some(&swap), Some(wif)).await?;

    // Re-check under the write locks: a concurrent btc_fund/match must not be
    // overwritten by the snapshot we read before the recovery write.
    let mut orders = state.order_book.write().await;
    if let Some(current) = orders.get_order(&canonical_id) {
        if current.preimage.is_some()
            || current.maker_address != order.maker_address
            || current.vtr_amount != order.vtr_amount
            || current.target_asset != order.target_asset
            || current.target_amount != order.target_amount
            || current.expiry != order.expiry
            || current.hash_lock.is_some_and(|value| value != hash_lock)
            || current
                .funding_txid
                .is_some_and(|value| value != funding_txid)
        {
            return Err(RpcError::BadRequest(
                "Local order changed during match import; retry from current state".into(),
            ));
        }
    }
    orders.replace_order(order.clone());
    drop(orders);

    let mut swaps = state.swaps.write().await;
    if let Some(current) = swaps.get(&canonical_id) {
        if current.btc_funding_txid.is_some()
            || current.vtr_claim_txid.is_some()
            || current.vtr_refund_txid.is_some()
            || current
                .vtr_funding_txid
                .is_some_and(|value| value != funding_txid)
        {
            return Err(RpcError::BadRequest(
                "Local swap advanced during match import; retry from current state".into(),
            ));
        }
    }
    swaps.insert(canonical_id, swap);
    Ok(match_response(&order))
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
    crate::handlers::authorize_wallet_operation(state, &req.passphrase, req.otp_code.as_deref())
        .await?;
    btc_fund_authorized(state, req).await
}

/// Fund the BTC leg after the caller has already authorized the wallet.
pub async fn btc_fund_authorized(
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
        crate::swap_recovery::persist(state, &order, Some(swap)).await?;
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
            crate::swap_recovery::persist(state, &order, Some(swap))
                .await
                .map_err(|e| e.to_string())?;
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
    // HTTP callers must authorize with a passphrase and fresh TOTP. The
    // desktop bridge authorizes its own wallet and calls the shared path
    // directly; a raw key over HTTP is only tolerated by the regtest tests.
    if !req.taker_wif.is_empty() && !state.regtest {
        return Err(RpcError::BadRequest(
            "taker_wif is only accepted on regtest; use passphrase and otp_code".into(),
        ));
    }
    vtr_claim_with_state(&state, req).await.map(Json)
}

pub async fn vtr_claim_with_state(
    state: &AppState,
    req: VtrClaimRequest,
) -> RpcResult<SwapActionResponse> {
    // Sign with the wallet's own key — the private key never reaches the client.
    // A non-empty `taker_wif` is an explicit override for CLI/tests only.
    let taker_wif = if req.taker_wif.is_empty() {
        verify_wallet_auth(state, &req.passphrase, req.otp_code.as_deref()).await?
    } else {
        req.taker_wif.clone()
    };

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

    // The preimage may be supplied directly, or recovered from the taker's own
    // BTC settlement observation of the maker's claim. The latter is what lets
    // a taker on a different node complete the swap: the maker reveals the
    // secret on-chain, the taker's scan records it, and it is stored in the
    // swap state (never in the observation, which stays secret-free).
    let preimage = if req.preimage.trim().is_empty() {
        let swaps = state.swaps.read().await;
        swaps
            .get(&req.order_id)
            .and_then(|swap| swap.preimage)
            .ok_or_else(|| {
                RpcError::BadRequest(
                    "Preimage not supplied and no confirmed BTC claim has been observed yet".into(),
                )
            })?
    } else {
        parse_hash32(&req.preimage, "preimage")?
    };

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::NotFound("Swap state not found".into()))?;
    if swap.vtr_refund_txid.is_some() {
        return Err(RpcError::BadRequest("VTR refund already submitted".into()));
    }
    if let Some(txid) = swap.vtr_claim_txid {
        if swap.vtr_claim_conflicted && swap.vtr_claim_bumps < 3 {
            if let Some(old_tx) = swap.vtr_claim_tx.as_ref() {
                let previous_fee = old_tx
                    .outputs
                    .first()
                    .map(|output| order.vtr_amount.saturating_sub(output.value))
                    .unwrap_or(vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS);
                let bumped = previous_fee
                    .saturating_mul(3)
                    .saturating_div(2)
                    .max(vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS.saturating_mul(2))
                    .min(order.vtr_amount.saturating_sub(546));
                if bumped > previous_fee {
                    if let Ok(claim_tx) = vtorrent_wallet_service::build_vtr_htlc_claim(
                        vtorrent_wallet_service::VtrClaimParams {
                            hash_lock,
                            taker_address: &taker_address,
                            maker_address: &order.maker_address,
                            expiry: order.expiry,
                            vtr_amount: order.vtr_amount,
                            funding_txid,
                            preimage,
                            taker_wif: &taker_wif,
                            fee_satoshis: bumped,
                        },
                    ) {
                        if crate::swap_recovery::validate_vtr(state, &claim_tx)
                            .await
                            .is_ok()
                        {
                            let new_txid = claim_tx.txid();
                            swap.vtr_claim_txid = Some(new_txid);
                            swap.vtr_claim_tx = Some(claim_tx.clone());
                            swap.vtr_claim_bumps = swap.vtr_claim_bumps.saturating_add(1);
                            swap.vtr_claim_conflicted = false;
                            swap.refresh_status();
                            crate::swap_recovery::persist_vtr_claim_replacement(
                                state, &order, swap,
                            )
                            .await?;
                            let admitted =
                                crate::swap_recovery::submit_vtr(state, &claim_tx).await?;
                            if !admitted {
                                swap.vtr_claim_conflicted = true;
                                crate::swap_recovery::persist(state, &order, Some(swap)).await?;
                            }
                            return Ok(SwapActionResponse {
                                order_id: req.order_id,
                                txid: hex::encode(new_txid),
                                status: "VtrClaimSubmitted".into(),
                            });
                        }
                    }
                }
            }
        }
        let transaction = swap
            .vtr_claim_tx
            .as_ref()
            .ok_or_else(|| RpcError::BadRequest("Signed VTR claim recovery data missing".into()))?
            .clone();
        crate::swap_recovery::persist(state, &order, Some(swap)).await?;
        let admitted = crate::swap_recovery::submit_vtr(state, &transaction).await?;
        if !admitted {
            swap.vtr_claim_conflicted = true;
            crate::swap_recovery::persist(state, &order, Some(swap)).await?;
        }
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
            taker_wif: &taker_wif,
            fee_satoshis: vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS,
        })
        .map_err(RpcError::BadRequest)?;
    let claim_txid = claim_tx.txid();
    crate::swap_recovery::validate_vtr(state, &claim_tx).await?;

    swap.preimage = Some(preimage);
    swap.vtr_claim_txid = Some(claim_txid);
    swap.vtr_claim_tx = Some(claim_tx.clone());
    swap.vtr_claim_conflicted = false;
    swap.refresh_status();
    crate::swap_recovery::persist(state, &order, Some(swap)).await?;
    let admitted = crate::swap_recovery::submit_vtr(state, &claim_tx).await?;
    if !admitted {
        swap.vtr_claim_conflicted = true;
        crate::swap_recovery::persist(state, &order, Some(swap)).await?;
    }

    Ok(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(claim_txid),
        status: "VtrClaimSubmitted".to_string(),
    })
}

/// POST /api/v1/swap/vtr-claim-external
///
/// Lets a taker node that does not hold the maker's order book claim a VTR
/// HTLC from its exact funding terms. The chain output, script, amount,
/// preimage, and signing key are all checked before broadcast.
pub async fn vtr_claim_external(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExternalVtrClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    let hash_lock = parse_hash32(&req.hash_lock, "hash lock")?;
    let preimage = parse_hash32(&req.preimage, "preimage")?;
    let funding_txid = parse_hash32(&req.funding_txid, "funding txid")?;
    vtorrent_core::address::validate_p2pkh(&req.taker_address)
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    vtorrent_core::address::validate_p2pkh(&req.maker_address)
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    use sha2::{Digest, Sha256};
    if Sha256::digest(preimage).as_slice() != hash_lock {
        return Err(RpcError::BadRequest(
            "Preimage does not match hash lock".into(),
        ));
    }
    let htlc = vtorrent_node::atomic_swap::Htlc::with_expiry(
        hash_lock,
        req.taker_address.clone(),
        req.maker_address.clone(),
        req.expiry,
        req.vtr_amount,
    )
    .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    let expected_script = htlc
        .build_script()
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    {
        let chain = state.chain.lock().await;
        let utxo = chain.get_utxo(&funding_txid, 0).ok_or_else(|| {
            RpcError::BadRequest("VTR funding output is missing or already spent".into())
        })?;
        if utxo.value != req.vtr_amount {
            return Err(RpcError::BadRequest(format!(
                "VTR funding amount mismatch: chain has {}, request has {}",
                utxo.value, req.vtr_amount
            )));
        }
        if utxo.script_pubkey != expected_script {
            return Err(RpcError::BadRequest(
                "VTR funding output does not match the supplied HTLC terms".into(),
            ));
        }
    }
    let taker_wif = if req.taker_wif.is_empty() {
        verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?
    } else {
        req.taker_wif.clone()
    };
    let key = vtorrent_core::keys::PrivateKey::from_wif(&taker_wif)
        .map_err(|error| RpcError::BadRequest(error.to_string()))?;
    let derived = vtorrent_core::address::Address::from_pubkey(
        &key.public_key()
            .map_err(|error| RpcError::BadRequest(error.to_string()))?,
        key.is_compressed(),
        vtorrent_core::network::legacy::PUBKEY_ADDRESS_PREFIX,
    )
    .to_string();
    if derived != req.taker_address {
        return Err(RpcError::BadRequest(
            "Signing key does not match the taker address".into(),
        ));
    }
    let claim =
        vtorrent_wallet_service::build_vtr_htlc_claim(vtorrent_wallet_service::VtrClaimParams {
            hash_lock,
            taker_address: &req.taker_address,
            maker_address: &req.maker_address,
            expiry: req.expiry,
            vtr_amount: req.vtr_amount,
            funding_txid,
            preimage,
            taker_wif: &taker_wif,
            fee_satoshis: vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS,
        })
        .map_err(RpcError::BadRequest)?;
    let txid = claim.txid();
    crate::swap_recovery::validate_vtr(&state, &claim).await?;
    crate::swap_recovery::submit_vtr(&state, &claim).await?;
    Ok(Json(SwapActionResponse {
        order_id: if req.order_id.is_empty() {
            hex::encode(funding_txid)
        } else {
            req.order_id
        },
        txid: hex::encode(txid),
        status: "VtrClaimSubmitted".into(),
    }))
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
    crate::handlers::authorize_wallet_operation(state, &req.passphrase, req.otp_code.as_deref())
        .await?;
    btc_claim_authorized(state, req).await
}

/// Claim the BTC leg after the caller has already authorized the wallet.
pub async fn btc_claim_authorized(
    state: &AppState,
    req: BtcClaimRequest,
) -> RpcResult<SwapActionResponse> {
    claim_btc_with_verification(
        state,
        req,
        async |htlc: &vtorrent_btc::htlc::BtcHtlc, txid| {
            let peers = crate::btc_peers::resolve_btc_peers(state, htlc.network).await?;
            let btc = state.btc_wallet.read().await;
            let wallet = btc
                .as_ref()
                .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
            wallet
                .verify_swap_funding(htlc, txid, &peers, now_secs_mock(state).await)
                .await
                .map_err(|e| RpcError::BadRequest(format!("BTC funding verification failed: {e}")))
        },
        async |raw: &[u8]| broadcast_btc(state, raw).await,
    )
    .await
}

async fn claim_btc_with_verification(
    state: &AppState,
    req: BtcClaimRequest,
    verify: impl AsyncFnOnce(&vtorrent_btc::htlc::BtcHtlc, [u8; 32]) -> RpcResult<()>,
    broadcast: impl AsyncFnOnce(&[u8]) -> RpcResult<[u8; 32]>,
) -> RpcResult<SwapActionResponse> {
    let order = state
        .order_book
        .read()
        .await
        .get_order(&req.order_id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Swap order not found".into()))?;
    let mut swaps = state.swaps.write().await;
    let (preimage, btc_funding_txid, maker_btc_address, btc_amount, btc_expiry, refund_address) = {
        let swap = swaps
            .get_mut(&req.order_id)
            .ok_or_else(|| RpcError::NotFound(format!("Swap {} not found", req.order_id)))?;
        if swap.btc_refund_txid.is_some() {
            return Err(RpcError::BadRequest("BTC refund already submitted".into()));
        }
        if let Some(refund_txid) = swap.vtr_refund_txid {
            let chain = state.chain.lock().await;
            let refund_confirmed = chain.get_transaction_owned(&refund_txid).is_some();
            let mempool = state.mempool.lock().await;
            let refund_pending = mempool.get_transaction(&refund_txid).is_some();
            drop(mempool);
            drop(chain);
            if refund_confirmed || refund_pending {
                return Err(RpcError::BadRequest(
                    "VTR refund already submitted; refusing to reveal the swap secret".into(),
                ));
            }
            if swap.vtr_refund_broadcast || !swap.vtr_refund_replacements.is_empty() {
                return Err(RpcError::BadRequest(
                    "VTR refund may still confirm; refusing to reveal the swap secret".into(),
                ));
            }
            // The recorded refund was never relayed and has no replacements, so
            // it cannot confirm. Clear the latch instead of blocking the claim.
            swap.vtr_refund_txid = None;
            swap.vtr_refund_tx = None;
            swap.refresh_status();
            crate::swap_recovery::persist_failed_refund_clear(state, &order, swap).await?;
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
            None => order
                .preimage
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

    let hash_lock = order
        .hash_lock
        .ok_or_else(|| RpcError::BadRequest("Order hash lock missing".into()))?;
    {
        use sha2::{Digest, Sha256};
        if Sha256::digest(preimage).as_slice() != hash_lock
            || swaps[&req.order_id].hash_lock != hash_lock
            || order.maker_btc_address.as_deref() != Some(&maker_btc_address)
            || order.target_amount != btc_amount
            || btc_expiry < 500_000_000
            || btc_expiry
                .checked_add(vtorrent_wallet_service::swap_policy::SWAP_CLAIM_SAFETY_MARGIN)
                .is_none_or(|expiry| expiry > order.expiry)
        {
            return Err(RpcError::BadRequest(
                "BTC claim terms do not match the order or safe expiry policy".into(),
            ));
        }
    }
    let network = *state.btc_network.read().await;
    let htlc = vtorrent_btc::htlc::BtcHtlc {
        hash_lock,
        recipient: maker_btc_address.clone(),
        refund_address: refund_address.clone(),
        expiry: btc_expiry,
        amount: btc_amount,
        network,
    };
    verify(&htlc, btc_funding_txid).await?;
    // Network verification may take long enough to cross the safe claim deadline.
    let now = now_secs_mock(state).await;
    if now.saturating_add(u64::from(
        vtorrent_wallet_service::swap_policy::MIN_BTC_SWAP_WINDOW,
    )) >= u64::from(btc_expiry)
    {
        return Err(RpcError::BadRequest(
            "BTC claim window expired during funding verification".into(),
        ));
    }
    {
        let chain = state.chain.lock().await;
        vtorrent_wallet_service::swap_policy::verify_vtr_swap_funding(&chain, &order, now)
            .map_err(RpcError::BadRequest)?;
    }

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
                network,
            },
        )
        .map_err(RpcError::BadRequest)?
    };
    let swap = swaps
        .get_mut(&req.order_id)
        .ok_or_else(|| RpcError::Internal("Swap state disappeared".into()))?;
    swap.btc_claim_txid = Some(txid);
    // Persist the signed claim so a stalled broadcast can be rebuilt at a
    // higher fee and replaced (the claim is RBF-signalling).
    swap.btc_claim_raw = Some(raw.clone());
    swap.refresh_status();
    crate::swap_recovery::persist(state, &order, Some(swap)).await?;
    broadcast(&raw).await?;

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
    if state.wallet_encrypted.read().await.is_some() {
        crate::handlers::authorize_wallet_operation(
            state,
            req.passphrase.as_str(),
            req.otp_code.as_deref(),
        )
        .await?;
    }
    swap_refund_authorized(state, req).await
}

/// Refund after the caller has already authorized the wallet.
pub async fn swap_refund_authorized(
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
            // Once the maker has taken the BTC leg the preimage is public and
            // the VTR leg belongs to the taker. Refunding it would keep both.
            if swap.preimage.is_some() || swap.btc_claim_txid.is_some() {
                return Err(RpcError::BadRequest(
                    "BTC leg already claimed; refusing to refund the VTR leg".into(),
                ));
            }
            // A taker claim can be pending in the mempool or already confirmed.
            // Refunding over it can strand the taker's BTC.
            if swap.vtr_observation.as_ref().is_some_and(|observation| {
                observation.state == vtorrent_node::atomic_swap::VtrSettlementState::Claimed
            }) {
                return Err(RpcError::BadRequest(
                    "VTR HTLC was already claimed; refusing to refund".into(),
                ));
            }
            if let Some(funding) = swap.vtr_funding_txid {
                let mempool = state.mempool.lock().await;
                let transactions = mempool.get_transactions();
                let claimed = crate::swap_reconciliation::any_pending_claim(
                    transactions.iter(),
                    &funding,
                    &swap.hash_lock,
                );
                drop(mempool);
                if claimed {
                    return Err(RpcError::BadRequest(
                        "A VTR claim is pending; refusing to refund".into(),
                    ));
                }
            }
            if swap.vtr_claim_txid.is_some() {
                return Err(RpcError::BadRequest("VTR claim already submitted".into()));
            }
            if swap.vtr_refund_txid.is_some() {
                let order = order
                    .as_ref()
                    .ok_or_else(|| RpcError::NotFound("VTR order metadata missing".into()))?;
                crate::swap_recovery::persist(state, order, Some(swap)).await?;
                let txid = crate::refund_bump::submit_latest(state, swap).await?;
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
            crate::swap_recovery::validate_vtr(state, &refund).await?;
            swap.vtr_refund_txid = Some(txid);
            swap.vtr_refund_tx = Some(refund.clone());
            // Treat the refund as relayed before submission: a crash between
            // the two must leave the safe latch rather than a clearable record.
            swap.vtr_refund_broadcast = true;
            swap.refresh_status();
            crate::swap_recovery::persist(state, &order, Some(swap)).await?;
            if let Err(error) = crate::swap_recovery::submit_vtr_required(state, &refund).await {
                // A refund that could not be admitted must not stay recorded:
                // it would block the BTC claim on this swap.
                swap.vtr_refund_txid = None;
                swap.vtr_refund_tx = None;
                swap.vtr_refund_broadcast = false;
                swap.refresh_status();
                let _ =
                    crate::swap_recovery::persist_failed_refund_clear(state, &order, swap).await;
                return Err(error);
            }
            hex::encode(txid)
        }
        SwapLeg::Btc => {
            if swap.btc_claim_txid.is_some() {
                return Err(RpcError::BadRequest("BTC claim already submitted".into()));
            }
            // The maker's claim reveals the preimage. Refunding BTC after that
            // point lets the taker keep both legs if the claim later reorgs.
            if swap.preimage.is_some()
                || swap.btc_observation.as_ref().is_some_and(|observation| {
                    matches!(
                        observation.state,
                        vtorrent_node::atomic_swap::BtcSettlementState::Claimed
                            | vtorrent_node::atomic_swap::BtcSettlementState::ClaimConfirming
                    )
                })
            {
                return Err(RpcError::BadRequest(
                    "BTC claim has been observed; a BTC refund would double-spend the swap".into(),
                ));
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
                        vtorrent_node::atomic_swap::btc_htlc_fee_satoshis(),
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
    let mut swaps = state.swaps.write().await;
    if let Some(existing) = swaps.get_mut(order_id) {
        if existing.hash_lock != recovered.hash_lock
            || existing
                .btc_funding_txid
                .is_some_and(|id| Some(id) != recovered.btc_funding_txid)
        {
            return Err(RpcError::BadRequest(
                "BTC recovery contract conflicts with swap state".into(),
            ));
        }
        if existing.btc_funding_txid.is_none() {
            existing.btc_funding_txid = recovered.btc_funding_txid;
            existing.maker_btc_address = recovered.maker_btc_address;
            existing.taker_btc_refund_address = recovered.taker_btc_refund_address;
            existing.btc_amount = recovered.btc_amount;
            existing.btc_expiry = recovered.btc_expiry;
            existing.btc_funding_raw = recovered.btc_funding_raw;
        }
        if existing.btc_refund_raw.is_none() && recovered.btc_refund_raw.is_some() {
            existing.btc_refund_raw = recovered.btc_refund_raw;
            existing.btc_refund_txid = recovered.btc_refund_txid;
            existing.refresh_status();
        }
    } else {
        swaps.insert(order_id.to_owned(), recovered);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
