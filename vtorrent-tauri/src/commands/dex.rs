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
    let swaps = handle.rpc_state.swaps.read().await;
    Ok(order_book
        .list_orders()
        .into_iter()
        .filter(|o| {
            o.status == vtorrent_node::atomic_swap::OrderStatus::Open
                || swaps.contains_key(&hex::encode(o.order_id))
        })
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
    state.sync_swap_wallet(rpc).await?;

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
    vtorrent_rpc::swap_recovery::persist(rpc, &order, None)
        .await
        .map_err(TauriError::from)?;
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
    state.sync_swap_wallet(rpc).await?;

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
    if let Some(mut order) = order_book.get_order(&order_id).cloned() {
        if order.status == vtorrent_node::atomic_swap::OrderStatus::Open {
            order.status = vtorrent_node::atomic_swap::OrderStatus::Cancelled;
            vtorrent_rpc::swap_recovery::persist(rpc, &order, None)
                .await
                .map_err(TauriError::from)?;
        }
    }
    Ok(order_book.cancel_order(&order_id))
}

// ─── Swap lifecycle commands ─────────────────────────────────────────────────

#[tauri::command]
pub async fn get_swap_status(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<vtorrent_rpc::swap_reconciliation::SwapStatusResponse> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let vtr = vtorrent_rpc::swap_reconciliation::status(&handle.rpc_state, &order_id)
        .await
        .map_err(TauriError::from)?;
    let btc = handle
        .rpc_state
        .swaps
        .read()
        .await
        .get(&order_id)
        .and_then(|swap| swap.btc_observation.clone());
    Ok(vtorrent_rpc::swap_reconciliation::SwapStatusResponse {
        btc,
        order_id,
        vtr,
        btc_reconciled: false,
    })
}

#[tauri::command]
pub async fn reconcile_btc_swap(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<vtorrent_node::atomic_swap::BtcSwapObservation> {
    let rpc = state
        .node
        .lock()
        .await
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?
        .rpc_state
        .clone();
    state.sync_swap_wallet(&rpc).await?;
    vtorrent_rpc::btc_reconciliation::reconcile(&rpc, &order_id)
        .await
        .map_err(TauriError::from)
}

#[tauri::command]
pub async fn match_dex_order(
    state: tauri::State<'_, AppState>,
    order_id: String,
    taker_address: String,
    _passphrase: String,
    _otp_code: Option<String>,
) -> Result<vtorrent_rpc::models::MatchOrderResponse> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;
    state.sync_swap_wallet(rpc).await?;
    let wif = rpc
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or(TauriError::WalletLocked)?;
    vtorrent_rpc::handlers::match_dex_order_with_wif(
        rpc,
        vtorrent_rpc::models::MatchOrderRequest {
            order_id,
            taker_address,
            passphrase: _passphrase.into(),
            otp_code: _otp_code,
        },
        &wif,
    )
    .await
    .map_err(TauriError::from)
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
    state.sync_swap_wallet(&handle.rpc_state).await?;
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
    state.sync_swap_wallet(&handle.rpc_state).await?;
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
    state.sync_swap_wallet(&handle.rpc_state).await?;
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
    let has_wallet = state
        .wallet
        .lock()
        .map_err(|_| TauriError::WalletLocked)?
        .is_some();
    if has_wallet {
        state.sync_swap_wallet(&handle.rpc_state).await?;
    }
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
