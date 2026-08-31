use serde::Serialize;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

use vtorrent_rpc::handlers::btc_txid_hex;

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
    use vtorrent_node::atomic_swap::SwapOrder;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let mut order_book = handle.rpc_state.order_book.write().await;

    let mut order = SwapOrder::new(
        maker_address,
        vtr_amount,
        target_asset,
        target_amount,
        86400,
    );
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
    order_book.add_order(order);
    Ok(result)
}

#[tauri::command]
pub async fn cancel_dex_order(state: tauri::State<'_, AppState>, order_id: String) -> Result<bool> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let mut order_book = handle.rpc_state.order_book.write().await;
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
    use vtorrent_node::atomic_swap::{AtomicSwap, Htlc, MIN_HTLC_LOCKTIME};
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
    let (preimage, hash_lock) = match (order.preimage, order.hash_lock) {
        (Some(p), Some(h)) => (p, h),
        _ => {
            let swap = AtomicSwap::new();
            (swap.preimage, swap.hash_lock)
        }
    };
    let htlc = Htlc::new(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        remaining_locktime,
        order.vtr_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to construct HTLC: {}", e)))?;

    const FUNDING_FEE_SATOSHIS: u64 = vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
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
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build HTLC funding tx: {}", e)))?;
    let funding_tx =
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), &wif)
            .map_err(|e| {
                TauriError::InvalidInput(format!("Unable to sign HTLC funding tx: {}", e))
            })?;
    let funding_txid = funding_tx.txid();

    let reserved = rpc.order_book.write().await.begin_funding(&order_id);
    if reserved.is_none() {
        return Err(TauriError::NotFound(format!(
            "Order {} is no longer open",
            order_id
        )));
    }
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
    let matched = rpc
        .order_book
        .write()
        .await
        .fund_and_match_order(&order_id, taker_address, preimage, hash_lock, funding_txid)
        .ok_or_else(|| TauriError::Internal("Funding reservation disappeared".into()))?;

    if let Some(sender) = &rpc.tx_submit {
        let _ = sender.try_send(funding_tx);
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
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| TauriError::InvalidInput("Order has no hash lock".into()))?;
    let maker_btc_address = order
        .maker_btc_address
        .clone()
        .ok_or_else(|| TauriError::InvalidInput("Order has no maker BTC address".into()))?;

    // Lifecycle guard (mirrors the RPC handler): the VTR leg must be funded
    // first — locking BTC into an HTLC for an unfunded order would strand the
    // taker's BTC until refund. A finished swap can never be funded again.
    {
        let swaps = rpc.swaps.read().await;
        let swap = swaps.get(&order_id).ok_or_else(|| {
            TauriError::InvalidInput("VTR leg not funded yet — fund the order first".into())
        })?;
        if swap.status != SwapStatus::VtrFunded {
            return Err(TauriError::InvalidInput(format!(
                "Swap is in state {:?}; BTC funding requires VtrFunded",
                swap.status
            )));
        }
    }

    let btc_amount = order.target_amount;
    if btc_amount == 0 {
        return Err(TauriError::InvalidInput(
            "Order target amount is zero".into(),
        ));
    }

    let (btc_funding_txid, btc_expiry) = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        // Broadcast hook: honor the daemon's configured BTC peer when set
        // (mirrors the RPC handler; previously the desktop app broadcast to
        // public seeds only).
        let peer = rpc.btc_peer.read().await.clone();
        let network = *rpc.btc_network.read().await;
        vtorrent_wallet_service::build_btc_htlc_funding(
            w,
            hash_lock,
            &maker_btc_address,
            &btc_refund_address,
            btc_amount,
            async |raw: &[u8]| {
                if let Some(host) = peer.clone() {
                    let addr = tokio::net::lookup_host(&host)
                        .await
                        .ok()
                        .and_then(|mut it| it.next())
                        .ok_or_else(|| format!("BTC peer {} DNS resolution failed", host))?;
                    vtorrent_btc::sync::broadcast_tx_to(raw, network, &[addr])
                        .await
                        .map_err(|e| format!("BTC broadcast to {} failed: {}", host, e))
                } else {
                    vtorrent_btc::sync::broadcast_tx(raw)
                        .await
                        .map_err(|e| format!("BTC broadcast failed: {}", e))
                }
            },
        )
        .await
        .map_err(TauriError::InvalidInput)?
    };

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.btc_funding_txid = Some(btc_funding_txid);
    swap.maker_btc_address = Some(maker_btc_address);
    swap.taker_btc_refund_address = Some(btc_refund_address);
    swap.btc_amount = btc_amount;
    swap.btc_expiry = btc_expiry;
    swap.status = SwapStatus::BtcFunded;

    Ok(SwapActionResult {
        order_id,
        txid: btc_txid_hex(&btc_funding_txid),
        status: "BtcFunded".to_string(),
    })
}

#[tauri::command]
pub async fn vtr_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
    preimage: String,
    taker_wif: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let preimage_bytes = {
        let bytes = hex::decode(&preimage)
            .map_err(|_| TauriError::InvalidInput("Invalid preimage hex".into()))?;
        if bytes.len() != 32 {
            return Err(TauriError::InvalidInput("Preimage must be 32 bytes".into()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    };
    if taker_wif.is_empty() {
        return Err(TauriError::InvalidInput("Taker WIF is required".into()));
    }

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| TauriError::InvalidInput("Order has no hash lock".into()))?;
    let funding_txid = order
        .funding_txid
        .ok_or_else(|| TauriError::InvalidInput("Order has no VTR funding txid".into()))?;
    let taker_address = order
        .taker_address
        .clone()
        .ok_or_else(|| TauriError::InvalidInput("Order has no taker address".into()))?;

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(preimage_bytes);
    let digest = hasher.finalize();
    if digest.as_slice() != hash_lock {
        return Err(TauriError::InvalidInput(
            "Preimage does not match hash lock".into(),
        ));
    }

    let claim_tx =
        vtorrent_wallet_service::build_vtr_htlc_claim(vtorrent_wallet_service::VtrClaimParams {
            hash_lock,
            taker_address: &taker_address,
            maker_address: &order.maker_address,
            expiry: order.expiry,
            vtr_amount: order.vtr_amount,
            funding_txid,
            preimage: preimage_bytes,
            taker_wif: &taker_wif,
        })
        .map_err(TauriError::InvalidInput)?;
    let claim_txid = claim_tx.txid();

    {
        let mut mempool = rpc.mempool.lock().await;
        mempool
            .add_transaction_with_fee(
                claim_tx.clone(),
                vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS,
            )
            .map_err(|e| {
                TauriError::InvalidInput(format!("Mempool rejected VTR claim tx: {}", e))
            })?;
    }
    if let Some(sender) = &rpc.tx_submit {
        let _ = sender.try_send(claim_tx);
    }

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.preimage = Some(preimage_bytes);
    swap.status = SwapStatus::Claimed;

    Ok(SwapActionResult {
        order_id,
        txid: hex::encode(claim_txid),
        status: "Claimed".to_string(),
    })
}

#[tauri::command]
pub async fn btc_claim(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::SwapStatus;

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let (preimage, btc_funding_txid, maker_btc_address, btc_amount, btc_expiry, refund_address) = {
        let swaps = rpc.swaps.read().await;
        let swap = swaps
            .get(&order_id)
            .ok_or_else(|| TauriError::NotFound(format!("Swap {} not found", order_id)))?;
        let preimage = match swap.preimage {
            Some(p) => p,
            None => {
                let order_book = rpc.order_book.read().await;
                order_book
                    .get_order(&order_id)
                    .and_then(|o| o.preimage)
                    .ok_or_else(|| TauriError::InvalidInput("Preimage not available".into()))?
            }
        };
        let btc_funding_txid = swap
            .btc_funding_txid
            .ok_or_else(|| TauriError::InvalidInput("BTC funding txid not recorded".into()))?;
        let maker_btc_address = swap
            .maker_btc_address
            .clone()
            .ok_or_else(|| TauriError::InvalidInput("Maker BTC address not recorded".into()))?;
        let refund_address = swap.taker_btc_refund_address.clone().ok_or_else(|| {
            TauriError::InvalidInput("Taker BTC refund address not recorded".into())
        })?;
        (
            preimage,
            btc_funding_txid,
            maker_btc_address,
            swap.btc_amount,
            swap.btc_expiry,
            refund_address,
        )
    };

    // Build and sign via the shared service path; broadcast honoring the
    // daemon's configured BTC peer (mirrors the RPC handler).
    let (raw, txid) = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        vtorrent_wallet_service::build_btc_htlc_claim(
            w,
            vtorrent_wallet_service::BtcClaimParams {
                funding_txid: btc_funding_txid,
                preimage,
                maker_btc_address: &maker_btc_address,
                refund_address: &refund_address,
                expiry: btc_expiry,
                amount: btc_amount,
                network: *rpc.btc_network.read().await,
            },
        )
        .map_err(TauriError::InvalidInput)?
    };
    {
        let peer = rpc.btc_peer.read().await.clone();
        let network = *rpc.btc_network.read().await;
        if let Some(host) = peer {
            let addr = tokio::net::lookup_host(&host)
                .await
                .ok()
                .and_then(|mut it| it.next())
                .ok_or_else(|| {
                    TauriError::Internal(format!("BTC peer {} DNS resolution failed", host))
                })?;
            vtorrent_btc::sync::broadcast_tx_to(&raw, network, &[addr])
                .await
                .map_err(|e| {
                    TauriError::Internal(format!("BTC broadcast to {} failed: {}", host, e))
                })?;
        } else {
            vtorrent_btc::sync::broadcast_tx(&raw)
                .await
                .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;
        }
    }

    {
        let mut swaps = rpc.swaps.write().await;
        if let Some(swap) = swaps.get_mut(&order_id) {
            swap.status = SwapStatus::Claimed;
        }
    }

    Ok(SwapActionResult {
        order_id,
        txid: btc_txid_hex(&txid),
        status: "Claimed".to_string(),
    })
}

#[tauri::command]
pub async fn swap_refund(
    state: tauri::State<'_, AppState>,
    order_id: String,
) -> Result<SwapActionResult> {
    use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let rpc = &handle.rpc_state;

    let order = {
        let order_book = rpc.order_book.read().await;
        order_book
            .get_order(&order_id)
            .cloned()
            .ok_or_else(|| TauriError::NotFound(format!("Order {} not found", order_id)))?
    };
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
    if now < order.expiry {
        return Err(TauriError::InvalidInput("Swap has not expired yet".into()));
    }

    // Lifecycle guard: each leg refunds independently; a completed refund is
    // the only terminal state for this endpoint (mirrors the RPC handler).
    {
        let swaps = rpc.swaps.read().await;
        if let Some(swap) = swaps.get(&order_id) {
            if swap.status == SwapStatus::Refunded {
                return Err(TauriError::InvalidInput(
                    "Swap is in state Refunded; operation not allowed".into(),
                ));
            }
        }
    }

    // ── VTR-side refund (the maker reclaims their VTR) ──────────────────────
    // Mirrors the RPC handler: the desktop app previously skipped this leg
    // entirely, so a desktop refund left the maker's VTR stranded in the HTLC.
    let vtr_refund_txid = {
        let (hash_lock, funding_txid, taker_address) = (
            order.hash_lock,
            order.funding_txid,
            order.taker_address.clone(),
        );
        match (hash_lock, funding_txid, taker_address) {
            (Some(hash_lock), Some(funding_txid), Some(taker_address)) => {
                let maker_wif =
                    rpc.wallet_wif.read().await.clone().ok_or_else(|| {
                        TauriError::InvalidInput("Maker wallet not unlocked".into())
                    })?;
                let refund_tx = vtorrent_wallet_service::build_vtr_htlc_refund(
                    vtorrent_wallet_service::VtrRefundParams {
                        hash_lock,
                        taker_address: &taker_address,
                        maker_address: &order.maker_address,
                        expiry: order.expiry,
                        vtr_amount: order.vtr_amount,
                        funding_txid,
                        maker_wif: &maker_wif,
                    },
                )
                .map_err(TauriError::InvalidInput)?;
                let refund_txid = refund_tx.txid();

                {
                    let mut mempool = rpc.mempool.lock().await;
                    mempool
                        .add_transaction_with_fee(
                            refund_tx.clone(),
                            vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS,
                        )
                        .map_err(|e| {
                            TauriError::InvalidInput(format!(
                                "Mempool rejected VTR refund tx: {}",
                                e
                            ))
                        })?;
                }
                if let Some(sender) = &rpc.tx_submit {
                    let _ = sender.try_send(refund_tx);
                }
                Some(refund_txid)
            }
            _ => None,
        }
    };

    let btc_refund_txid = {
        let swaps = rpc.swaps.read().await;
        let swap = swaps.get(&order_id);
        match swap {
            Some(s) if s.btc_funding_txid.is_some() && s.btc_expiry > 0 => {
                let funding_txid = s.btc_funding_txid.unwrap();
                let refund_address = s.taker_btc_refund_address.clone().ok_or_else(|| {
                    TauriError::InvalidInput("Taker BTC refund address not recorded".into())
                })?;
                let htlc = vtorrent_btc::htlc::BtcHtlc {
                    hash_lock: s.hash_lock,
                    recipient: s.maker_btc_address.clone().unwrap_or_default(),
                    refund_address,
                    expiry: s.btc_expiry,
                    amount: s.btc_amount,
                    network: *rpc.btc_network.read().await,
                };
                const REFUND_FEE_SATOSHIS: u64 = vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS;
                let unsigned = htlc
                    .build_refund_tx(funding_txid, REFUND_FEE_SATOSHIS)
                    .map_err(|e| {
                        TauriError::InvalidInput(format!("Unable to build BTC refund tx: {}", e))
                    })?;
                let refund_wif = {
                    let btc = rpc.btc_wallet.read().await;
                    let w = btc.as_ref().ok_or_else(|| {
                        TauriError::InvalidInput("BTC wallet not initialized".into())
                    })?;
                    w.derive_wif(0)
                        .map_err(|e| TauriError::Internal(e.to_string()))?
                };
                let signed = htlc.sign_refund_tx(unsigned, &refund_wif).map_err(|e| {
                    TauriError::InvalidInput(format!("Unable to sign BTC refund tx: {}", e))
                })?;
                let raw = bitcoin::consensus::encode::serialize(&signed);
                let txid = {
                    use bitcoin::hashes::Hash;
                    signed.compute_txid().to_byte_array()
                };
                vtorrent_btc::sync::broadcast_tx(&raw)
                    .await
                    .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;
                Some(txid)
            }
            _ => None,
        }
    };

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, order.hash_lock.unwrap_or([0u8; 32])));
    swap.status = SwapStatus::Refunded;

    Ok(SwapActionResult {
        order_id: order_id.clone(),
        txid: match (vtr_refund_txid, btc_refund_txid) {
            (Some(vtr), _) => hex::encode(vtr),
            (None, Some(btc)) => btc_txid_hex(&btc),
            (None, None) => hex::encode(order.order_id),
        },
        status: "Refunded".to_string(),
    })
}
