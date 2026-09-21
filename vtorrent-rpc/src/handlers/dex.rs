use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

use super::truncate_chars;

pub async fn get_dex_orders(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<DexOrderResponse>>> {
    let order_book = state.order_book.read().await;
    let swaps = state.swaps.read().await;
    let orders: Vec<DexOrderResponse> = order_book
        .list_orders()
        .iter()
        .filter(|o| {
            o.status == vtorrent_node::atomic_swap::OrderStatus::Open
                || swaps.contains_key(&hex::encode(o.order_id))
        })
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
    place_dex_order_with_state(&state, req).await.map(Json)
}

pub async fn place_dex_order_with_state(
    state: &AppState,
    req: PlaceOrderRequest,
) -> RpcResult<PlaceOrderResponse> {
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
    // A malformed BTC address is only rejected during btc_fund — after the
    // maker's VTR is already locked in the HTLC. Reject it up front.
    if let Some(btc_addr) = &req.maker_btc_address {
        if btc_addr
            .parse::<bitcoin::Address<bitcoin::address::NetworkUnchecked>>()
            .is_err()
        {
            return Err(RpcError::BadRequest(format!(
                "Invalid maker BTC address: {}",
                btc_addr
            )));
        }
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
    crate::swap_recovery::persist(state, &order, None).await?;
    state.order_book.write().await.add_order(order);

    Ok(PlaceOrderResponse {
        order_id,
        htlc_address: None,
        hash_lock,
        funding_txid: None,
        status: "Open".to_string(),
    })
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
    // Ownership must be verifiable: with the wallet locked (or not imported)
    // the maker address is unknown, so cancellation is refused rather than
    // silently allowed for any caller.
    let maker = maker.ok_or_else(|| {
        RpcError::Unauthorized(
            "Wallet locked — unlock the maker's wallet to cancel this order".into(),
        )
    })?;
    if order.maker_address != maker {
        return Err(RpcError::Unauthorized(format!(
            "Only the maker ({}) may cancel order {} — your wallet address ({}) does not match",
            truncate_chars(&order.maker_address, 64),
            id,
            truncate_chars(&maker, 64)
        )));
    }
    let mut book = state.order_book.write().await;
    let mut cancelled_order = book
        .get_order(&id)
        .cloned()
        .ok_or_else(|| RpcError::NotFound("Order disappeared".into()))?;
    if cancelled_order.status == vtorrent_node::atomic_swap::OrderStatus::Open {
        cancelled_order.status = vtorrent_node::atomic_swap::OrderStatus::Cancelled;
        crate::swap_recovery::persist(&state, &cancelled_order, None).await?;
    }
    let cancelled = book.cancel_order(&id);
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

/// Build and sign a legacy claim transaction.
///
/// Shared by `POST /api/v1/claim/submit` and
/// `POST /api/v1/blockchain/bootstrap` so the v2 signature construction exists
/// in exactly one place. Returns the transaction and the claimed amount.
pub fn build_legacy_claim_tx(
    wif_private_key: &str,
    recipient_address: &str,
) -> RpcResult<(vtorrent_node::block::Transaction, u64)> {
    use secp256k1::{Secp256k1, SecretKey};
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_node::block::{Transaction, TxOutput, TxType};
    use vtorrent_node::genesis::get_legacy_balance;
    use vtorrent_wallet::tx_builder::{p2pkh_script_pubkey, pubkey_to_vtorrent_address};

    if wif_private_key.is_empty() {
        return Err(RpcError::BadRequest(
            "WIF private key is required — provide the legacy address's WIF key to prove ownership"
                .into(),
        ));
    }
    if recipient_address.is_empty() {
        return Err(RpcError::BadRequest(
            "Recipient address is required — provide a valid VTR address to receive the claim"
                .into(),
        ));
    }

    let key = PrivateKey::from_wif(wif_private_key).map_err(|e| {
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
    let pubkey_bytes = vtorrent_core::keys::serialize_pubkey(&pubkey, key.is_compressed());
    let derived_address = pubkey_to_vtorrent_address(&pubkey_bytes).map_err(|e| {
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

    let script_pubkey = p2pkh_script_pubkey(recipient_address).map_err(|e| {
        RpcError::BadRequest(format!(
            "Invalid recipient address {}: {}",
            truncate_chars(recipient_address, 64),
            e
        ))
    })?;

    // Build the outputs first: the v2 signature commits to them, so a valid
    // signature cannot be replayed with a redirected recipient.
    let outputs = vec![TxOutput {
        value: claimable,
        script_pubkey,
    }];

    let msg_hash = vtorrent_node::consensus::claim_message_hash_v2(&derived_address, &outputs);
    let msg = secp256k1::Message::from_digest(msg_hash);
    let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
    let (rec_id, sig64) = rec_sig.serialize_compact();
    let compression_flag = if key.is_compressed() { 4 } else { 0 };
    let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + compression_flag];
    sig_bytes.extend_from_slice(&sig64);

    let tx = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs,
        lock_time: 0,
        claim_address: Some(derived_address),
        claim_signature: Some(sig_bytes),
    };

    Ok((tx, claimable))
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
    let (tx, claimable) = build_legacy_claim_tx(&req.wif_private_key, &req.recipient_address)?;
    let derived_address = tx
        .claim_address
        .clone()
        .ok_or_else(|| RpcError::Internal("Claim transaction missing address".into()))?;

    {
        let chain = state.chain.lock().await;
        if chain.is_claimed(&derived_address) {
            return Err(RpcError::BadRequest(format!(
                "Address {} has already been claimed",
                derived_address
            )));
        }
    }

    let txid = hex::encode(tx.txid());

    {
        let chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        mempool.admit_with_chain_fee(&chain, tx).map_err(|e| {
            RpcError::BadRequest(format!(
                "Mempool rejected claim {} for {} ({} sats): {}",
                txid,
                truncate_chars(&derived_address, 64),
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

/// POST /api/v1/blockchain/bootstrap
///
/// Mines the height-1 bootstrap claim block. Genesis has no stakeable UTXO, so
/// no coinstake can be produced and staking can never start (T3). This endpoint
/// is the only way a fresh chain begins: it builds a signed legacy claim and
/// mines it directly into a height-1 PoS block, seeding `total_staked`.
///
/// One-shot: valid only while the chain is at genesis. Permissionless: the
/// first valid claim wins (a real legacy holder bootstraps the chain).
pub async fn bootstrap_chain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BootstrapRequest>,
) -> RpcResult<Json<BootstrapResponse>> {
    let (mut claim, claimed) = build_legacy_claim_tx(&req.wif_private_key, &req.recipient_address)?;
    // The height-1 bootstrap block encodes its height in the first tx's
    // lock_time. The v2 claim signature commits to the outputs, not lock_time,
    // so setting it after signing does not invalidate the claim.
    claim.lock_time = 1;
    let txid = hex::encode(claim.txid());

    let (block_hash, height) = {
        let mut chain = state.chain.lock().await;
        if chain.best_height() != 0 {
            return Err(RpcError::BadRequest(
                "Bootstrap is only valid while the chain is at genesis".into(),
            ));
        }
        let hash = chain
            .apply_bootstrap_claim(claim)
            .map_err(|e| RpcError::BadRequest(format!("Bootstrap claim rejected: {}", e)))?;
        (hex::encode(hash), chain.best_height() as u64)
    };

    // Announce to peers, mirroring the regtest faucet path: the block was mined
    // directly into the chain, bypassing the node's normal block-production
    // path, so the node event loop must emit the NewBlock event and broadcast.
    if let Some(sender) = &state.block_submit {
        let chain = state.chain.lock().await;
        if let Some(block) = chain.get_block_at_height(height as u32).cloned() {
            let _ = sender.try_send(block);
        }
    }

    tracing::info!(
        "Bootstrap claim {} mined at height {} ({})",
        txid,
        height,
        block_hash
    );

    Ok(Json(BootstrapResponse {
        txid,
        block_hash,
        block_height: height,
        claimed_satoshis: claimed,
        recipient_address: req.recipient_address,
    }))
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;

    /// A WIF whose derived address is registered as a synthetic legacy holder.
    fn legacy_wif_with_balance(balance: u64) -> (String, String) {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        use vtorrent_core::keys::PrivateKey;
        use vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address;

        let key = PrivateKey::from_bytes([91u8; 32], true).unwrap();
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let pubkey_bytes = vtorrent_core::keys::serialize_pubkey(&pubkey, true);
        let address = pubkey_to_vtorrent_address(&pubkey_bytes).unwrap();
        vtorrent_node::genesis::set_test_legacy_balance(&address, balance);
        (key.to_wif(198), address)
    }

    #[test]
    fn build_legacy_claim_tx_matches_expected_shape() {
        let (wif, address) = legacy_wif_with_balance(500 * 100_000_000);
        let (_, recipient) = {
            use secp256k1::{PublicKey, Secp256k1, SecretKey};
            use vtorrent_core::keys::PrivateKey;
            let key = PrivateKey::from_bytes([92u8; 32], true).unwrap();
            let secp = Secp256k1::new();
            let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
            let pubkey = PublicKey::from_secret_key(&secp, &secret);
            (
                key.to_wif(198),
                vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string(),
            )
        };

        let (tx, claimed) = build_legacy_claim_tx(&wif, &recipient).expect("helper builds");
        assert_eq!(tx.tx_type, vtorrent_node::block::TxType::LegacyClaim);
        assert_eq!(tx.claim_address.as_deref(), Some(address.as_str()));
        assert_eq!(claimed, 500 * 100_000_000);
        assert_eq!(tx.total_output(), claimed);
        assert!(tx.claim_signature.is_some());
        assert!(tx.inputs.is_empty());
    }

    #[test]
    fn build_legacy_claim_tx_rejects_unknown_address() {
        // A WIF whose address has no snapshot balance must be rejected.
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        use vtorrent_core::keys::PrivateKey;
        let key = PrivateKey::from_bytes([93u8; 32], true).unwrap();
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let recipient = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string();
        let err = build_legacy_claim_tx(&key.to_wif(198), &recipient).unwrap_err();
        assert!(err.to_string().contains("No claimable balance"));
    }

    fn recipient_address(seed: u8) -> String {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};
        use vtorrent_core::keys::PrivateKey;
        let key = PrivateKey::from_bytes([seed; 32], true).unwrap();
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string()
    }

    #[tokio::test]
    async fn bootstrap_endpoint_mines_height1_and_is_one_shot() {
        let (wif, _) = legacy_wif_with_balance(500 * 100_000_000);
        let recipient = recipient_address(94);
        let state = std::sync::Arc::new(AppState::new());

        let res = bootstrap_chain(
            State(state.clone()),
            Json(BootstrapRequest {
                wif_private_key: wif.clone(),
                recipient_address: recipient.clone(),
            }),
        )
        .await
        .expect("first bootstrap must succeed")
        .0;
        assert_eq!(res.block_height, 1);
        assert_eq!(res.claimed_satoshis, 500 * 100_000_000);
        assert!(!res.block_hash.is_empty());

        // One-shot: a second call must be rejected because height > 0.
        let err = bootstrap_chain(
            State(state),
            Json(BootstrapRequest {
                wif_private_key: wif,
                recipient_address: recipient,
            }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("genesis"),
            "unexpected error: {}",
            err
        );
    }
}
