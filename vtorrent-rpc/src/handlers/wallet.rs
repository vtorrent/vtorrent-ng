use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use super::{now_secs, verify_wallet_auth};
use crate::error::{RpcError, RpcResult};
use crate::models::*;
use crate::state::AppState;

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
                    "address query parameter is required when no wallet is imported — import a wallet first or provide ?address=".into(),
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
        return Err(RpcError::BadRequest(
            "WIF private key is required — provide a base58-encoded WIF string".into(),
        ));
    }
    if req.passphrase.is_empty() {
        return Err(RpcError::BadRequest(
            "Passphrase is required — used for Argon2id + ChaCha20-Poly1305 wallet encryption"
                .into(),
        ));
    }

    // Validate the WIF key and derive the address.
    let key = PrivateKey::from_wif(&req.wif).map_err(|e| {
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
    let address = pubkey_to_vtorrent_address(&pubkey.serialize()).map_err(|e| {
        RpcError::Internal(format!(
            "Failed to derive VTR address from public key: {}",
            e
        ))
    })?;

    // Encrypt the WIF with the passphrase so unlock/send can verify it.
    let encrypted =
        vtorrent_wallet::encryption::encrypt_wallet(req.wif.as_bytes(), &req.passphrase).map_err(
            |e| {
                RpcError::Internal(format!(
                    "Wallet encryption failed (Argon2id + ChaCha20-Poly1305): {}",
                    e
                ))
            },
        )?;

    *state.wallet_encrypted.write().await = Some(encrypted);
    *state.wallet_change_address.write().await = Some(address.clone());
    *state.wallet_totp_secret.write().await = req
        .otp_secret
        .as_deref()
        .map(vtorrent_wallet::otp::TotpSecret::from_base32)
        .transpose()
        .map_err(|e| {
            RpcError::BadRequest(format!(
                "Invalid TOTP secret: {} — expected base32-encoded secret",
                e
            ))
        })?;
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
/// Record the staking intent so it survives daemon restarts: the address is
/// re-armed automatically whenever the wallet is unlocked again.
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
    let bytes = serde_json::to_vec_pretty(&blob).map_err(|e| {
        RpcError::Internal(format!("Wallet serialize failed (JSON encoding): {}", e))
    })?;
    // Create the temp file with 0600 BEFORE writing content: setting the
    // mode after the write leaves a window where the encrypted wallet is
    // world-readable (default umask).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
            .map_err(|e| {
                RpcError::Internal(format!("Wallet write to {} failed: {}", tmp.display(), e))
            })?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, &bytes).map_err(|e| {
        RpcError::Internal(format!("Wallet write to {} failed: {}", tmp.display(), e))
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        RpcError::Internal(format!(
            "Wallet rename from {} to {} failed: {}",
            tmp.display(),
            path.display(),
            e
        ))
    })?;
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
        return Err(RpcError::BadRequest(
            "Amount must be greater than 0 — provide a positive satoshi amount".into(),
        ));
    }
    if req.to_address.is_empty() {
        return Err(RpcError::BadRequest(
            "Recipient address is required — provide a valid VTR address".into(),
        ));
    }

    // Re-verify the passphrase (and TOTP if 2FA is enabled) before signing.
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;

    // Retrieve the change address.
    let change_address = state
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| RpcError::Internal("Change address not set — wallet imported but no change address derived (state inconsistent)".into()))?;

    // Collect all UTXOs from the chain that belong to the hot wallet address.
    let utxos: Vec<vtorrent_node::chain::Utxo> = {
        let chain = state.chain.lock().await;
        chain.get_utxos_for_address(&change_address)
    };

    if utxos.is_empty() {
        return Err(RpcError::BadRequest(format!(
            "No UTXOs available for wallet address {} — fund the address first by sending VTR to it",
            &change_address[..change_address.len().min(64)]
        )));
    }

    // Build and sign the transaction using the mempool's recommended fee
    // rate, with the relay floor enforced as an absolute minimum so small
    // transfers are not rejected by our own node.
    let fee_rate = {
        let mempool = state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let tx = TxBuilder::new()
        .recipient(&req.to_address, req.amount_satoshis)
        .change_address(&change_address)
        .fee_rate(fee_rate)
        .min_absolute_fee(vtorrent_wallet::tx_builder::MIN_ABSOLUTE_FEE_SATS)
        .sign_with_wif(&wif)
        .build(&utxos)
        .map_err(|e| {
            RpcError::BadRequest(format!(
                "Transaction build failed ({} inputs, {} sats to {}): {}",
                utxos.len(),
                req.amount_satoshis,
                &req.to_address[..req.to_address.len().min(64)],
                e
            ))
        })?;

    let txid = hex::encode(tx.txid());
    // Actual fee = selected inputs − outputs. Summing the whole wallet UTXO
    // list here would overcount by every unselected coin.
    let fee_satoshis: u64 = tx
        .inputs
        .iter()
        .filter_map(|inp| {
            utxos
                .iter()
                .find(|u| u.txid == inp.prev_txid && u.vout == inp.prev_vout)
                .map(|u| u.value)
        })
        .sum::<u64>()
        .saturating_sub(tx.outputs.iter().map(|o| o.value).sum::<u64>());

    // Add to local mempool and broadcast to P2P network. Fee is recomputed
    // from the chain's UTXO set (lock order: chain → mempool) so mempool fee
    // statistics reflect reality rather than the builder's estimate.
    {
        let chain = state.chain.lock().await;
        let mut mempool = state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx.clone())
            .map_err(|e| {
                RpcError::BadRequest(format!(
                    "Mempool rejected transaction {} ({} sats to {}): {}",
                    txid,
                    req.amount_satoshis,
                    &req.to_address[..req.to_address.len().min(64)],
                    e
                ))
            })?;
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
        return Err(RpcError::BadRequest(
            "Passphrase is required — provide the wallet encryption passphrase".into(),
        ));
    }

    // Verify the passphrase (and TOTP if 2FA is enabled) and decrypt the WIF
    // into memory. The wallet stays locked unless the credentials are correct.
    let wif = verify_wallet_auth(&state, &req.passphrase, req.otp_code.as_deref()).await?;
    *state.wallet_wif.write().await = Some(wif.clone());

    // Re-derive the change address from the decrypted key. After a daemon
    // restart the wallet is restored from disk (locked); unlock must restore
    // the change address too, otherwise send/balance fail until re-import.
    {
        let key = vtorrent_core::keys::PrivateKey::from_wif(&wif)
            .map_err(|e| RpcError::Internal(format!("Decrypted WIF is invalid: {}", e)))?;
        let secret_key = secp256k1::SecretKey::from_slice(key.as_bytes())
            .map_err(|_| RpcError::Internal("Decrypted key is malformed".into()))?;
        let secp = secp256k1::Secp256k1::new();
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
        let address = vtorrent_wallet::tx_builder::pubkey_to_vtorrent_address(&pubkey.serialize())
            .map_err(|e| RpcError::Internal(format!("Failed to derive change address: {}", e)))?;
        *state.wallet_change_address.write().await = Some(address);
    }

    let expires_at = if req.timeout_secs == 0 {
        Some(0u64)
    } else {
        Some(now_secs().saturating_add(req.timeout_secs))
    };

    *state.wallet_unlock_expiry.write().await = expires_at;

    // Auto-resume staking if it was enabled before the last restart: the
    // intent file records the address; signing now works because the wallet
    // is unlocked.
    if let Some(path) = &state.staking_state_path {
        if let Ok(blob) = std::fs::read_to_string(path) {
            let enabled = serde_json::from_str::<serde_json::Value>(&blob)
                .ok()
                .and_then(|v| {
                    v.get("enabled")
                        .and_then(|e| e.as_bool())
                        .or_else(|| v.get("address").map(|_| true))
                })
                .unwrap_or(false);
            let addr = serde_json::from_str::<serde_json::Value>(&blob)
                .ok()
                .and_then(|v| {
                    v.get("address")
                        .and_then(|a| a.as_str())
                        .map(|s| s.to_string())
                });
            if enabled {
                if let Some(address) = addr {
                    tracing::info!("Auto-resuming staking for {} after unlock", address);
                    if let Some(tx) = &state.staking_control {
                        let _ = tx
                            .send(vtorrent_node::staking::StakingCommand::Start {
                                address: address.clone(),
                                wif: Some(
                                    state.wallet_wif.read().await.clone().unwrap_or_default(),
                                ),
                            })
                            .await;
                    }
                    *state.staking_enabled.write().await = true;
                    *state.staking_address.write().await = Some(address);
                }
            }
        }
    }

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

// ─── GET /api/v1/blockchain/utxo/:txid/:vout ─────────────────────────────────

#[derive(serde::Deserialize)]
pub struct GetTxOutParams {
    pub txid: String,
    pub vout: u32,
}

pub async fn get_txout(
    State(state): State<Arc<AppState>>,
    Path(params): Path<GetTxOutParams>,
) -> RpcResult<Json<GetTxOutResponse>> {
    let txid_bytes: [u8; 32] = super::parse_hash32(&params.txid, "txid")
        .map_err(|_| RpcError::BadRequest("Invalid txid hex".into()))?;

    let chain = state.chain.lock().await;
    let utxo = chain.get_utxo(&txid_bytes, params.vout).ok_or_else(|| {
        RpcError::NotFound(format!(
            "UTXO {}:{} not found or already spent",
            params.txid, params.vout
        ))
    })?;
    let coinbase = chain
        .get_transaction(&txid_bytes)
        .map(|(tx, _, _)| tx.is_coinbase() || tx.is_coinstake())
        .unwrap_or(false);

    Ok(Json(GetTxOutResponse {
        txid: params.txid,
        vout: utxo.vout,
        value_satoshis: utxo.value,
        script_pubkey: hex::encode(&utxo.script_pubkey),
        height: utxo.height,
        coinbase,
    }))
}
