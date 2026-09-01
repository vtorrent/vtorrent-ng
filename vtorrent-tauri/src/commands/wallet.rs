use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use tauri::State;

use vtorrent_migrate::extractor::extract_wallet;
use vtorrent_wallet::wallet::Wallet;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

fn persist_wallet(state: &State<AppState>, wallet: &Wallet) -> Result<()> {
    let path = state
        .wallet_path
        .lock()
        .unwrap()
        .clone()
        .ok_or(TauriError::WalletNotInitialized)?;
    wallet.save(&path).map_err(TauriError::from)
}

fn ensure_wallet_parent(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TauriError::Io(error.to_string()))?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub keys_found: usize,
    pub addresses: Vec<String>,
    pub had_encryption: bool,
    pub had_2fa: bool,
    pub claimable_balance: u64,
}

#[derive(Debug, Serialize)]
pub struct WalletInfo {
    pub is_unlocked: bool,
    pub has_2fa: bool,
    pub address_count: usize,
    pub default_address: Option<String>,
    pub wallet_version: u32,
}

#[derive(Debug, Serialize)]
pub struct AddressInfo {
    pub address: String,
    pub label: String,
    pub balance: u64,
    pub is_legacy_import: bool,
}

#[derive(Debug, Serialize)]
pub struct Enable2FAResult {
    pub uri: String,
    pub secret: String,
    pub qr_data: String,
}

#[derive(Debug, Serialize)]
pub struct BtcStatus {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
    pub synced: bool,
}

#[derive(Debug, Serialize)]
pub struct ClaimCheckResult {
    pub address: String,
    pub claimable_satoshis: u64,
    pub display: String,
    pub already_claimed: bool,
}

#[derive(Debug, Serialize)]
pub struct ClaimSubmitResult {
    pub txid: String,
    pub claimed_satoshis: u64,
    pub recipient_address: String,
}

// ─── UTXO snapshot helpers ──────────────────────────────────────────────────

static UTXO_SNAPSHOT: &[u8] = include_bytes!("../../utxo_snapshot.bin");

const SNAPSHOT_MAGIC: &[u8; 4] = b"VTRS";
const SNAPSHOT_HEADER_SIZE: usize = 12;
const ADDR_LEN: usize = 34;
const ENTRY_SIZE: usize = ADDR_LEN + 8;

fn lookup_snapshot_balances(addresses: &[String]) -> u64 {
    if UTXO_SNAPSHOT.len() < SNAPSHOT_HEADER_SIZE {
        return 0;
    }
    if &UTXO_SNAPSHOT[0..4] != SNAPSHOT_MAGIC {
        return 0;
    }

    let entry_count = u32::from_le_bytes([
        UTXO_SNAPSHOT[8],
        UTXO_SNAPSHOT[9],
        UTXO_SNAPSHOT[10],
        UTXO_SNAPSHOT[11],
    ]) as usize;

    let entries_start = SNAPSHOT_HEADER_SIZE;
    let entries_end = entries_start + entry_count * ENTRY_SIZE;
    if UTXO_SNAPSHOT.len() < entries_end {
        return 0;
    }
    let entries = &UTXO_SNAPSHOT[entries_start..entries_end];

    let mut total = 0u64;
    for address in addresses {
        let addr_bytes = address.as_bytes();
        if addr_bytes.len() > ADDR_LEN {
            continue;
        }

        let mut lo = 0usize;
        let mut hi = entry_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry_offset = mid * ENTRY_SIZE;
            let entry_addr = &entries[entry_offset..entry_offset + ADDR_LEN];

            let stored_len = entry_addr.iter().position(|&b| b == 0).unwrap_or(ADDR_LEN);
            let stored_addr = &entry_addr[..stored_len];

            match stored_addr.cmp(addr_bytes) {
                std::cmp::Ordering::Equal => {
                    let bal_offset = entry_offset + ADDR_LEN;
                    let bal = u64::from_le_bytes([
                        entries[bal_offset],
                        entries[bal_offset + 1],
                        entries[bal_offset + 2],
                        entries[bal_offset + 3],
                        entries[bal_offset + 4],
                        entries[bal_offset + 5],
                        entries[bal_offset + 6],
                        entries[bal_offset + 7],
                    ]);
                    total = total.saturating_add(bal);
                    break;
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
    }
    total
}

// ─── Wallet lifecycle commands ───────────────────────────────────────────────

#[tauri::command]
pub fn create_wallet(
    state: State<AppState>,
    passphrase: String,
    wallet_path: String,
) -> Result<WalletInfo> {
    if passphrase.len() < 8 {
        return Err(TauriError::InvalidInput(
            "Passphrase must be at least 8 characters".into(),
        ));
    }

    let path = std::path::PathBuf::from(&wallet_path);
    ensure_wallet_parent(&path)?;
    let wallet = Wallet::create(&passphrase).map_err(TauriError::from)?;

    wallet.save(&path).map_err(TauriError::from)?;

    let default_address = wallet.default_address().map(|a| a.to_string());
    let address_count = wallet.address_count();
    let has_2fa = wallet.has_2fa();

    *state.wallet.lock().unwrap() = Some(wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(WalletInfo {
        is_unlocked: true,
        has_2fa,
        address_count,
        default_address,
        wallet_version: 2,
    })
}

#[tauri::command]
pub fn open_wallet(
    state: State<AppState>,
    wallet_path: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<WalletInfo> {
    let path = std::path::PathBuf::from(&wallet_path);

    let wallet = Wallet::load(&path, &passphrase, otp_code.as_deref()).map_err(TauriError::from)?;

    let default_address = wallet.default_address().map(|a| a.to_string());
    let address_count = wallet.address_count();
    let has_2fa = wallet.has_2fa();

    *state.wallet.lock().unwrap() = Some(wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(WalletInfo {
        is_unlocked: true,
        has_2fa,
        address_count,
        default_address,
        wallet_version: 2,
    })
}

#[tauri::command]
pub fn lock_wallet(state: State<AppState>) -> Result<()> {
    *state.wallet.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub fn get_wallet_info(state: State<AppState>) -> Result<WalletInfo> {
    let guard = state.wallet.lock().unwrap();
    match &*guard {
        Some(wallet) => Ok(WalletInfo {
            is_unlocked: true,
            has_2fa: wallet.has_2fa(),
            address_count: wallet.address_count(),
            default_address: wallet.default_address().map(|a| a.to_string()),
            wallet_version: 2,
        }),
        None => Ok(WalletInfo {
            is_unlocked: false,
            has_2fa: false,
            address_count: 0,
            default_address: None,
            wallet_version: 0,
        }),
    }
}

// ─── Legacy wallet import commands ──────────────────────────────────────────

#[tauri::command]
pub fn import_legacy_wallet(
    state: State<AppState>,
    wallet_dat_base64: String,
    passphrase: Option<String>,
    new_wallet_passphrase: String,
    new_wallet_path: String,
) -> Result<ImportResult> {
    let wallet_bytes = B64
        .decode(&wallet_dat_base64)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid base64: {}", e)))?;

    if new_wallet_passphrase.is_empty() {
        return Err(TauriError::InvalidInput(
            "A new wallet passphrase is required".into(),
        ));
    }

    let extraction =
        extract_wallet(&wallet_bytes, passphrase.as_deref()).map_err(TauriError::from)?;

    if extraction.keys.is_empty() {
        return Err(TauriError::Migration("No keys found in wallet.dat".into()));
    }

    let path = std::path::PathBuf::from(&new_wallet_path);
    ensure_wallet_parent(&path)?;
    let mut new_wallet = Wallet::create(&new_wallet_passphrase).map_err(TauriError::from)?;

    let mut addresses = Vec::new();
    for key in &extraction.keys {
        let addr = new_wallet
            .import_wif(&key.wif, Some(&key.legacy_address))
            .map_err(TauriError::from)?;
        addresses.push(addr.to_string());
    }

    new_wallet.save(&path).map_err(TauriError::from)?;

    let claimable_balance = lookup_snapshot_balances(&addresses);

    *state.wallet.lock().unwrap() = Some(new_wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(ImportResult {
        keys_found: extraction.keys.len(),
        addresses,
        had_encryption: extraction.was_encrypted,
        had_2fa: extraction.had_2fa,
        claimable_balance,
    })
}

// ─── Address management commands ─────────────────────────────────────────────

#[tauri::command]
pub fn generate_address(state: State<AppState>, label: Option<String>) -> Result<AddressInfo> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let address = wallet
        .generate_key(label.as_deref())
        .map_err(TauriError::from)?;
    let address_string = address.to_string();
    persist_wallet(&state, wallet)?;

    Ok(AddressInfo {
        address: address_string,
        label: label.unwrap_or_else(|| "New Address".into()),
        balance: 0,
        is_legacy_import: false,
    })
}

#[tauri::command]
pub async fn get_addresses(state: tauri::State<'_, AppState>) -> Result<Vec<AddressInfo>> {
    let wallet_addrs: Vec<(String, String, u64, bool)> = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        wallet.list_addresses()
    };

    let chain_guard = state.node.lock().await;
    let chain = chain_guard.as_ref().map(|h| h.rpc_state.chain.clone());
    drop(chain_guard);

    let balances: Vec<u64> = if let Some(chain) = chain {
        let chain = chain.lock().await;
        wallet_addrs
            .iter()
            .map(|(addr, _, _, _)| {
                chain
                    .get_utxos_for_address(addr)
                    .iter()
                    .map(|u| u.value)
                    .sum()
            })
            .collect()
    } else {
        wallet_addrs.iter().map(|(_, _, bal, _)| *bal).collect()
    };

    Ok(wallet_addrs
        .into_iter()
        .zip(balances)
        .map(
            |((address, label, _, is_legacy_import), balance)| AddressInfo {
                address,
                label,
                balance,
                is_legacy_import,
            },
        )
        .collect())
}

// ─── 2FA commands ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn enable_2fa(state: State<AppState>) -> Result<Enable2FAResult> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let config = wallet.enable_2fa().map_err(TauriError::from)?;
    persist_wallet(&state, wallet)?;

    Ok(Enable2FAResult {
        uri: config.to_uri("vTorrent-Wallet"),
        secret: config.secret_base32(),
        qr_data: config.to_uri("vTorrent-Wallet"),
    })
}

#[tauri::command]
pub fn verify_2fa(state: State<AppState>, code: String) -> Result<bool> {
    let guard = state.wallet.lock().unwrap();
    let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;

    let valid = wallet.verify_2fa(&code).map_err(TauriError::from)?;
    Ok(valid)
}

#[tauri::command]
pub fn disable_2fa(state: State<AppState>, code: String) -> Result<()> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    if !wallet.verify_2fa(&code).map_err(TauriError::from)? {
        return Err(TauriError::TwoFAFailed);
    }

    wallet.disable_2fa().map_err(TauriError::from)?;
    persist_wallet(&state, wallet)?;
    Ok(())
}

// ─── Send VTR ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn send_vtr(
    state: tauri::State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
) -> Result<String> {
    let (wif, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let wif = wallet
            .get_default_wif()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        let from = wallet
            .default_address()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        (wif, from)
    };

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    let utxos = {
        let chain = handle.rpc_state.chain.lock().await;
        chain.get_utxos_for_address(&from_address)
    };

    let fee_rate = {
        let mempool = handle.rpc_state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let tx = vtorrent_wallet_service::build_payment(
        &utxos,
        &to_address,
        &from_address,
        amount_satoshis,
        fee_rate,
        &wif,
    )
    .map_err(|e| TauriError::NodeError(format!("Transaction build failed: {}", e)))?;

    let txid = hex::encode(tx.txid());

    {
        let chain = handle.rpc_state.chain.lock().await;
        let mut mempool = handle.rpc_state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx)
            .map_err(|e| TauriError::NodeError(format!("Mempool rejected tx: {}", e)))?;
    }

    tracing::info!(
        "Sent {} satoshis to {} (txid: {})",
        amount_satoshis,
        to_address,
        txid
    );
    Ok(txid)
}

// ─── Bitcoin wallet commands ─────────────────────────────────────────────────

#[tauri::command]
pub async fn get_btc_status(state: State<'_, AppState>) -> Result<BtcStatus> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let btc = handle.rpc_state.btc_wallet.read().await;
    match btc.as_ref() {
        None => Ok(BtcStatus {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
            synced: false,
        }),
        Some(wallet) => Ok(BtcStatus {
            initialized: true,
            balance_satoshis: wallet.balance(),
            address: wallet.current_address().ok(),
            best_height: wallet.best_height(),
            synced: wallet.synced(),
        }),
    }
}

#[tauri::command]
pub async fn get_btc_address(state: State<'_, AppState>) -> Result<String> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let btc = handle.rpc_state.btc_wallet.read().await;
    match btc.as_ref() {
        None => Err(TauriError::NodeError("BTC wallet not initialized".into())),
        Some(wallet) => wallet
            .current_address()
            .map_err(|e| TauriError::NodeError(e.to_string())),
    }
}

#[tauri::command]
pub async fn send_btc(
    state: State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
) -> Result<String> {
    if to_address.trim().is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }
    if amount_satoshis == 0 {
        return Err(TauriError::InvalidInput("Amount must be non-zero".into()));
    }

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    let (txid_hex, raw, spent_utxos) = {
        let mut btc = handle.rpc_state.btc_wallet.write().await;
        let wallet = btc
            .as_mut()
            .ok_or_else(|| TauriError::NodeError("BTC wallet not initialized".into()))?;
        wallet
            .send_to(&to_address, amount_satoshis, 1_000)
            .map_err(|e| TauriError::NodeError(e.to_string()))?
    };

    let broadcast_result = {
        let network = *handle.rpc_state.btc_network.read().await;
        let peer = handle.rpc_state.btc_peer.read().await.clone();
        if let Some(host) = peer {
            let addr = tokio::net::lookup_host(&host)
                .await
                .ok()
                .and_then(|mut it| it.next())
                .ok_or_else(|| {
                    crate::error::TauriError::InvalidInput(format!("BTC peer {} unresolved", host))
                })?;
            vtorrent_btc::sync::broadcast_tx_to(&raw, network, &[addr]).await
        } else {
            vtorrent_btc::sync::broadcast_tx(&raw).await
        }
    };
    if let Err(e) = broadcast_result {
        if let Some(wallet) = handle.rpc_state.btc_wallet.write().await.as_mut() {
            wallet.restore_utxos(&spent_utxos);
        }
        return Err(TauriError::NodeError(format!(
            "BTC broadcast failed: {}",
            e
        )));
    }

    tracing::info!("BTC tx broadcast: {}", txid_hex);
    Ok(txid_hex)
}

// ─── Legacy claim commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn check_legacy_claim(
    state: State<'_, AppState>,
    legacy_address: String,
) -> Result<ClaimCheckResult> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let chain = handle.rpc_state.chain.lock().await;
    let claimable = vtorrent_node::genesis::get_legacy_balance(&legacy_address);
    let already_claimed = chain.is_claimed(&legacy_address);
    Ok(ClaimCheckResult {
        address: legacy_address,
        claimable_satoshis: claimable,
        display: format!("{:.6} VTR", claimable as f64 / 100_000_000.0),
        already_claimed,
    })
}

#[tauri::command]
pub async fn submit_legacy_claim(
    state: State<'_, AppState>,
    wif_private_key: String,
    recipient_address: String,
) -> Result<ClaimSubmitResult> {
    use vtorrent_core::keys::PrivateKey;
    use vtorrent_node::block::{Transaction, TxOutput, TxType};
    use vtorrent_node::genesis::get_legacy_balance;
    use vtorrent_wallet::tx_builder::{p2pkh_script_pubkey, pubkey_to_vtorrent_address};

    if wif_private_key.is_empty() {
        return Err(TauriError::InvalidInput(
            "WIF private key is required".into(),
        ));
    }
    if recipient_address.is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }

    let key = PrivateKey::from_wif(&wif_private_key)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid WIF key: {}", e)))?;
    let secp = secp256k1::Secp256k1::new();
    let secret_key = secp256k1::SecretKey::from_slice(key.as_bytes())
        .map_err(|e| TauriError::InvalidInput(format!("Invalid key bytes: {}", e)))?;
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_bytes = vtorrent_core::keys::serialize_pubkey(&pubkey, key.is_compressed());
    let derived_address = pubkey_to_vtorrent_address(&pubkey_bytes)
        .map_err(|e| TauriError::NodeError(e.to_string()))?;

    let claimable = get_legacy_balance(&derived_address);
    if claimable == 0 {
        return Err(TauriError::InvalidInput(format!(
            "No claimable balance for address {}",
            derived_address
        )));
    }

    let txid;
    {
        let guard = state.node.lock().await;
        let handle = guard
            .as_ref()
            .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

        {
            let chain = handle.rpc_state.chain.lock().await;
            if chain.is_claimed(&derived_address) {
                return Err(TauriError::InvalidInput(format!(
                    "Address {} has already been claimed",
                    derived_address
                )));
            }
        }

        let script_pubkey = p2pkh_script_pubkey(&recipient_address)
            .map_err(|e| TauriError::InvalidInput(format!("Invalid recipient address: {}", e)))?;

        let msg_hash = vtorrent_node::consensus::claim_message_hash(&derived_address);
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
            outputs: vec![TxOutput {
                value: claimable,
                script_pubkey,
            }],
            lock_time: 0,
            claim_address: Some(derived_address.clone()),
            claim_signature: Some(sig_bytes),
        };

        txid = hex::encode(tx.txid());

        let real_fee = {
            let chain = handle.rpc_state.chain.lock().await;
            chain.compute_tx_fee(&tx)
        };
        let mut mempool = handle.rpc_state.mempool.lock().await;
        match real_fee {
            Some(fee) => mempool
                .add_transaction_with_fee(tx, fee)
                .map_err(|e| TauriError::NodeError(format!("Failed to submit claim: {}", e)))?,
            None => {
                return Err(TauriError::NodeError(
                    "Claim inputs not found in UTXO set".into(),
                ))
            }
        }
    }

    tracing::info!(
        "Legacy claim submitted: {} VTR from {} to {}",
        claimable as f64 / 100_000_000.0,
        derived_address,
        recipient_address
    );

    Ok(ClaimSubmitResult {
        txid,
        claimed_satoshis: claimable,
        recipient_address,
    })
}
