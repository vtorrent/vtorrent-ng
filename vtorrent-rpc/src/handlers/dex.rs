use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

pub async fn get_dex_orders(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<DexOrderResponse>>> {
    let order_book = state.order_book.read().await;
    let orders: Vec<DexOrderResponse> = order_book
        .list_open_orders()
        .iter()
        .map(|o| DexOrderResponse {
            id: hex::encode(o.order_id),
            maker_address: o.maker_address.clone(),
            offer_amount_satoshis: o.vtr_amount,
            offer_asset: "VTR".to_string(),
            request_amount_satoshis: o.target_amount,
            request_asset: o.target_asset.clone(),
            rate: o.rate(),
            status: format!("{:?}", o.status),
            funding_txid: o.funding_txid.map(hex::encode),
            created_at: o.created_at,
            expires_at: o.expiry as u64,
        })
        .collect();

    Ok(Json(orders))
}

pub async fn place_dex_order(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PlaceOrderRequest>,
) -> RpcResult<Json<PlaceOrderResponse>> {
    use vtorrent_node::atomic_swap::{
        AtomicSwap, SwapOrder, DEFAULT_HTLC_LOCKTIME, MAX_HTLC_LOCKTIME, MIN_HTLC_LOCKTIME,
    };

    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.offer_amount_satoshis == 0 || req.request_amount_satoshis == 0 {
        return Err(RpcError::BadRequest(format!(
            "DEX order amounts must be greater than zero (offer: {} sats, request: {} sats)",
            req.offer_amount_satoshis, req.request_amount_satoshis
        )));
    }
    if req.request_asset.trim().is_empty() {
        return Err(RpcError::BadRequest(
            "Requested asset is required — specify the target asset (e.g. \"BTC\")".into(),
        ));
    }
    if vtorrent_core::address::validate_p2pkh(&req.maker_address).is_err() {
        return Err(RpcError::BadRequest(format!(
            "Invalid maker address: {}",
            req.maker_address
        )));
    }

    let locktime = if req.expiry_secs == 0 {
        DEFAULT_HTLC_LOCKTIME
    } else if req.expiry_secs <= u32::MAX as u64 {
        req.expiry_secs as u32
    } else {
        return Err(RpcError::BadRequest(format!(
            "DEX order expiry {} seconds is too large — must fit in u32",
            req.expiry_secs
        )));
    };
    if !(MIN_HTLC_LOCKTIME..=MAX_HTLC_LOCKTIME).contains(&locktime) {
        return Err(RpcError::BadRequest(format!(
            "DEX order expiry {} seconds is outside valid range [{}, {}] seconds",
            locktime, MIN_HTLC_LOCKTIME, MAX_HTLC_LOCKTIME
        )));
    }

    let swap = AtomicSwap::new();
    let hash_lock = hex::encode(swap.hash_lock);
    let mut order = SwapOrder::new(
        req.maker_address,
        req.offer_amount_satoshis,
        req.request_asset,
        req.request_amount_satoshis,
        locktime,
    );
    order.hash_lock = Some(swap.hash_lock);
    order.preimage = Some(swap.preimage);
    if let Some(btc_addr) = req.maker_btc_address.clone() {
        order.maker_btc_address = Some(btc_addr);
    }
    let order_id = hex::encode(order.order_id);
    state.order_book.write().await.add_order(order);

    Ok(Json(PlaceOrderResponse {
        order_id,
        htlc_address: None,
        hash_lock,
        funding_txid: None,
        status: "Open".to_string(),
    }))
}

pub async fn cancel_dex_order(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<Value>> {
    let maker = state.wallet_change_address.read().await.clone();
    let order = {
        let order_book = state.order_book.read().await;
        order_book.get_order(&id).cloned()
    };
    let order = order.ok_or_else(|| {
        RpcError::NotFound(format!(
            "Order {} not found — check the order_id and ensure it exists on this node",
            id
        ))
    })?;
    if let Some(maker) = maker {
        if order.maker_address != maker {
            return Err(RpcError::Unauthorized(format!(
                "Only the maker ({}) may cancel order {} — your wallet address ({}) does not match",
                &order.maker_address[..order.maker_address.len().min(64)],
                id,
                &maker[..maker.len().min(64)]
            )));
        }
    }
    let cancelled = state.order_book.write().await.cancel_order(&id);
    if !cancelled {
        return Err(RpcError::NotFound(format!(
            "Order {} could not be cancelled — it may have already been cancelled or filled",
            id
        )));
    }
    Ok(Json(
        json!({ "success": true, "message": format!("Order {} cancelled", id) }),
    ))
}

pub async fn check_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ClaimCheckRequest>,
) -> RpcResult<Json<ClaimCheckResponse>> {
    use vtorrent_node::genesis::get_legacy_balance;

    let chain = state.chain.lock().await;
    let claimable = get_legacy_balance(&req.legacy_address);
    let already_claimed = chain.is_claimed(&req.legacy_address);

    Ok(Json(ClaimCheckResponse {
        address: req.legacy_address.clone(),
        claimable_satoshis: claimable,
        display: format!("{:.6} VTR", claimable as f64 / 100_000_000.0),
        already_claimed,
    }))
}

/// POST /api/v1/claim/submit
///
/// Verifies ownership of a legacy vTorrent address via WIF signature and
/// creates a claim transaction that mints the equivalent VTR on the new chain.
///
/// This uses `vtorrent-snapshot` to verify the legacy balance and
/// `vtorrent-wallet::TxBuilder` to build the claim transaction.
pub async fn submit_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ClaimSubmitRequest>,
) -> RpcResult<Json<ClaimSubmitResponse>> {
    use secp256k1::{Secp256k1, SecretKey};
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_node::block::{Transaction, TxOutput, TxType};
    use vtorrent_node::genesis::get_legacy_balance;
    use vtorrent_wallet::tx_builder::{p2pkh_script_pubkey, pubkey_to_vtorrent_address};

    if req.wif_private_key.is_empty() {
        return Err(RpcError::BadRequest(
            "WIF private key is required — provide the legacy address's WIF key to prove ownership"
                .into(),
        ));
    }
    if req.recipient_address.is_empty() {
        return Err(RpcError::BadRequest(
            "Recipient address is required — provide a valid VTR address to receive the claim"
                .into(),
        ));
    }

    let key = PrivateKey::from_wif(&req.wif_private_key).map_err(|e| {
        RpcError::BadRequest(format!(
            "Invalid WIF key: {} — expected base58 with valid checksum",
            e
        ))
    })?;
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(key.as_bytes()).map_err(|e| {
        RpcError::BadRequest(format!(
            "Invalid key bytes: {} — WIF decoded but secret key is malformed",
            e
        ))
    })?;
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let derived_address = pubkey_to_vtorrent_address(&pubkey.serialize()).map_err(|e| {
        RpcError::Internal(format!(
            "Failed to derive VTR address from public key: {}",
            e
        ))
    })?;

    let claimable = get_legacy_balance(&derived_address);
    if claimable == 0 {
        return Err(RpcError::BadRequest(format!(
            "No claimable balance for address {}",
            derived_address
        )));
    }

    {
        let chain = state.chain.lock().await;
        if chain.is_claimed(&derived_address) {
            return Err(RpcError::BadRequest(format!(
                "Address {} has already been claimed",
                derived_address
            )));
        }
    }

    let script_pubkey = p2pkh_script_pubkey(&req.recipient_address).map_err(|e| {
        RpcError::BadRequest(format!(
            "Invalid recipient address {}: {}",
            &req.recipient_address[..req.recipient_address.len().min(64)],
            e
        ))
    })?;

    let msg_hash = vtorrent_node::consensus::claim_message_hash(&derived_address);
    let msg = secp256k1::Message::from_digest(msg_hash);
    let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
    let (rec_id, sig64) = rec_sig.serialize_compact();
    let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + 4];
    sig_bytes.extend_from_slice(&sig64);

    let tx = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: claimable,
            script_pubkey,
        }],
        lock_time: 0,
        claim_address: Some(derived_address.clone()),
        claim_signature: Some(sig_bytes),
    };

    let txid = hex::encode(tx.txid());

    {
        let chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        mempool.admit_with_chain_fee(&chain, tx).map_err(|e| {
            RpcError::BadRequest(format!(
                "Mempool rejected claim {} for {} ({} sats): {}",
                txid,
                &derived_address[..derived_address.len().min(64)],
                claimable,
                e
            ))
        })?;
    }

    tracing::info!(
        "Claim transaction {} submitted for {} ({} sats)",
        txid,
        derived_address,
        claimable
    );

    Ok(Json(ClaimSubmitResponse {
        txid,
        claimed_satoshis: claimable,
        recipient_address: req.recipient_address,
    }))
}
