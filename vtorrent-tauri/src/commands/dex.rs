use serde::Serialize;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct DexOrderResult {
    pub id: String,
    pub maker_address: String,
    pub offer_amount_satoshis: u64,
    pub offer_asset: String,
    pub request_amount_satoshis: u64,
    pub request_asset: String,
    pub rate: f64,
    pub status: String,
    pub created_at: u32,
    pub expires_at: u32,
}

#[derive(Debug, Serialize)]
pub struct SwapActionResult {
    pub order_id: String,
    pub txid: String,
    pub status: String,
}

// ─── DEX order book commands ────────────────────────────────────────────────

#[tauri::command]
pub async fn get_dex_orders(state: tauri::State<'_, AppState>) -> Result<Vec<DexOrderResult>> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let order_book = handle.rpc_state.order_book.read().await;
    Ok(order_book
        .list_open_orders()
        .into_iter()
        .map(|o| DexOrderResult {
            id: hex::encode(o.order_id),
            maker_address: o.maker_address.clone(),
            offer_amount_satoshis: o.vtr_amount,
            offer_asset: "VTR".into(),
            request_amount_satoshis: o.target_amount,
            request_asset: o.target_asset.clone(),
            rate: if o.target_amount > 0 {
                o.vtr_amount as f64 / o.target_amount as f64
            } else {
                0.0
            },
            status: format!("{:?}", o.status),
            created_at: 0,
            expires_at: o.expiry,
        })
        .collect())
}

#[tauri::command]
pub async fn place_dex_order(
    state: tauri::State<'_, AppState>,
    maker_address: String,
    maker_btc_address: Option<String>,
    vtr_amount: u64,
    target_asset: String,
    target_amount: u64,
) -> Result<DexOrderResult> {
    use vtorrent_node::atomic_swap::{AtomicSwap, SwapOrder, MAX_HTLC_LOCKTIME, MIN_HTLC_LOCKTIME};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    // Validation mirrors the RPC handler (previously the desktop app accepted
    // zero-amount orders, invalid maker addresses, and unbounded expiry —
    // producing orders that could never be funded or matched).
    if !rpc.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    if vtr_amount == 0 || target_amount == 0 {
        return Err(TauriError::InvalidInput(format!(
            "DEX order amounts must be greater than zero (offer: {} sats, request: {} sats)",
            vtr_amount, target_amount
        )));
    }
    if target_asset.trim().is_empty() {
        return Err(TauriError::InvalidInput(
            "Requested asset is required — specify the target asset (e.g. \"BTC\")".into(),
        ));
    }
    if vtorrent_core::address::validate_p2pkh(&maker_address).is_err() {
        return Err(TauriError::InvalidInput(format!(
            "Invalid maker address: {}",
            maker_address
        )));
    }
    // A malformed BTC address is only rejected during btc_fund — after the
    // maker's VTR is already locked in the HTLC. Reject it up front.
    if let Some(btc_addr) = &maker_btc_address {
        if btc_addr
            .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>()
            .is_err()
        {
            return Err(TauriError::InvalidInput(format!(
                "Invalid maker BTC address: {}",
                btc_addr
            )));
        }
    }
    let locktime = 86400u32;
    if !(MIN_HTLC_LOCKTIME..=MAX_HTLC_LOCKTIME).contains(&locktime) {
        return Err(TauriError::InvalidInput(
            "DEX order expiry outside valid range".into(),
        ));
    }

    // Seed the order with a preimage/hash_lock so the HTLC legs can be built
    // (orders without a hash lock strand the VTR funding at btc_fund time).
    let swap = AtomicSwap::new();
    let mut order = SwapOrder::new(
        maker_address,
        vtr_amount,
        target_asset,
        target_amount,
        locktime,
    );
    order.hash_lock = Some(swap.hash_lock);
    order.preimage = Some(swap.preimage);
    order.maker_btc_address = maker_btc_address;
    let result = DexOrderResult {
        id: hex::encode(order.order_id),
        maker_address: order.maker_address.clone(),
        offer_amount_satoshis: order.vtr_amount,
        offer_asset: "VTR".into(),
        request_amount_satoshis: order.target_amount,
        request_asset: order.target_asset.clone(),
        rate: if order.target_amount > 0 {
            order.vtr_amount as f64 / order.target_amount as f64
        } else {
            0.0
        },
        status: format!("{:?}", order.status),
        created_at: 0,
        expires_at: order.expiry,
    };
    rpc.order_book.write().await.add_order(order);
    Ok(result)
}

#[tauri::command]
pub async fn cancel_dex_order(state: tauri::State<'_, AppState>, order_id: String) -> Result<bool> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    // Ownership check (mirrors the RPC handler): without this, any code path
    // in the desktop app could cancel third-party orders.
    if !rpc.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    let wallet_address = rpc
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| TauriError::Internal("Change address not set".into()))?;
    let is_maker = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .map(|o| o.maker_address == wallet_address)
            .unwrap_or(false)
    };
    if !is_maker {
        return Err(TauriError::Unauthorized(
            "Only the maker may cancel this order".into(),
        ));
    }

    let mut order_book = rpc.order_book.write().await;
    Ok(order_book.cancel_order(&order_id))
}

// ─── Swap lifecycle commands ─────────────────────────────────────────────────

#[tauri::command]
pub async fn match_dex_order(
    state: tauri::State<'_, AppState>,
    order_id: String,
    taker_address: String,
    _passphrase: String,
    _otp_code: Option<String>,
) -> Result<vtorrent_rpc::models::MatchOrderResponse> {
    use vtorrent_node::atomic_swap::{
        AtomicSwap, Htlc, SwapState, SwapStatus, MAX_HTLC_LOCKTIME, MIN_HTLC_LOCKTIME,
    };
    use vtorrent_wallet::tx_builder::sign_custom_transaction;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    if !rpc.is_wallet_unlocked().await {
        return Err(TauriError::WalletLocked);
    }
    if taker_address.trim().is_empty() {
        return Err(TauriError::InvalidInput("Taker address is required".into()));
    }
    vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&taker_address)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid taker address: {}", e)))?;

    let wif = rpc
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or(TauriError::WalletLocked)?;
    let wallet_address = rpc
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| TauriError::Internal("Change address not set".into()))?;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .filter(|o| matches!(o.status, vtorrent_node::atomic_swap::OrderStatus::Open))
            .cloned()
            .ok_or_else(|| {
                TauriError::NotFound(format!("Order {} not found or not open", order_id))
            })?
    };
    if order.maker_address != wallet_address {
        return Err(TauriError::Unauthorized(
            "Only the maker's imported wallet may fund this order".into(),
        ));
    }

    // Honor the regtest mock clock when set (mirrors the RPC handler).
    let now = {
        let mock = rpc.mock_time.read().await;
        match *mock {
            Some(t) => t as u32,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32,
        }
    };
    let remaining_locktime = order.expiry.saturating_sub(now);
    if remaining_locktime < MIN_HTLC_LOCKTIME {
        return Err(TauriError::InvalidInput(
            "DEX order is too close to expiry to fund safely".into(),
        ));
    }
    if remaining_locktime > MAX_HTLC_LOCKTIME {
        return Err(TauriError::InvalidInput(
            "DEX order expiry exceeds maximum locktime".into(),
        ));
    }
    let (preimage, hash_lock) = match (order.preimage, order.hash_lock) {
        (Some(p), Some(h)) => (p, h),
        _ => {
            let swap = AtomicSwap::new();
            (swap.preimage, swap.hash_lock)
        }
    };
    let htlc = Htlc::with_expiry(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to construct HTLC: {}", e)))?;

    const FUNDING_FEE_SATOSHIS: u64 = vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
    // Reserve the order BEFORE building/signing the funding transaction: two
    // concurrent match calls would otherwise both select the same UTXO and
    // both admit competing funding txs to the mempool (mirrors the RPC
    // handler). Reserving first means the loser exits here without touching
    // the mempool.
    let reserved = rpc.order_book.write().await.begin_funding(&order_id);
    if reserved.is_none() {
        return Err(TauriError::NotFound(format!(
            "Order {} is no longer open",
            order_id
        )));
    }
    let funding_tx = match (async {
        let funding_utxo = {
            let chain = rpc.chain.lock().await;
            chain
                .get_utxos_for_address(&wallet_address)
                .into_iter()
                .filter(|utxo| utxo.value >= order.vtr_amount.saturating_add(FUNDING_FEE_SATOSHIS))
                .max_by_key(|utxo| utxo.value)
                .ok_or_else(|| {
                    TauriError::InvalidInput("No single wallet UTXO can fund this HTLC".into())
                })?
        };
        let unsigned_funding = htlc
            .build_funding_tx(
                funding_utxo.txid,
                funding_utxo.vout,
                funding_utxo.value,
                FUNDING_FEE_SATOSHIS,
            )
            .map_err(|e| {
                TauriError::InvalidInput(format!("Unable to build HTLC funding tx: {}", e))
            })?;
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), &wif)
            .map_err(|e| TauriError::InvalidInput(format!("Unable to sign HTLC funding tx: {}", e)))
    })
    .await
    {
        Ok(tx) => tx,
        Err(e) => {
            rpc.order_book.write().await.release_funding(&order_id);
            return Err(e);
        }
    };
    let funding_txid = funding_tx.txid();

    let admission = {
        let mut mempool = rpc.mempool.lock().await;
        mempool.add_transaction_with_fee(funding_tx.clone(), FUNDING_FEE_SATOSHIS)
    };
    if let Err(e) = admission {
        rpc.order_book.write().await.release_funding(&order_id);
        return Err(TauriError::InvalidInput(format!(
            "Mempool rejected HTLC funding transaction: {}",
            e
        )));
    }
    let matched = match rpc.order_book.write().await.fund_and_match_order(
        &order_id,
        taker_address,
        preimage,
        hash_lock,
        funding_txid,
    ) {
        Some(m) => m,
        None => {
            // Release the reservation so the order stays retryable; without
            // this it is stuck in Funding forever (invisible to open-order
            // listings and unmatchable).
            rpc.order_book.write().await.release_funding(&order_id);
            return Err(TauriError::Internal(
                "Funding reservation disappeared".into(),
            ));
        }
    };

    if let Some(sender) = &rpc.tx_submit {
        let _ = sender.try_send(funding_tx);
    }

    // Materialize the swap state in VtrFunded stage so lifecycle guards on
    // btc-fund / claims / refunds operate from a known baseline. Without
    // this, the desktop btc_fund always fails with "VTR leg not funded yet".
    {
        let mut swaps = rpc.swaps.write().await;
        let swap = swaps
            .entry(hex::encode(matched.order.order_id))
            .or_insert_with(|| SwapState::new(matched.order.order_id, matched.hash_lock));
        if swap.vtr_funding_txid.is_none() {
            swap.vtr_funding_txid = Some(funding_txid);
            swap.status = SwapStatus::VtrFunded;
        }
    }

    Ok(vtorrent_rpc::models::MatchOrderResponse {
        order_id: hex::encode(matched.order.order_id),
        maker_address: matched.order.maker_address,
        vtr_amount: matched.order.vtr_amount,
        target_asset: matched.order.target_asset,
        target_amount: matched.order.target_amount,
        hash_lock: hex::encode(matched.hash_lock),
        expiry: matched.order.expiry,
        funding_txid: hex::encode(funding_txid),
    })
}

#[tauri::command]
pub async fn btc_fund(
    state: tauri::State<'_, AppState>,
    order_id: String,
    btc_refund_address: String,
) -> Result<SwapActionResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let result = vtorrent_rpc::handlers::btc_fund_with_state(
        &handle.rpc_state,
        vtorrent_rpc::models::BtcFundRequest {
            order_id,
            btc_refund_address,
        },
    )
    .await
    .map_err(TauriError::from)?;
    Ok(SwapActionResult {
        order_id: result.order_id,
        txid: result.btc_funding_txid,
        status: result.status,
    })
}

#[tauri::command]
pub async fn vtr_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
    preimage: String,
    taker_wif: String,
) -> Result<SwapActionResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let result = vtorrent_rpc::handlers::vtr_claim_with_state(
        &handle.rpc_state,
        vtorrent_rpc::models::VtrClaimRequest {
            order_id,
            preimage,
            taker_wif: taker_wif.into(),
        },
    )
    .await
    .map_err(TauriError::from)?;
    Ok(SwapActionResult {
        order_id: result.order_id,
        txid: result.txid,
        status: result.status,
    })
}

#[tauri::command]
pub async fn btc_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<SwapActionResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let result = vtorrent_rpc::handlers::btc_claim_with_state(
        &handle.rpc_state,
        vtorrent_rpc::models::BtcClaimRequest { order_id },
    )
    .await
    .map_err(TauriError::from)?;
    Ok(SwapActionResult {
        order_id: result.order_id,
        txid: result.txid,
        status: result.status,
    })
}

#[tauri::command]
pub async fn swap_refund(
    state: tauri::State<'_, AppState>,
    order_id: String,
    leg: Option<vtorrent_rpc::models::SwapLeg>,
) -> Result<SwapActionResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let result = vtorrent_rpc::handlers::swap_refund_with_state(
        &handle.rpc_state,
        vtorrent_rpc::models::SwapRefundRequest { order_id, leg },
    )
    .await
    .map_err(TauriError::from)?;
    Ok(SwapActionResult {
        order_id: result.order_id,
        txid: result.txid,
        status: result.status,
    })
}
