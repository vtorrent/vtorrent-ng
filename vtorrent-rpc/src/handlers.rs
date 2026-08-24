use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use vtorrent_node::atomic_swap::{SwapState, SwapStatus};

use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;
use std::sync::Arc;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Current time in seconds, honoring the regtest mock clock if set.
async fn now_secs_mock(state: &AppState) -> u64 {
    if let Some(t) = *state.mock_time.read().await {
        t
    } else {
        now_secs()
    }
}

/// Broadcast a raw BTC transaction to the configured network/peer.
async fn broadcast_btc(state: &AppState, raw: &[u8]) -> RpcResult<[u8; 32]> {
    let network = *state.btc_network.read().await;
    let peer = *state.btc_peer.read().await;
    if let Some(addr) = peer {
        vtorrent_btc::sync::broadcast_tx_to(raw, network, &[addr])
            .await
            .map_err(|e| RpcError::Internal(format!("BTC broadcast failed: {}", e)))
    } else {
        vtorrent_btc::sync::broadcast_tx(raw)
            .await
            .map_err(|e| RpcError::Internal(format!("BTC broadcast failed: {}", e)))
    }
}

fn parse_hash32(value: &str, field: &str) -> RpcResult<[u8; 32]> {
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
fn btc_txid_hex(bytes: &[u8; 32]) -> String {
    let mut display = *bytes;
    display.reverse();
    hex::encode(display)
}

/// Reject swap operations whose current lifecycle stage makes them invalid
/// (e.g. double-funding, claiming after refund, refunding after claim).
fn require_swap_stage(
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

fn block_response(
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

fn transaction_lookup_response(
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

// ─── Node Info ────────────────────────────────────────────────────────────────

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
        let mut mempool = state.mempool.lock().await;
        // Verify the fee from the live UTXO set — same rule the P2P relay
        // path applies — so self-reported fee estimates cannot buy priority.
        let real_fee = {
            let chain = state.chain.lock().await;
            chain.compute_tx_fee(&tx)
        };
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

pub async fn get_balance(State(state): State<Arc<AppState>>) -> RpcResult<Json<BalanceResponse>> {
    let staking_enabled = *state.staking_enabled.read().await;
    let staking_address = state.staking_address.read().await.clone();
    let change_address = state.wallet_change_address.read().await.clone();

    // Compute confirmed + staking + our UTXO map under the chain lock, then
    // release it before locking the mempool.
    let (confirmed, staking, our_utxos, our_script) = {
        let chain = state.chain.lock().await;
        let confirmed: u64 = change_address
            .as_ref()
            .map(|addr| {
                chain
                    .get_utxos_for_address(addr)
                    .iter()
                    .map(|u| u.value)
                    .sum()
            })
            .unwrap_or(0);

        // The staking figure is the amount actually staked (UTXOs at the staking
        // address), not a fabricated fraction of the confirmed balance.
        let staking = if staking_enabled {
            staking_address
                .as_ref()
                .map(|addr| {
                    chain
                        .get_utxos_for_address(addr)
                        .iter()
                        .map(|u| u.value)
                        .sum()
                })
                .unwrap_or(0)
        } else {
            0
        };

        let our_utxos: std::collections::HashMap<([u8; 32], u32), u64> = change_address
            .as_ref()
            .map(|addr| {
                chain
                    .get_utxos_for_address(addr)
                    .into_iter()
                    .map(|u| ((u.txid, u.vout), u.value))
                    .collect()
            })
            .unwrap_or_default();

        let our_script = change_address
            .as_ref()
            .and_then(|addr| vtorrent_wallet::tx_builder::p2pkh_script_pubkey(addr).ok());

        (confirmed, staking, our_utxos, our_script)
    };

    // Unconfirmed = net pending activity for the hot wallet: mempool outputs
    // paying to us minus mempool inputs spending our confirmed UTXOs.
    let unconfirmed: u64 = if change_address.is_some() {
        let mempool = state.mempool.lock().await;
        let mp_txs = mempool.get_transactions();
        let mut incoming = 0u64;
        let mut outgoing = 0u64;
        for tx in &mp_txs {
            if let Some(script) = &our_script {
                for out in &tx.outputs {
                    if &out.script_pubkey == script {
                        incoming = incoming.saturating_add(out.value);
                    }
                }
            }
            for inp in &tx.inputs {
                if let Some(v) = our_utxos.get(&(inp.prev_txid, inp.prev_vout)) {
                    outgoing = outgoing.saturating_add(*v);
                }
            }
        }
        incoming.saturating_sub(outgoing)
    } else {
        0
    };

    Ok(Json(BalanceResponse {
        confirmed,
        unconfirmed,
        staking,
        display: format!("{:.6} VTR", confirmed as f64 / 100_000_000.0),
    }))
}

/// List spendable UTXOs for an explicitly requested or imported wallet address.
pub async fn get_wallet_utxos(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WalletUtxosQuery>,
) -> RpcResult<Json<WalletUtxosResponse>> {
    let address = match query.address.filter(|address| !address.trim().is_empty()) {
        Some(address) => address,
        None => state
            .wallet_change_address
            .read()
            .await
            .clone()
            .ok_or_else(|| {
                RpcError::BadRequest(
                    "address query parameter is required when no wallet is imported".into(),
                )
            })?,
    };

    let mut utxos = {
        let chain = state.chain.lock().await;
        chain.get_utxos_for_address(&address)
    };
    utxos.sort_by_key(|utxo| (utxo.height, utxo.txid, utxo.vout));
    let total_satoshis = utxos.iter().map(|utxo| utxo.value).sum();
    let utxos = utxos
        .into_iter()
        .map(|utxo| WalletUtxoResponse {
            txid: hex::encode(utxo.txid),
            vout: utxo.vout,
            value_satoshis: utxo.value,
            script_pubkey: hex::encode(utxo.script_pubkey),
            block_height: utxo.height,
            block_timestamp: utxo.timestamp,
        })
        .collect();

    Ok(Json(WalletUtxosResponse {
        address,
        total_satoshis,
        utxos,
    }))
}

pub async fn get_addresses(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<AddressesResponse>> {
    // Return only the hot wallet's addresses, not the entire network UTXO set
    // (which would leak the full chain address/balance map and scale with
    // network size rather than wallet size).
    let chain = state.chain.lock().await;
    let wallet_addr = state.wallet_change_address.read().await.clone();

    let addresses: Vec<AddressInfo> = match wallet_addr {
        Some(addr) => {
            let balance: u64 = chain
                .get_utxos_for_address(&addr)
                .iter()
                .map(|u| u.value)
                .sum();
            vec![AddressInfo {
                address: addr,
                label: None,
                balance,
                is_change: false,
            }]
        }
        None => Vec::new(),
    };

    Ok(Json(AddressesResponse { addresses }))
}

/// Verify the hot wallet passphrase (and TOTP code if 2FA is enabled) and
/// return the decrypted WIF. Fails if no wallet has been imported or the
/// credentials are incorrect.
async fn verify_wallet_auth(
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

/// POST /api/v1/wallet/import
///
/// Imports a WIF-encoded private key into the hot wallet.  The key is
/// encrypted with the provided passphrase (Argon2id + ChaCha20-Poly1305) and
/// persisted to `wallet_path` when configured (written 0600, atomic rename);
/// the plaintext never touches disk.  The wallet starts locked; call
/// `/api/v1/wallet/unlock` with the same passphrase to use it.
pub async fn import_wallet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ImportWalletRequest>,
) -> RpcResult<Json<ImportWalletResponse>> {
    use secp256k1::{Secp256k1, SecretKey};
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address;

    if req.wif.is_empty() {
        return Err(RpcError::BadRequest("WIF private key is required".into()));
    }
    if req.passphrase.is_empty() {
        return Err(RpcError::BadRequest("Passphrase is required".into()));
    }

    // Validate the WIF key and derive the address.
    let key = PrivateKey::from_wif(&req.wif)
        .map_err(|e| RpcError::BadRequest(format!("Invalid WIF key: {}", e)))?;
    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(key.as_bytes())
        .map_err(|e| RpcError::BadRequest(format!("Invalid key bytes: {}", e)))?;
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let address = pubkey_to_vtorrent_address(&pubkey.serialize())
        .map_err(|e| RpcError::Internal(e.to_string()))?;

    // Encrypt the WIF with the passphrase so unlock/send can verify it.
    let encrypted =
        vtorrent_wallet::encryption::encrypt_wallet(req.wif.as_bytes(), &req.passphrase)
            .map_err(|e| RpcError::Internal(format!("Wallet encryption failed: {}", e)))?;

    *state.wallet_encrypted.write().await = Some(encrypted);
    *state.wallet_change_address.write().await = Some(address.clone());
    *state.wallet_totp_secret.write().await = req
        .otp_secret
        .as_deref()
        .map(vtorrent_wallet::otp::TotpSecret::from_base32)
        .transpose()
        .map_err(|e| RpcError::BadRequest(format!("Invalid TOTP secret: {}", e)))?;
    // The wallet starts locked; the WIF is only decrypted into memory on unlock.
    *state.wallet_wif.write().await = None;
    *state.wallet_unlock_expiry.write().await = None;

    persist_wallet(&state).await?;

    tracing::info!("Hot wallet imported: {}", address);

    Ok(Json(ImportWalletResponse {
        address,
        success: true,
    }))
}

/// Write the encrypted hot wallet to `wallet_path` (0600, atomic rename).
/// No-op when persistence is disabled (standalone/test instances).
async fn persist_wallet(state: &AppState) -> RpcResult<()> {
    let Some(path) = &state.wallet_path else {
        return Ok(());
    };
    let encrypted = state.wallet_encrypted.read().await.clone();
    let Some(encrypted) = encrypted else {
        return Ok(());
    };
    let blob = serde_json::json!({ "version": 1u8, "wallet": encrypted });

    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&blob)
        .map_err(|e| RpcError::Internal(format!("Wallet serialize failed: {}", e)))?;
    std::fs::write(&tmp, &bytes)
        .map_err(|e| RpcError::Internal(format!("Wallet write failed: {}", e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| RpcError::Internal(format!("Wallet rename failed: {}", e)))?;
    tracing::info!("Encrypted wallet persisted to {}", path.display());
    Ok(())
}

/// POST /api/v1/wallet/send
///
/// Builds, signs, and broadcasts a real VTR transaction using the hot wallet
/// key imported via `/api/v1/wallet/import`.
///
/// The transaction is:
///   1. Built with `TxBuilder` (coin selection, change output, fee calculation)
///   2. Signed with the imported WIF key
///   3. Added to the local mempool
///   4. The txid is returned immediately; propagation to peers happens async
pub async fn send_vtr(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendRequest>,
) -> RpcResult<Json<SendResponse>> {
    use vtorrent_wallet::tx_builder::TxBuilder;

    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.amount_satoshis == 0 {
        return Err(RpcError::BadRequest("Amount must be greater than 0".into()));
    }
    if req.to_address.is_empty() {
        return Err(RpcError::BadRequest("Recipient address is required".into()));
    }

    // Re-verify the passphrase (and TOTP if 2FA is enabled) before signing.
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;

    // Retrieve the change address.
    let change_address = state
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcError::Internal("Change address not set".into()))?;

    // Collect all UTXOs from the chain that belong to the hot wallet address.
    let utxos: Vec<vtorrent_node::chain::Utxo> = {
        let chain = state.chain.lock().await;
        chain.get_utxos_for_address(&change_address)
    };

    if utxos.is_empty() {
        return Err(RpcError::BadRequest(
            "No UTXOs available for this wallet address. Fund the address first.".into(),
        ));
    }

    // Build and sign the transaction using the mempool's recommended fee rate.
    let fee_rate = {
        let mempool = state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let tx = TxBuilder::new()
        .recipient(&req.to_address, req.amount_satoshis)
        .change_address(&change_address)
        .fee_rate(fee_rate)
        .sign_with_wif(&wif)
        .build(&utxos)
        .map_err(|e| RpcError::BadRequest(format!("Transaction build failed: {}", e)))?;

    let txid = hex::encode(tx.txid());
    let fee_satoshis: u64 = utxos
        .iter()
        .map(|u| u.value)
        .sum::<u64>()
        .saturating_sub(tx.outputs.iter().map(|o| o.value).sum::<u64>());

    // Add to local mempool and broadcast to P2P network.
    {
        let mut mempool = state.mempool.lock().await;
        mempool
            .add_transaction(tx.clone())
            .map_err(|e| RpcError::BadRequest(format!("Mempool rejected transaction: {}", e)))?;
    }
    // If a live P2P node is attached, submit the tx for network broadcast.
    if let Some(ref sender) = state.tx_submit {
        let _ = sender.try_send(tx);
    }

    tracing::info!(
        "Transaction {} submitted to mempool and broadcast ({} sats to {})",
        txid,
        req.amount_satoshis,
        req.to_address
    );

    Ok(Json(SendResponse {
        txid,
        amount_satoshis: req.amount_satoshis,
        fee_satoshis,
    }))
}

pub async fn unlock_wallet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnlockRequest>,
) -> RpcResult<Json<UnlockResponse>> {
    if req.passphrase.is_empty() {
        return Err(RpcError::BadRequest("Passphrase is required".into()));
    }

    // Verify the passphrase (and TOTP if 2FA is enabled) and decrypt the WIF
    // into memory. The wallet stays locked unless the credentials are correct.
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    *state.wallet_wif.write().await = Some(wif);

    let expires_at = if req.timeout_secs == 0 {
        Some(0u64)
    } else {
        Some(now_secs().saturating_add(req.timeout_secs))
    };

    *state.wallet_unlock_expiry.write().await = expires_at;

    Ok(Json(UnlockResponse {
        success: true,
        expires_at,
    }))
}

pub async fn lock_wallet(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    // Clear the hot wallet key on lock for security.
    state.lock_wallet().await;
    Ok(Json(json!({ "success": true, "message": "Wallet locked" })))
}

/// GET /api/v1/wallet/transactions?limit=N
/// Returns the most recent confirmed transactions from the main chain.
pub async fn get_transactions(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> RpcResult<Json<Vec<TransactionResponse>>> {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(50)
        .min(500);

    let chain = state.chain.lock().await;
    let txs = chain.get_recent_transactions(limit);

    let result = txs
        .into_iter()
        .map(
            |(txid, height, ts, tx_type, amount, _fee)| TransactionResponse {
                display: format!("{:.6} VTR", amount as f64 / 100_000_000.0),
                txid,
                block_height: height,
                timestamp: ts,
                tx_type,
                amount_satoshis: amount,
            },
        )
        .collect();

    Ok(Json(result))
}

// ─── Staking ──────────────────────────────────────────────────────────────────

pub async fn get_staking_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<StakingStatusResponse>> {
    let enabled = *state.staking_enabled.read().await;
    let staking_address = state.staking_address.read().await.clone();
    let blocks_staked = *state.blocks_staked.read().await;
    let last_stake_time_raw = *state.last_stake_time.read().await;
    let chain = state.chain.lock().await;

    // Sum only the staking address's UTXOs, not the entire network UTXO set.
    let staking_utxos: Vec<vtorrent_node::chain::Utxo> = staking_address
        .as_ref()
        .map(|addr| chain.get_utxos_for_address(addr))
        .unwrap_or_default();
    let total_staking: u64 = staking_utxos.iter().map(|u| u.value).sum();
    let eligible_utxos = staking_utxos.len();

    let expected_per_day = if enabled {
        total_staking as f64 * 0.05 / 365.0
    } else {
        0.0
    };

    Ok(Json(StakingStatusResponse {
        enabled,
        staking_address,
        eligible_utxos,
        total_staking_satoshis: total_staking,
        expected_reward_per_day: expected_per_day,
        last_stake_time: if last_stake_time_raw == 0 {
            None
        } else {
            Some(last_stake_time_raw)
        },
        blocks_staked,
    }))
}

pub async fn start_staking(
    State(state): State<Arc<AppState>>,
    Json(req): Json<StakingStartRequest>,
) -> RpcResult<Json<Value>> {
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    if req.address.is_empty() {
        return Err(RpcError::BadRequest("Staking address is required".into()));
    }

    // Sign the coinstake with the unlocked hot-wallet key. If the requested
    // staking address is not owned by the hot wallet, coinstake signatures
    // will be rejected by the chain.
    let wif = state.wallet_wif.read().await.clone();

    if let Some(tx) = &state.staking_control {
        let _ = tx
            .send(vtorrent_node::staking::StakingCommand::Start {
                address: req.address.clone(),
                wif,
            })
            .await;
    }

    *state.staking_enabled.write().await = true;
    *state.staking_address.write().await = Some(req.address.clone());

    Ok(Json(json!({
        "success": true,
        "message": format!("Staking started for address {}", req.address)
    })))
}

pub async fn stop_staking(State(state): State<Arc<AppState>>) -> RpcResult<Json<Value>> {
    if let Some(tx) = &state.staking_control {
        let _ = tx.send(vtorrent_node::staking::StakingCommand::Stop).await;
    }
    *state.staking_enabled.write().await = false;
    *state.staking_address.write().await = None;
    Ok(Json(
        json!({ "success": true, "message": "Staking stopped" }),
    ))
}

// ─── Torrent ──────────────────────────────────────────────────────────────────

pub async fn list_torrent_sessions(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Vec<TorrentSessionResponse>>> {
    let sessions = state.torrent_sessions.read().await;
    let result: Vec<TorrentSessionResponse> = sessions
        .list_sessions()
        .iter()
        .map(|s| {
            let summary = s.incentive_summary();
            TorrentSessionResponse {
                id: s.id.clone(),
                name: s.metainfo.name.clone(),
                info_hash: hex::encode(s.metainfo.info_hash),
                state: s.state.to_string(),
                progress: s.progress(),
                size_bytes: s.metainfo.total_size,
                downloaded_bytes: s.bytes_downloaded,
                uploaded_bytes: s.bytes_uploaded,
                download_speed: s.download_speed,
                upload_speed: s.upload_speed,
                peer_count: s.peers.len(),
                vtr_earned_satoshis: summary.total_earned_satoshis,
                vtr_paid_satoshis: summary.total_paid_satoshis,
            }
        })
        .collect();

    Ok(Json(result))
}

pub async fn add_torrent(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddTorrentRequest>,
) -> RpcResult<Json<AddTorrentResponse>> {
    use base64::Engine as _;
    use vtorrent_torrent::metainfo::{MagnetLink, Metainfo};
    use vtorrent_torrent::session::TorrentSession;

    let metainfo = if req.source_type == "magnet" {
        let magnet =
            MagnetLink::parse(&req.source).map_err(|e| RpcError::BadRequest(e.to_string()))?;
        Metainfo::from_magnet_link(&magnet)
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&req.source)
            .map_err(|_| RpcError::BadRequest("Invalid base64 torrent data".into()))?;
        Metainfo::from_bytes(&bytes).map_err(|e| RpcError::BadRequest(e.to_string()))?
    };

    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo, req.wallet_address);
    let session_id = state.torrent_sessions.write().await.add_session(session);

    // Spawn the download engine for this session.
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&state.torrent_sessions);
    let download_dir = state.download_dir.read().await.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(sid, sessions, download_dir, cancel).await;
    });

    Ok(Json(AddTorrentResponse {
        session_id,
        info_hash,
        name,
    }))
}

pub async fn remove_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<Value>> {
    if let Some(cancel) = state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    let removed = state.torrent_sessions.write().await.remove_session(&id);
    if removed.is_none() {
        return Err(RpcError::NotFound(format!("Session {} not found", id)));
    }
    Ok(Json(
        json!({ "success": true, "message": format!("Session {} removed", id) }),
    ))
}

// ─── DEX ──────────────────────────────────────────────────────────────────────

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
            .ok_or_else(|| {
                RpcError::BadRequest("No single wallet UTXO can fund this HTLC".into())
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
            RpcError::BadRequest(format!("Unable to build HTLC funding transaction: {}", e))
        })?;
    let funding_tx =
        sign_custom_transaction(unsigned_funding, std::slice::from_ref(&funding_utxo), &wif)
            .map_err(|e| {
                RpcError::BadRequest(format!("Unable to sign HTLC funding transaction: {}", e))
            })?;
    let funding_txid = funding_tx.txid();

    // Reserve the order immediately before mempool admission so a second taker
    // cannot create a competing funding transaction for the same order.
    let reserved = state.order_book.write().await.begin_funding(&req.order_id);
    if reserved.is_none() {
        return Err(RpcError::NotFound(format!(
            "Order {} is no longer open",
            req.order_id
        )));
    }

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

// ─── Legacy Claim ──────────────────────────────────────────────────────────────────

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

    // 5. Submit to mempool.
    {
        let mut mempool = state.mempool.lock().await;
        mempool
            .add_transaction(tx)
            .map_err(|e| RpcError::BadRequest(format!("Mempool rejected claim: {}", e)))?;
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

fn utxo_select(
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

/// GET /api/v1/btc/status
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

    // Lifecycle guard: the BTC HTLC must not already be funded, and a
    // finished swap (claimed/refunded) can never be funded again.
    {
        let swaps = state.swaps.read().await;
        require_swap_stage(
            swaps.get(&req.order_id),
            &[
                SwapStatus::BtcFunded,
                SwapStatus::Claimed,
                SwapStatus::Refunded,
            ],
        )?;
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

    // Lifecycle guard: a swap that already completed (claimed or refunded)
    // cannot be claimed again.
    {
        let swaps = state.swaps.read().await;
        require_swap_stage(
            swaps.get(&req.order_id),
            &[SwapStatus::Claimed, SwapStatus::Refunded],
        )?;
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
        // Lifecycle guard: only a funded swap can be claimed, and only once.
        require_swap_stage(
            Some(swap),
            &[
                SwapStatus::Claimed,
                SwapStatus::Refunded,
                SwapStatus::Funding,
            ],
        )?;
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

    // Lifecycle guard: a swap that already completed cannot be refunded again.
    {
        let swaps = state.swaps.read().await;
        require_swap_stage(
            swaps.get(&req.order_id),
            &[SwapStatus::Claimed, SwapStatus::Refunded],
        )?;
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

// ─── Regtest Faucet ───────────────────────────────────────────────────────────

/// POST /api/v1/faucet
///
/// Mints coins to an address (regtest only). This is a development primitive
/// that lets a local node obtain spendable VTR without a legacy claim or a
/// 6-hour stake age, so the wallet/DEX/swap flow can be exercised end-to-end.
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
