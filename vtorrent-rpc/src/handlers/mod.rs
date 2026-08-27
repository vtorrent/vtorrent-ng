use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;
use std::sync::Arc;

pub mod prelude;
pub mod staking;
pub mod swap;
pub mod torrent;
pub mod wallet;

pub use staking::*;
pub use swap::*;
pub use torrent::*;
pub use wallet::*;

pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Current time in seconds, honoring the regtest mock clock if set.
pub(crate) async fn now_secs_mock(state: &AppState) -> u64 {
    if let Some(t) = *state.mock_time.read().await {
        t
    } else {
        now_secs()
    }
}

/// Broadcast a raw BTC transaction to the configured network/peer.
pub(crate) async fn broadcast_btc(state: &AppState, raw: &[u8]) -> RpcResult<[u8; 32]> {
    let network = *state.btc_network.read().await;
    let peer = state.btc_peer.read().await.clone();
    if let Some(host) = peer {
        // Resolve per broadcast: container peers may change IPs across restarts.
        let addr = tokio::net::lookup_host(&host)
            .await
            .ok()
            .and_then(|mut it| it.next())
            .ok_or_else(|| RpcError::Internal(format!("BTC peer {} unresolved", host)))?;
        vtorrent_btc::sync::broadcast_tx_to(raw, network, &[addr])
            .await
            .map_err(|e| RpcError::Internal(format!("BTC broadcast failed: {}", e)))
    } else {
        vtorrent_btc::sync::broadcast_tx(raw)
            .await
            .map_err(|e| RpcError::Internal(format!("BTC broadcast failed: {}", e)))
    }
}

pub(crate) fn parse_hash32(value: &str, field: &str) -> RpcResult<[u8; 32]> {
    let bytes =
        hex::decode(value).map_err(|_| RpcError::BadRequest(format!("Invalid {} hex", field)))?;
    if bytes.len() != 32 {
        return Err(RpcError::BadRequest(format!("{} must be 32 bytes", field)));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Ok(hash)
}

/// Encode a Bitcoin txid stored in internal (little-endian) byte order as the
/// display-order hex string used by Bitcoin Core and block explorers.
pub fn btc_txid_hex(bytes: &[u8; 32]) -> String {
    let mut display = *bytes;
    display.reverse();
    hex::encode(display)
}

/// Reject swap operations whose current lifecycle stage makes them invalid
/// (e.g. double-funding, claiming after refund, refunding after claim).
pub fn require_swap_stage(
    swap: Option<&vtorrent_node::atomic_swap::SwapState>,
    forbidden: &[vtorrent_node::atomic_swap::SwapStatus],
) -> RpcResult<()> {
    if let Some(swap) = swap {
        if forbidden.contains(&swap.status) {
            return Err(RpcError::BadRequest(format!(
                "Swap is in state {:?}; operation not allowed",
                swap.status
            )));
        }
    }
    Ok(())
}

pub(crate) fn block_response(
    hash: [u8; 32],
    height: u32,
    block: &vtorrent_node::block::Block,
) -> BlockResponse {
    BlockResponse {
        hash: hex::encode(hash),
        height: height as u64,
        version: block.header.version,
        prev_hash: hex::encode(block.header.prev_block_hash),
        merkle_root: hex::encode(block.header.merkle_root),
        timestamp: block.header.timestamp,
        bits: block.header.bits,
        nonce: block.header.nonce,
        tx_count: block.transactions.len(),
        size_bytes: bincode::serialized_size(block).unwrap_or(0) as usize,
    }
}

pub(crate) fn transaction_lookup_response(
    txid: [u8; 32],
    tx: &vtorrent_node::block::Transaction,
    block_hash: Option<[u8; 32]>,
    block_height: Option<u32>,
) -> TransactionLookupResponse {
    TransactionLookupResponse {
        txid: hex::encode(txid),
        block_hash: block_hash.map(hex::encode),
        block_height,
        version: tx.version,
        tx_type: tx.type_str().to_string(),
        inputs: tx
            .inputs
            .iter()
            .map(|input| TransactionInputResponse {
                prev_txid: hex::encode(input.prev_txid),
                prev_vout: input.prev_vout,
                script_sig: hex::encode(&input.script_sig),
                sequence: input.sequence,
            })
            .collect(),
        outputs: tx
            .outputs
            .iter()
            .map(|output| TransactionOutputResponse {
                value_satoshis: output.value,
                script_pubkey: hex::encode(&output.script_pubkey),
            })
            .collect(),
        lock_time: tx.lock_time,
        claim_address: tx.claim_address.clone(),
    }
}

pub fn validate_p2pkh(addr: &str) -> RpcResult<()> {
    vtorrent_core::address::validate_p2pkh(addr)
        .map(|_| ())
        .map_err(|e| RpcError::BadRequest(format!("Invalid address: {}", e)))
}

/// Verify the hot wallet passphrase (and TOTP code if 2FA is enabled) and
/// return the decrypted WIF. Fails if no wallet has been imported or the
/// credentials are incorrect.
pub(crate) async fn verify_wallet_auth(
    state: &AppState,
    passphrase: &str,
    otp_code: Option<&str>,
) -> RpcResult<String> {
    let encrypted = state.wallet_encrypted.read().await.clone().ok_or_else(|| {
        RpcError::BadRequest("No wallet imported. Call POST /api/v1/wallet/import first.".into())
    })?;

    let plaintext = vtorrent_wallet::encryption::decrypt_wallet(&encrypted, passphrase)
        .map_err(|_| RpcError::Unauthorized("Incorrect passphrase".into()))?;
    let wif = String::from_utf8(plaintext)
        .map_err(|_| RpcError::Internal("Wallet decryption produced invalid data".into()))?;

    if let Some(secret) = state.wallet_totp_secret.read().await.as_ref() {
        let code = otp_code
            .filter(|c| !c.is_empty())
            .ok_or_else(|| RpcError::Unauthorized("TOTP code required".into()))?;
        secret
            .verify_or_error(code)
            .map_err(|_| RpcError::Unauthorized("Invalid TOTP code".into()))?;
    }

    Ok(wif)
}

pub(crate) fn utxo_select(
    utxos: &[vtorrent_btc::utxo::Utxo],
    amount: u64,
    fee: u64,
) -> Option<Vec<vtorrent_btc::utxo::Utxo>> {
    let required = amount.checked_add(fee)?;
    let mut sorted: Vec<vtorrent_btc::utxo::Utxo> = utxos.to_vec();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let mut selected = Vec::new();
    let mut sum = 0u64;
    for u in sorted {
        sum = sum.saturating_add(u.value);
        selected.push(u);
        if sum >= required {
            return Some(selected);
        }
    }
    None
}

pub async fn get_node_info(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<NodeInfoResponse>> {
    let chain = state.chain.lock().await;
    let peer_count = *state.peer_count.read().await;
    let syncing = *state.syncing.read().await;
    let uptime = now_secs().saturating_sub(state.start_time);

    let height = chain.best_height() as u64;
    let _best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));

    // Compute sync percentage from best known peer height.
    let sync_percent = if syncing {
        let peer_height = *state.best_peer_height.read().await;
        if peer_height > 0 {
            ((height as f64 / peer_height as f64) * 100.0).min(99.9)
        } else {
            0.0
        }
    } else {
        100.0
    };

    // Get mempool size without holding the chain lock.
    drop(chain);
    let mempool_size = {
        let mempool = state.mempool.lock().await;
        mempool.size()
    };
    // Re-acquire chain for best_hash (already computed above, just use height).
    let chain = state.chain.lock().await;
    let height = chain.best_height() as u64;
    let best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));

    Ok(Json(NodeInfoResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        network: state.network.clone(),
        block_height: height,
        best_block_hash: best_hash,
        connections: peer_count,
        syncing,
        sync_percent,
        mempool_size,
        uptime_secs: uptime,
    }))
}

// ─── Blockchain ───────────────────────────────────────────────────────────────

pub async fn get_block_height(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BlockHeightResponse>> {
    let chain = state.chain.lock().await;
    let height = chain.best_height() as u64;
    let best_hash = chain
        .best_hash()
        .map(hex::encode)
        .unwrap_or_else(|| "0".repeat(64));
    let timestamp = chain
        .get_block_at_height(chain.best_height())
        .map(|b| b.header.timestamp)
        .unwrap_or_else(|| now_secs() as u32);

    Ok(Json(BlockHeightResponse {
        height,
        hash: best_hash,
        timestamp,
    }))
}

pub async fn get_block_by_hash(
    State(state): State<Arc<AppState>>,
    Path(hash_hex): Path<String>,
) -> RpcResult<Json<BlockResponse>> {
    let hash = parse_hash32(&hash_hex, "block hash")?;
    let chain = state.chain.lock().await;
    let height = chain
        .block_height(&hash)
        .ok_or_else(|| RpcError::NotFound(format!("Active-chain block {} not found", hash_hex)))?;
    let block = chain
        .get_block(&hash)
        .ok_or_else(|| RpcError::Internal(format!("Indexed block {} is missing", hash_hex)))?;
    Ok(Json(block_response(hash, height, block)))
}

/// Get an active-chain block by height.
pub async fn get_block_by_height(
    State(state): State<Arc<AppState>>,
    Path(height): Path<u32>,
) -> RpcResult<Json<BlockResponse>> {
    let chain = state.chain.lock().await;
    let hash = chain
        .block_hash_at_height(height)
        .ok_or_else(|| RpcError::NotFound(format!("Block at height {} not found", height)))?;
    let block = chain.get_block_at_height(height).ok_or_else(|| {
        RpcError::Internal(format!("Indexed block at height {} is missing", height))
    })?;
    Ok(Json(block_response(hash, height, block)))
}

/// Get a transaction by txid, searching the active chain first and then mempool.
pub async fn get_transaction_by_id(
    State(state): State<Arc<AppState>>,
    Path(txid_hex): Path<String>,
) -> RpcResult<Json<TransactionLookupResponse>> {
    let txid = parse_hash32(&txid_hex, "transaction ID")?;

    {
        let chain = state.chain.lock().await;
        if let Some((tx, block_hash, height)) = chain.get_transaction(&txid) {
            return Ok(Json(transaction_lookup_response(
                txid,
                tx,
                Some(block_hash),
                Some(height),
            )));
        }
    }

    let mempool = state.mempool.lock().await;
    let tx = mempool
        .get_transaction(&txid)
        .ok_or_else(|| RpcError::NotFound(format!("Transaction {} not found", txid_hex)))?;
    Ok(Json(transaction_lookup_response(txid, tx, None, None)))
}

/// Submit a raw, signed transaction to the local mempool and live P2P node.
pub async fn broadcast_transaction(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BroadcastTransactionRequest>,
) -> RpcResult<Json<BroadcastTransactionResponse>> {
    const MAX_RAW_TX_BYTES: usize = 1_000_000;

    if req.raw_tx.is_empty() {
        return Err(RpcError::BadRequest("raw_tx is required".into()));
    }
    let raw = hex::decode(&req.raw_tx)
        .map_err(|_| RpcError::BadRequest("raw_tx must be hexadecimal".into()))?;
    if raw.len() > MAX_RAW_TX_BYTES {
        return Err(RpcError::BadRequest(format!(
            "raw_tx exceeds the {} byte limit",
            MAX_RAW_TX_BYTES
        )));
    }
    let tx: vtorrent_node::block::Transaction = bincode::deserialize(&raw)
        .map_err(|_| RpcError::BadRequest("raw_tx is not a valid vTorrent transaction".into()))?;
    let txid = tx.txid();

    {
        // Verify the fee from the live UTXO set — same rule the P2P relay
        // path applies — so self-reported fee estimates cannot buy priority.
        // Chain is locked BEFORE mempool to match the node loop's lock order
        // (chain → mempool) and avoid ABBA deadlock with block processing.
        let real_fee = {
            let chain = state.chain.lock().await;
            chain.compute_tx_fee(&tx)
        };
        let mut mempool = state.mempool.lock().await;
        match real_fee {
            Some(fee) => mempool
                .add_transaction_with_fee(tx.clone(), fee)
                .map_err(|e| {
                    RpcError::BadRequest(format!("Mempool rejected transaction: {}", e))
                })?,
            None => {
                return Err(RpcError::BadRequest(
                    "Transaction inputs not found in UTXO set".into(),
                ))
            }
        }
    }

    let relayed = match &state.tx_submit {
        Some(sender) => sender.try_send(tx).is_ok(),
        None => false,
    };
    tracing::info!(txid = %hex::encode(txid), relayed, "Raw transaction accepted for broadcast");

    Ok(Json(BroadcastTransactionResponse {
        txid: hex::encode(txid),
        accepted: true,
        relayed,
    }))
}

pub async fn get_mempool(State(state): State<Arc<AppState>>) -> RpcResult<Json<MempoolResponse>> {
    let mempool = state.mempool.lock().await;
    let txs = mempool.get_transactions();
    let txids: Vec<String> = txs.iter().map(|tx| hex::encode(tx.txid())).collect();
    let count = txids.len();
    // Sum the actual serialized size of each transaction, not an estimate.
    let size_bytes: usize = txs
        .iter()
        .map(|tx| serde_json::to_vec(tx).map(|v| v.len()).unwrap_or(0))
        .sum();

    Ok(Json(MempoolResponse {
        count,
        size_bytes,
        txids,
    }))
}

/// Return the current fee recommendations derived from the local mempool.
pub async fn get_fee_estimate(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<FeeEstimateResponse>> {
    let mempool = state.mempool.lock().await;
    Ok(Json(FeeEstimateResponse {
        recommended_sat_per_byte: mempool.recommended_fee_rate(),
        minimum_sat_per_byte: mempool.min_fee_rate(),
        median_sat_per_byte: mempool.median_fee_rate(),
        mempool_transactions: mempool.size(),
    }))
}

// ─── Wallet ───────────────────────────────────────────────────────────────────

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
        return Err(RpcError::BadRequest(
            "DEX order amounts must be greater than zero".into(),
        ));
    }
    if req.request_asset.trim().is_empty() {
        return Err(RpcError::BadRequest("Requested asset is required".into()));
    }
    // Validate the maker address so an invalid address cannot silently lock
    // funds to an unspendable output.
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
        return Err(RpcError::BadRequest("DEX order expiry is too large".into()));
    };
    if !(MIN_HTLC_LOCKTIME..=MAX_HTLC_LOCKTIME).contains(&locktime) {
        return Err(RpcError::BadRequest(format!(
            "DEX order expiry must be between {} and {} seconds",
            MIN_HTLC_LOCKTIME, MAX_HTLC_LOCKTIME
        )));
    }

    // Generate the swap secret now, but do not fund until a taker specifies the
    // recipient address. Funding at order placement would make the HTLC claimable
    // by an unknown party and is therefore unsafe.
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
    // Only the maker (the wallet that placed the order) may cancel it.
    let maker = state.wallet_change_address.read().await.clone();
    let order = {
        let order_book = state.order_book.read().await;
        order_book.get_order(&id).cloned()
    };
    let order = order.ok_or_else(|| RpcError::NotFound(format!("Order {} not found", id)))?;
    if let Some(maker) = maker {
        if order.maker_address != maker {
            return Err(RpcError::Unauthorized(
                "Only the maker may cancel this order".into(),
            ));
        }
    }
    let cancelled = state.order_book.write().await.cancel_order(&id);
    if !cancelled {
        return Err(RpcError::NotFound(format!("Order {} not found", id)));
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
        return Err(RpcError::BadRequest("WIF private key is required".into()));
    }
    if req.recipient_address.is_empty() {
        return Err(RpcError::BadRequest("Recipient address is required".into()));
    }

    // 1. Derive the legacy address from the WIF key.
    let key = PrivateKey::from_wif(&req.wif_private_key)
        .map_err(|e| RpcError::BadRequest(format!("Invalid WIF key: {}", e)))?;
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(key.as_bytes())
        .map_err(|e| RpcError::BadRequest(format!("Invalid key bytes: {}", e)))?;
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let derived_address = pubkey_to_vtorrent_address(&pubkey.serialize())
        .map_err(|e| RpcError::Internal(e.to_string()))?;

    // 2. Look up the claimable balance for this address.
    let claimable = get_legacy_balance(&derived_address);
    if claimable == 0 {
        return Err(RpcError::BadRequest(format!(
            "No claimable balance for address {}",
            derived_address
        )));
    }

    // 3. Check if already claimed.
    {
        let chain = state.chain.lock().await;
        if chain.is_claimed(&derived_address) {
            return Err(RpcError::BadRequest(format!(
                "Address {} has already been claimed",
                derived_address
            )));
        }
    }

    // 4. Build a claim transaction (coinbase-style with claim_address set).
    //    The transaction has no inputs (it is a genesis claim) and one output
    //    to the recipient address.
    let script_pubkey = p2pkh_script_pubkey(&req.recipient_address)
        .map_err(|e| RpcError::BadRequest(format!("Invalid recipient address: {}", e)))?;

    // Sign the claim: a compact (recoverable) ECDSA signature over the claim
    // address. Signing the address (rather than the txid) avoids a circular
    // dependency, and the recoverable format lets validation derive the address
    // from the signature alone.
    let msg_hash = vtorrent_node::consensus::claim_message_hash(&derived_address);
    let msg = secp256k1::Message::from_digest(msg_hash);
    let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
    let (rec_id, sig64) = rec_sig.serialize_compact();
    let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + 4]; // compressed flag
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

    // 5. Submit to mempool. Fee verified from live UTXO set (lock order:
    // chain → mempool) so claim fees cannot buy priority.
    {
        let real_fee = {
            let chain = state.chain.lock().await;
            chain.compute_tx_fee(&tx)
        };
        let mut mempool = state.mempool.lock().await;
        match real_fee {
            Some(fee) => mempool
                .add_transaction_with_fee(tx, fee)
                .map_err(|e| RpcError::BadRequest(format!("Mempool rejected claim: {}", e)))?,
            None => {
                return Err(RpcError::BadRequest(
                    "Claim inputs not found in UTXO set".into(),
                ))
            }
        }
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

// --- SPV -------------------------------------------------------------------

/// GET /api/v1/spv/status - returns the current SPV header chain status.
pub async fn get_spv_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<SpvStatusResponse>> {
    let chain = state.spv_chain.read().await;
    let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();
    Ok(Json(SpvStatusResponse {
        header_count: chain.len(),
        best_height: chain.best_height(),
        best_hash,
    }))
}

/// POST /api/v1/spv/headers - submit a batch of block headers to the SPV chain.
pub async fn add_spv_headers(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpvAddHeadersRequest>,
) -> RpcResult<Json<SpvAddHeadersResponse>> {
    use vtorrent_spv::SpvHeader;

    let mut headers: Vec<SpvHeader> = Vec::with_capacity(req.headers.len());
    for h in req.headers {
        let prev_hash_bytes = hex::decode(&h.prev_hash)
            .map_err(|_| RpcError::BadRequest(format!("invalid prev_hash hex: {}", h.prev_hash)))?;
        let merkle_root_bytes = hex::decode(&h.merkle_root).map_err(|_| {
            RpcError::BadRequest(format!("invalid merkle_root hex: {}", h.merkle_root))
        })?;

        if prev_hash_bytes.len() != 32 || merkle_root_bytes.len() != 32 {
            return Err(RpcError::BadRequest(
                "prev_hash and merkle_root must be 32 bytes (64 hex chars)".into(),
            ));
        }

        let mut ph = [0u8; 32];
        let mut mr = [0u8; 32];
        ph.copy_from_slice(&prev_hash_bytes);
        mr.copy_from_slice(&merkle_root_bytes);

        headers.push(SpvHeader {
            version: h.version,
            prev_hash: ph,
            merkle_root: mr,
            timestamp: h.timestamp,
            bits: h.bits,
            nonce: h.nonce,
            height: h.height,
        });
    }

    let added = {
        let mut chain = state.spv_chain.write().await;
        chain
            .add_headers(headers)
            .map_err(|e| RpcError::BadRequest(format!("SPV header validation failed: {}", e)))?
    };

    let chain = state.spv_chain.read().await;
    let best_hash = chain.best_hash().map(hex::encode).unwrap_or_default();

    tracing::info!(
        "SPV: added {} headers, best height now {}",
        added,
        chain.best_height()
    );

    Ok(Json(SpvAddHeadersResponse {
        added,
        best_height: chain.best_height(),
        best_hash,
    }))
}

// ─── Peers ────────────────────────────────────────────────────────────────────

/// GET /api/v1/peers
///
/// Returns the list of currently connected P2P peers with their metadata.
/// The list is updated live by the daemon event bridge on `PeerConnected` /
/// `PeerDisconnected` events.
pub async fn get_peers(State(state): State<Arc<AppState>>) -> RpcResult<Json<PeersResponse>> {
    let peer_list = state.peer_list.read().await;
    let peers: Vec<PeerInfoResponse> = peer_list
        .iter()
        .map(|p| PeerInfoResponse {
            addr: p.addr.clone(),
            user_agent: p.user_agent.clone(),
            services: p.services,
            best_height: p.best_height,
        })
        .collect();
    let count = peers.len();
    Ok(Json(PeersResponse { count, peers }))
}

// ─── Bitcoin wallet ────────────────────────────────────────────────────────────

pub async fn get_btc_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BtcStatusResponse>> {
    let btc = state.btc_wallet.read().await;
    match &*btc {
        None => Ok(Json(BtcStatusResponse {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
            synced: false,
        })),
        Some(w) => Ok(Json(BtcStatusResponse {
            initialized: true,
            balance_satoshis: w.balance(),
            address: w.current_address().ok(),
            best_height: w.best_height(),
            synced: w.synced(),
        })),
    }
}

/// GET /api/v1/btc/address
pub async fn get_btc_address(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    // Read-only: return the current receiving address without advancing the
    // wallet's address index (a GET must not mutate state).
    let btc = state.btc_wallet.read().await;
    match &*btc {
        None => Err(RpcError::BadRequest("BTC wallet not initialized".into())),
        Some(w) => {
            let address = w
                .current_address()
                .map_err(|e| RpcError::Internal(e.to_string()))?;
            Ok(Json(json!({ "address": address })))
        }
    }
}

/// POST /api/v1/btc/send
pub async fn send_btc(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcSendRequest>,
) -> RpcResult<Json<BtcSendResponse>> {
    if req.amount_satoshis == 0 {
        return Err(RpcError::BadRequest("Amount must be non-zero".into()));
    }
    if req.to_address.trim().is_empty() {
        return Err(RpcError::BadRequest("Recipient address is required".into()));
    }
    let fee = req.fee_satoshis.unwrap_or(1_000);

    // Build and sign, removing spent UTXOs from the wallet. The selected
    // UTXOs are returned so the spend can be rolled back if broadcasting
    // fails (otherwise the wallet forgets outputs for a tx that never made
    // it onto the network).
    let (txid_hex, raw, spent_utxos) = {
        let mut btc = state.btc_wallet.write().await;
        let w = btc
            .as_mut()
            .ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
        w.send_to(&req.to_address, req.amount_satoshis, fee)
            .map_err(|e| RpcError::BadRequest(e.to_string()))?
    };

    // Broadcast to the Bitcoin network.
    if let Err(e) = broadcast_btc(&state, &raw).await {
        if let Some(w) = state.btc_wallet.write().await.as_mut() {
            w.restore_utxos(&spent_utxos);
        }
        return Err(e);
    }

    Ok(Json(BtcSendResponse {
        txid: txid_hex,
        raw_tx: String::new(),
    }))
}

// ─── Swap Orchestration ───────────────────────────────────────────────────────

/// POST /api/v1/swap/btc-fund
///
/// The taker funds the BTC HTLC using the maker's hash lock.
pub async fn faucet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<FaucetRequest>,
) -> RpcResult<Json<FaucetResponse>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Faucet is only available in regtest mode".into(),
        ));
    }
    if req.address.trim().is_empty() {
        return Err(RpcError::BadRequest("Address is required".into()));
    }
    let amount = req
        .amount_satoshis
        .unwrap_or(100 * vtorrent_node::consensus::COIN);
    if amount == 0 {
        return Err(RpcError::BadRequest("Amount must be non-zero".into()));
    }

    let (txid, height, block) = {
        let mut chain = state.chain.lock().await;
        let txid = chain
            .mint_to_address(&req.address, amount)
            .map_err(|e| RpcError::Internal(e.to_string()))?;
        let height = chain.best_height();
        let block = chain
            .get_block_at_height(height)
            .cloned()
            .ok_or_else(|| RpcError::Internal("minted block not found".into()))?;
        (txid, height, block)
    };

    // Announce the minted block to peers so a multi-node regtest network stays
    // in sync (the faucet mints directly into the chain, bypassing the node's
    // normal block-production path).
    if let Some(sender) = &state.block_submit {
        let _ = sender.try_send(block);
    }

    Ok(Json(FaucetResponse {
        address: req.address,
        amount_satoshis: amount,
        txid: hex::encode(txid),
        block_height: height as u64,
    }))
}

/// GET /api/v1/debug/order/:id/preimage
///
/// Returns the secret preimage for an order (regtest only). In a real swap the
/// taker learns the preimage when the maker claims BTC on-chain; this endpoint
/// exists purely to exercise the VTR claim leg in local regtest testing.
pub async fn debug_order_preimage(
    State(state): State<Arc<AppState>>,
    Path(order_id): Path<String>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Preimage debug endpoint is only available in regtest mode".into(),
        ));
    }
    let order_book = state.order_book.read().await;
    let order = order_book
        .get_order(&order_id)
        .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", order_id)))?;
    let preimage = order
        .preimage
        .ok_or_else(|| RpcError::BadRequest("Order has no preimage".into()))?;
    Ok(Json(json!({
        "order_id": order_id,
        "preimage": hex::encode(preimage),
    })))
}

/// POST /api/v1/debug/mocktime
///
/// Sets the regtest mock clock (regtest only). Time-dependent checks (e.g.
/// HTLC expiry) use this instead of the wall clock, enabling refund-path
/// testing without waiting for real time to pass. A `null` timestamp resets
/// to the wall clock.
pub async fn debug_mocktime(
    State(state): State<Arc<AppState>>,
    Json(req): Json<Value>,
) -> RpcResult<Json<Value>> {
    if !state.regtest {
        return Err(RpcError::Forbidden(
            "Mocktime is only available in regtest mode".into(),
        ));
    }
    let ts = req.get("timestamp").cloned();
    let new_time = match ts {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_u64(),
        _ => {
            return Err(RpcError::BadRequest(
                "timestamp must be a number or null".into(),
            ))
        }
    };
    *state.mock_time.write().await = new_time;
    Ok(Json(json!({ "mock_time": new_time })))
}

#[cfg(test)]
mod relay_floor_lockstep {
    #[test]
    fn wallet_minimum_matches_mempool_relay_policy() {
        assert_eq!(
            vtorrent_node::mempool::MIN_RELAY_FEE,
            vtorrent_wallet::tx_builder::MIN_ABSOLUTE_FEE_SATS,
            "wallet builder floor drifted from mempool relay policy"
        );
    }

    #[cfg(test)]
    mod swap_guard_tests {
        use crate::handlers::require_swap_stage;
        use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

        fn state_at(status: SwapStatus) -> SwapState {
            let mut s = SwapState::new([7u8; 32], [9u8; 32]);
            s.status = status;
            s
        }

        #[test]
        fn absent_state_is_allowed() {
            assert!(require_swap_stage(None, &[SwapStatus::Refunded]).is_ok());
        }

        #[test]
        fn terminal_states_block_everything() {
            for status in [SwapStatus::Claimed, SwapStatus::Refunded] {
                let s = state_at(status);
                assert!(
                    require_swap_stage(Some(&s), &[SwapStatus::Claimed, SwapStatus::Refunded])
                        .is_err()
                );
            }
        }

        #[test]
        fn funded_states_allow_claims() {
            for status in [
                SwapStatus::Funding,
                SwapStatus::VtrFunded,
                SwapStatus::BtcFunded,
            ] {
                let s = state_at(status);
                assert!(require_swap_stage(Some(&s), &[SwapStatus::Refunded]).is_ok());
            }
        }
    }
}
