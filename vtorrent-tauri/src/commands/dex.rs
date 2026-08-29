use serde::Serialize;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

fn btc_txid_hex(bytes: &[u8; 32]) -> String {
    let mut display = *bytes;
    display.reverse();
    hex::encode(display)
}

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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
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

    const FUNDING_FEE_SATOSHIS: u64 = 10_000;
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
    let btc_amount = order.target_amount;
    if btc_amount == 0 {
        return Err(TauriError::InvalidInput(
            "Order target amount is zero".into(),
        ));
    }

    let htlc = vtorrent_btc::htlc::BtcHtlc::new(
        hash_lock,
        maker_btc_address.clone(),
        btc_refund_address.clone(),
        vtorrent_btc::htlc::DEFAULT_HTLC_LOCKTIME,
        btc_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to construct BTC HTLC: {}", e)))?;

    const FUNDING_FEE_SATOSHIS: u64 = 1_000;
    let (funding_utxo, funder_wif, change_address) = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        let utxos = w.list_utxos();
        let selected = utxos
            .iter()
            .filter(|u| u.value >= btc_amount + FUNDING_FEE_SATOSHIS)
            .max_by_key(|u| u.value)
            .cloned()
            .ok_or_else(|| TauriError::InvalidInput("Insufficient BTC funds".into()))?;
        let wif = w
            .derive_wif(0)
            .map_err(|e| TauriError::Internal(e.to_string()))?;
        let change = w
            .current_address()
            .map_err(|e| TauriError::Internal(e.to_string()))?;
        (selected, wif, change)
    };

    let funding_txid_bytes: [u8; 32] = {
        use bitcoin::hashes::Hash;
        funding_utxo
            .txid
            .parse::<bitcoin::Txid>()
            .map(|t| t.to_byte_array())
            .map_err(|e| TauriError::InvalidInput(format!("Invalid UTXO txid: {}", e)))?
    };
    let unsigned = htlc
        .build_funding_tx(
            funding_txid_bytes,
            funding_utxo.vout,
            funding_utxo.value,
            FUNDING_FEE_SATOSHIS,
            &change_address,
        )
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build BTC funding tx: {}", e)))?;
    let signed = htlc
        .sign_funding_tx(unsigned, funding_utxo.value, &funder_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign BTC funding tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let btc_funding_txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    vtorrent_btc::sync::broadcast_tx(&raw)
        .await
        .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;

    let mut swaps = rpc.swaps.write().await;
    let swap = swaps
        .entry(order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.btc_funding_txid = Some(btc_funding_txid);
    swap.maker_btc_address = Some(maker_btc_address);
    swap.taker_btc_refund_address = Some(btc_refund_address);
    swap.btc_amount = btc_amount;
    swap.btc_expiry = htlc.expiry;
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
    use vtorrent_node::atomic_swap::{Htlc, SwapState, SwapStatus};
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

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

    let htlc = Htlc::with_expiry(
        hash_lock,
        taker_address.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| TauriError::InvalidInput(format!("Unable to reconstruct HTLC: {}", e)))?;

    const CLAIM_FEE_SATOSHIS: u64 = 10_000;
    let unsigned = htlc
        .build_claim_tx_unsigned(funding_txid, &preimage_bytes, CLAIM_FEE_SATOSHIS)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build VTR claim tx: {}", e)))?;

    let htlc_script = htlc
        .build_script()
        .map_err(|e| TauriError::InvalidInput(format!("Invalid HTLC addresses: {}", e)))?;
    let (sig, pubkey) = sign_input_over_subscript(&unsigned, 0, &htlc_script, &taker_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign VTR claim tx: {}", e)))?;

    let mut script_sig = Vec::new();
    script_sig.push(sig.len() as u8);
    script_sig.extend_from_slice(&sig);
    script_sig.push(pubkey.len() as u8);
    script_sig.extend_from_slice(&pubkey);
    script_sig.push(0x20);
    script_sig.extend_from_slice(&preimage_bytes);
    script_sig.push(0x51); // OP_1

    let mut claim_tx = unsigned;
    claim_tx.inputs[0].script_sig = script_sig;
    let claim_txid = claim_tx.txid();

    {
        let mut mempool = rpc.mempool.lock().await;
        mempool
            .add_transaction_with_fee(claim_tx.clone(), CLAIM_FEE_SATOSHIS)
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
        network: *rpc.btc_network.read().await,
    };
    const CLAIM_FEE_SATOSHIS: u64 = 1_000;
    let unsigned = htlc
        .build_claim_tx(btc_funding_txid, &preimage, CLAIM_FEE_SATOSHIS)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to build BTC claim tx: {}", e)))?;
    let maker_wif = {
        let btc = rpc.btc_wallet.read().await;
        let w = btc
            .as_ref()
            .ok_or_else(|| TauriError::InvalidInput("BTC wallet not initialized".into()))?;
        w.derive_wif(0)
            .map_err(|e| TauriError::Internal(e.to_string()))?
    };
    let signed = htlc
        .sign_claim_tx(unsigned, &preimage, &maker_wif)
        .map_err(|e| TauriError::InvalidInput(format!("Unable to sign BTC claim tx: {}", e)))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    vtorrent_btc::sync::broadcast_tx(&raw)
        .await
        .map_err(|e| TauriError::Internal(format!("BTC broadcast failed: {}", e)))?;

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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    if now < order.expiry {
        return Err(TauriError::InvalidInput("Swap has not expired yet".into()));
    }

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
                const REFUND_FEE_SATOSHIS: u64 = 1_000;
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
        txid: btc_refund_txid
            .map(|mut t| {
                t.reverse();
                t
            })
            .map(hex::encode)
            .unwrap_or_else(|| hex::encode(order.order_id)),
        status: "Refunded".to_string(),
    })
}
