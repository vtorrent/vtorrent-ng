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

pub(crate) fn ensure_node_stopped(state: &AppState) -> Result<()> {
    match state.node.try_lock() {
        Ok(guard) if guard.is_some() => Err(TauriError::NodeError(
            "Stop the node before switching wallets".into(),
        )),
        Ok(_) => Ok(()),
        Err(_) => Err(TauriError::NodeError(
            "Node is busy; stop it before switching wallets".into(),
        )),
    }
}

fn ensure_wallet_path_allowed(state: &AppState, path: &std::path::Path) -> Result<()> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(TauriError::InvalidInput(
            "Wallet path must not contain '..'".into(),
        ));
    }
    let root =
        state.app_data_dir.lock().unwrap().clone().ok_or_else(|| {
            TauriError::InvalidInput("Wallet directory is not initialized".into())
        })?;
    let root = root.canonicalize().unwrap_or(root);
    let parent = path.parent().unwrap_or(path);
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if !parent.starts_with(&root) {
        return Err(TauriError::InvalidInput(
            "Wallet path must be inside the app data directory".into(),
        ));
    }
    if path.exists() {
        let target = path
            .canonicalize()
            .map_err(|error| TauriError::Io(error.to_string()))?;
        if !target.starts_with(&root) {
            return Err(TauriError::InvalidInput(
                "Wallet path must be inside the app data directory".into(),
            ));
        }
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
    ensure_node_stopped(state.inner())?;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    let passphrase = zeroize::Zeroizing::new(passphrase);
    if passphrase.len() < 8 {
        return Err(TauriError::InvalidInput(
            "Passphrase must be at least 8 characters".into(),
        ));
    }

    let path = std::path::PathBuf::from(&wallet_path);
    if path.exists() {
        return Err(TauriError::InvalidInput(
            "A wallet already exists at this path; open it instead".into(),
        ));
    }
    ensure_wallet_path_allowed(state.inner(), &path)?;
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
pub async fn open_wallet(
    state: State<'_, AppState>,
    wallet_path: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<WalletInfo> {
    ensure_node_stopped(state.inner())?;
    state.cold_stake_scripts.lock().unwrap().clear();
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let path = std::path::PathBuf::from(&wallet_path);
    ensure_wallet_path_allowed(state.inner(), &path)?;

    let wallet = Wallet::load(
        &path,
        &passphrase,
        otp_code.as_ref().map(|code| code.as_str()),
    )
    .map_err(TauriError::from)?;

    let default_address = wallet.default_address().map(|a| a.to_string());
    let address_count = wallet.address_count();
    let has_2fa = wallet.has_2fa();

    *state.wallet.lock().unwrap() = Some(wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    {
        let node = state.node.lock().await;
        if let Some(handle) = node.as_ref() {
            if let Err(error) = state.sync_swap_wallet(&handle.rpc_state).await {
                *state.wallet.lock().unwrap() = None;
                return Err(error);
            }
            vtorrent_rpc::handlers::wallet::resume_staking_if_requested(&handle.rpc_state).await;
        }
    }

    Ok(WalletInfo {
        is_unlocked: true,
        has_2fa,
        address_count,
        default_address,
        wallet_version: 2,
    })
}

#[tauri::command]
pub async fn lock_wallet(state: State<'_, AppState>) -> Result<()> {
    *state.wallet.lock().unwrap() = None;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    if let Some(handle) = state.node.lock().await.as_ref() {
        handle.rpc_state.lock_wallet().await;
    }
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
    ensure_node_stopped(state.inner())?;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    let wallet_dat_base64 = zeroize::Zeroizing::new(wallet_dat_base64);
    let passphrase = passphrase.map(zeroize::Zeroizing::new);
    let new_wallet_passphrase = zeroize::Zeroizing::new(new_wallet_passphrase);
    let wallet_bytes = zeroize::Zeroizing::new(
        B64.decode(&wallet_dat_base64)
            .map_err(|e| TauriError::InvalidInput(format!("Invalid base64: {}", e)))?,
    );

    if new_wallet_passphrase.is_empty() {
        return Err(TauriError::InvalidInput(
            "A new wallet passphrase is required".into(),
        ));
    }

    let extraction = extract_wallet(&wallet_bytes, passphrase.as_ref().map(|p| p.as_str()))
        .map_err(TauriError::from)?;

    if extraction.keys.is_empty() {
        return Err(TauriError::Migration("No keys found in wallet.dat".into()));
    }

    let path = std::path::PathBuf::from(&new_wallet_path);
    ensure_wallet_path_allowed(state.inner(), &path)?;
    ensure_wallet_parent(&path)?;
    if path.exists() {
        return Err(TauriError::InvalidInput(
            "A wallet already exists at this path; open it instead".into(),
        ));
    }
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

    Ok(Enable2FAResult {
        uri: config.to_uri("vTorrent-Wallet"),
        secret: config.secret_base32(),
        qr_data: config.to_uri("vTorrent-Wallet"),
    })
}

#[tauri::command]
pub fn verify_2fa(state: State<AppState>, code: String) -> Result<bool> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    if wallet.has_pending_2fa() {
        let pending = wallet.pending_2fa_config();
        let valid = wallet
            .confirm_pending_2fa(&code)
            .map_err(TauriError::from)?;
        if valid {
            if let Err(error) = persist_wallet(&state, wallet) {
                if let Some(pending) = pending {
                    wallet.restore_pending_2fa(pending);
                }
                return Err(error);
            }
        }
        return Ok(valid);
    }

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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VtrQuote {
    pub quote_id: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: u64,
    pub total_debit_satoshis: u64,
    pub change_satoshis: u64,
    pub input_count: usize,
    pub fee_rate: u64,
    pub network: String,
}

/// Build the transaction a send would broadcast and return its exact fee,
/// change, and total debit without admitting it to the mempool.
#[tauri::command]
pub async fn quote_vtr(
    state: tauri::State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
) -> Result<VtrQuote> {
    if to_address.trim().is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }
    if amount_satoshis == 0 {
        return Err(TauriError::InvalidInput("Amount must be non-zero".into()));
    }
    let (wif, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let wif = zeroize::Zeroizing::new(
            wallet
                .get_default_wif()
                .ok_or(TauriError::WalletNotInitialized)?
                .to_string(),
        );
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
        wif.as_str(),
    )
    .map_err(|e| TauriError::NodeError(format!("Transaction build failed: {}", e)))?;

    let input_total: u64 = tx
        .inputs
        .iter()
        .filter_map(|input| {
            utxos
                .iter()
                .find(|utxo| utxo.txid == input.prev_txid && utxo.vout == input.prev_vout)
                .map(|utxo| utxo.value)
        })
        .sum();
    let output_total: u64 = tx.outputs.iter().map(|output| output.value).sum();
    let fee_satoshis = input_total.saturating_sub(output_total);
    let change_script = vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&from_address)
        .map_err(|e| TauriError::NodeError(e.to_string()))?;
    let change_satoshis = tx
        .outputs
        .iter()
        .rev()
        .find(|output| output.script_pubkey == change_script)
        .map(|output| output.value)
        .unwrap_or(0);

    let input_count = tx.inputs.len();
    let quote_id = format!(
        "{:016x}{:016x}",
        rand::random::<u64>(),
        rand::random::<u64>()
    );
    {
        let mut prepared = state.prepared_sends.lock().unwrap();
        prepared
            .retain(|_, entry| entry.created_at.elapsed() < std::time::Duration::from_secs(300));
        prepared.insert(
            quote_id.clone(),
            crate::state::PreparedSend {
                tx,
                created_at: std::time::Instant::now(),
            },
        );
    }

    Ok(VtrQuote {
        quote_id,
        from_address,
        to_address,
        amount_satoshis,
        fee_satoshis,
        total_debit_satoshis: amount_satoshis.saturating_add(fee_satoshis),
        change_satoshis,
        input_count,
        fee_rate,
        network: handle.network.clone(),
    })
}

#[tauri::command]
pub async fn send_vtr(
    state: tauri::State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
    quote_id: Option<String>,
) -> Result<String> {
    if to_address.trim().is_empty() {
        return Err(TauriError::InvalidInput(
            "Recipient address is required".into(),
        ));
    }
    if amount_satoshis == 0 {
        return Err(TauriError::InvalidInput("Amount must be non-zero".into()));
    }
    let (wif, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let wif = zeroize::Zeroizing::new(
            wallet
                .get_default_wif()
                .ok_or(TauriError::WalletNotInitialized)?
                .to_string(),
        );
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
    let tx = if let Some(quote_id) = quote_id {
        let mut prepared = state.prepared_sends.lock().unwrap();
        prepared
            .retain(|_, entry| entry.created_at.elapsed() < std::time::Duration::from_secs(300));
        prepared
            .remove(&quote_id)
            .ok_or_else(|| {
                TauriError::InvalidInput(
                    "Send quote expired or not found; review the transaction again".into(),
                )
            })?
            .tx
    } else {
        vtorrent_wallet_service::build_payment(
            &utxos,
            &to_address,
            &from_address,
            amount_satoshis,
            fee_rate,
            wif.as_str(),
        )
        .map_err(|e| {
            let message = format!("Transaction build failed: {}", e);
            crate::activity_journal::record_failed(state.inner(), &message);
            TauriError::NodeError(message)
        })?
    };

    let recipient_script = vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&to_address)
        .map_err(|e| TauriError::NodeError(e.to_string()))?;
    if !tx
        .outputs
        .iter()
        .any(|output| output.script_pubkey == recipient_script && output.value == amount_satoshis)
    {
        return Err(TauriError::InvalidInput(
            "Send quote does not match the requested recipient or amount".into(),
        ));
    }

    let txid = hex::encode(tx.txid());

    {
        let chain = handle.rpc_state.chain.lock().await;
        let mut mempool = handle.rpc_state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx.clone())
            .map_err(|e| {
                let message = format!("Mempool rejected tx: {}", e);
                crate::activity_journal::record_failed(state.inner(), &message);
                TauriError::NodeError(message)
            })?;
    }

    // Relay the transaction to peers. The RPC path uses the same channel; the
    // desktop path previously stopped at the local mempool.
    if let Some(sender) = &handle.rpc_state.tx_submit {
        if sender.try_send(tx).is_err() {
            tracing::warn!(
                "tx_submit channel full; transaction {} remains in the local mempool",
                txid
            );
        }
    }

    crate::activity_journal::record_broadcast(state.inner(), &txid, "send", amount_satoshis);
    tracing::info!(
        "Sent {} satoshis to {} (txid: {})",
        amount_satoshis,
        to_address,
        txid
    );
    Ok(txid)
}

// ─── Cold staking (P2CS) ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ColdStakeKeys {
    pub script_pubkey: String,
    pub script_hash: String,
    pub staking_wif: zeroize::Zeroizing<String>,
    pub spending_wif: zeroize::Zeroizing<String>,
    pub staking_address: String,
    pub spending_address: String,
    pub locktime: u32,
}

/// Generate a P2CS key pair. The raw script is the only stakeable form; the
/// spending key is returned once and is not persisted.
#[tauri::command]
pub async fn create_cold_stake(
    state: State<'_, AppState>,
    locktime: u32,
    label: Option<String>,
) -> Result<ColdStakeKeys> {
    const MAX_LOCKTIME: u32 = 2_100_000_000;
    if locktime != 0 && locktime > MAX_LOCKTIME {
        return Err(TauriError::InvalidInput(format!(
            "locktime {locktime} exceeds maximum {MAX_LOCKTIME}"
        )));
    }
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
    let keys = wallet
        .generate_cold_stake_keys(locktime, label.as_deref())
        .map_err(TauriError::from)?;
    let script_hash = hex::encode(vtorrent_core::crypto::hash160(&keys.script_pubkey));
    if let Some(wallet_path) = state.wallet_path.lock().unwrap().clone() {
        vtorrent_wallet::cold_stake_registry::add(&wallet_path, &script_hash).map_err(|e| {
            TauriError::Io(format!("Could not persist cold-stake script hash: {e}"))
        })?;
    }
    persist_wallet(&state, wallet)?;
    state
        .cold_stake_scripts
        .lock()
        .unwrap()
        .insert(keys.script_pubkey.clone());
    Ok(ColdStakeKeys {
        script_pubkey: hex::encode(&keys.script_pubkey),
        script_hash,
        staking_wif: keys.staking_wif,
        spending_wif: keys.spending_wif,
        staking_address: keys.staking_address,
        spending_address: keys.spending_address,
        locktime: keys.locktime,
    })
}

/// Fund a P2CS script generated by this session. The script is paid directly;
/// the P2SH label is not used.
#[tauri::command]
pub async fn fund_cold_stake(
    state: State<'_, AppState>,
    script_pubkey: String,
    amount_satoshis: u64,
) -> Result<String> {
    if amount_satoshis == 0 {
        return Err(TauriError::InvalidInput("Amount must be non-zero".into()));
    }
    let script = hex::decode(script_pubkey.trim())
        .map_err(|e| TauriError::InvalidInput(format!("script_pubkey is not valid hex: {e}")))?;
    let parsed = vtorrent_script::Script::from_bytes(script.clone()).map_err(|e| {
        TauriError::InvalidInput(format!("script_pubkey is not a valid script: {e}"))
    })?;
    let vtorrent_script::ScriptType::P2CS { locktime, .. } =
        vtorrent_script::classify_script(&parsed)
    else {
        return Err(TauriError::InvalidInput(
            "script_pubkey is not a P2CS cold-stake script".into(),
        ));
    };
    if locktime > 2_100_000_000 {
        return Err(TauriError::InvalidInput(format!(
            "P2CS locktime {locktime} exceeds the maximum 2100000000"
        )));
    }
    let script_hash = hex::encode(vtorrent_core::crypto::hash160(&script));
    let durable = state
        .wallet_path
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|wallet_path| {
            vtorrent_wallet::cold_stake_registry::contains(wallet_path, &script_hash)
        });
    let in_memory = state.cold_stake_scripts.lock().unwrap().contains(&script);
    let in_wallet = state
        .wallet
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|wallet| wallet.has_cold_stake_script(&script));
    if !durable && !in_memory && !in_wallet {
        return Err(TauriError::InvalidInput(
            "Cold-stake script was not generated by this wallet".into(),
        ));
    }
    let (wif, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let wif = zeroize::Zeroizing::new(
            wallet
                .get_default_wif()
                .ok_or(TauriError::WalletNotInitialized)?
                .to_string(),
        );
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
    let tx = vtorrent_wallet_service::build_payment_to_script(
        &utxos,
        script,
        &from_address,
        amount_satoshis,
        fee_rate,
        wif.as_str(),
    )
    .map_err(|e| {
        let message = format!("Cold-stake funding build failed: {e}");
        crate::activity_journal::record_failed(state.inner(), &message);
        TauriError::NodeError(message)
    })?;
    let txid = hex::encode(tx.txid());
    {
        let chain = handle.rpc_state.chain.lock().await;
        let mut mempool = handle.rpc_state.mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx.clone())
            .map_err(|e| {
                let message = format!("Mempool rejected tx: {e}");
                crate::activity_journal::record_failed(state.inner(), &message);
                TauriError::NodeError(message)
            })?;
    }
    if let Some(sender) = &handle.rpc_state.tx_submit {
        if sender.try_send(tx).is_err() {
            tracing::warn!(
                "tx_submit channel full; cold-stake funding {} remains in the local mempool",
                txid
            );
        }
    }
    crate::activity_journal::record_broadcast(state.inner(), &txid, "send", amount_satoshis);
    tracing::info!(
        "Funded cold-stake script with {} satoshis (txid: {})",
        amount_satoshis,
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

async fn broadcast_btc_tx(
    rpc: &vtorrent_rpc::state::AppState,
    raw: &[u8],
) -> std::result::Result<(), String> {
    let network = *rpc.btc_network.read().await;
    let peer = rpc.btc_peer.read().await.clone();
    if let Some(host) = peer {
        let addr = tokio::net::lookup_host(&host)
            .await
            .ok()
            .and_then(|mut it| it.next())
            .ok_or_else(|| format!("BTC peer {} unresolved", host))?;
        vtorrent_btc::sync::broadcast_tx_to(raw, network, &[addr])
            .await
            .map(|_| ())
            .map_err(|e| format!("BTC broadcast to {} failed: {}", host, e))
    } else if network == bitcoin::Network::Bitcoin {
        vtorrent_btc::sync::broadcast_tx(raw)
            .await
            .map(|_| ())
            .map_err(|e| format!("BTC broadcast failed: {}", e))
    } else {
        Err("BTC peer required for non-mainnet broadcast".to_string())
    }
}

#[tauri::command]
pub async fn send_btc(
    state: State<'_, AppState>,
    to_address: String,
    amount_satoshis: u64,
    passphrase: String,
    otp_code: Option<String>,
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

    // Spending the node's BTC wallet requires an unlocked wallet, matching the
    // VTR path. The desktop command must enforce this too: it talks to the
    // wallet directly and would otherwise bypass the RPC handler's check.
    vtorrent_rpc::handlers::authorize_wallet_operation(
        &handle.rpc_state,
        &passphrase,
        otp_code.as_deref(),
    )
    .await
    .map_err(|error| TauriError::InvalidInput(error.to_string()))?;

    let (txid_hex, raw, spent_utxos) = {
        let mut btc = handle.rpc_state.btc_wallet.write().await;
        let wallet = btc
            .as_mut()
            .ok_or_else(|| TauriError::NodeError("BTC wallet not initialized".into()))?;
        wallet
            .send_to(&to_address, amount_satoshis, 1_000)
            .map_err(|e| TauriError::NodeError(e.to_string()))?
    };

    if let Err(e) = broadcast_btc_tx(&handle.rpc_state, &raw).await {
        if let Some(wallet) = handle.rpc_state.btc_wallet.write().await.as_mut() {
            wallet.restore_utxos(&spent_utxos);
        }
        return Err(TauriError::NodeError(e));
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

    let wif_private_key = zeroize::Zeroizing::new(wif_private_key);
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

    let key = PrivateKey::from_wif(wif_private_key.as_str())
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

        // Build outputs first: the v2 signature commits to them, so a valid
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

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::Manager;

    #[tokio::test]
    async fn fund_cold_stake_builds_a_p2cs_output() {
        use crate::state::NodeHandle;
        use std::sync::Arc;
        use vtorrent_node::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};
        use vtorrent_node::chain::Chain;
        use vtorrent_node::consensus::{compute_stake_modifier, COIN};
        use vtorrent_node::genesis::GENESIS_BITS;
        use vtorrent_node::mempool::Mempool;
        use vtorrent_rpc::state::AppState as RpcAppState;

        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![create_cold_stake, fund_cold_stake])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |cmd: &str, body: serde_json::Value| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(any(windows, target_os = "android")) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
        };

        let wallet = Wallet::create("pw").unwrap();
        let address = wallet.default_address().unwrap().to_string();
        *state.wallet.lock().unwrap() = Some(wallet);

        let mut chain = Chain::new_regtest().unwrap();
        let genesis = chain.best_hash().unwrap();
        let script = vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&address).unwrap();
        let mut funding = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: genesis,
                merkle_root: [0; 32],
                utxo_root: [0; 32],
                timestamp: 1_700_000_001,
                bits: GENESIS_BITS,
                nonce: 1,
                stake_modifier: compute_stake_modifier(0, &genesis),
            },
            transactions: vec![Transaction {
                version: 1,
                tx_type: TxType::Coinbase,
                inputs: vec![TxInput {
                    prev_txid: [0; 32],
                    prev_vout: u32::MAX,
                    script_sig: vec![1],
                    sequence: u32::MAX,
                }],
                outputs: vec![TxOutput {
                    value: 10 * COIN,
                    script_pubkey: script,
                }],
                lock_time: 1,
                claim_address: None,
                claim_signature: None,
            }],
        };
        funding.header.merkle_root = funding.compute_merkle_root();
        chain.add_block(funding.clone()).unwrap();

        let chain = Arc::new(tokio::sync::Mutex::new(chain));
        let mempool = Arc::new(tokio::sync::Mutex::new(Mempool::new(10_000)));
        let mut rpc = RpcAppState::new_with_shared(chain, mempool);
        let (sender, mut submitted) = tokio::sync::mpsc::channel(1);
        rpc.tx_submit = Some(sender);
        *state.node.lock().await = Some(NodeHandle {
            rpc_state: rpc,
            network: "vtorrent-regtest".to_string(),
            start_time: std::time::Instant::now(),
            mempool_path: None,
        });

        let registry_dir =
            std::env::temp_dir().join(format!("vtr-tauri-cold-stake-{}", std::process::id()));
        std::fs::create_dir_all(&registry_dir).unwrap();
        let wallet_path = registry_dir.join("wallet.vtr");
        *state.wallet_path.lock().unwrap() = Some(wallet_path.clone());

        let foreign = vtorrent_wallet::wallet::generate_cold_stake_keys(0, None).unwrap();
        let error = fund_cold_stake(state.clone(), hex::encode(foreign.script_pubkey), COIN)
            .await
            .unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        assert!(submitted.try_recv().is_err());

        let keys = invoke("create_cold_stake", serde_json::json!({ "locktime": 0 }))
            .expect("create command accepts omitted optional label")
            .deserialize::<serde_json::Value>()
            .unwrap();
        let script_hash = keys["script_hash"].as_str().unwrap();
        assert!(vtorrent_wallet::cold_stake_registry::contains(
            &wallet_path,
            script_hash
        ));
        state.cold_stake_scripts.lock().unwrap().clear();
        let script_hex = keys["script_pubkey"].as_str().unwrap();
        let txid = invoke(
            "fund_cold_stake",
            serde_json::json!({ "scriptPubkey": script_hex, "amountSatoshis": COIN }),
        )
        .expect("fund command accepts frontend camelCase arguments")
        .deserialize::<String>()
        .unwrap();

        let node = state.node.lock().await;
        let handle = node.as_ref().unwrap();
        let mempool = handle.rpc_state.mempool.lock().await;
        let txid_bytes: [u8; 32] = hex::decode(&txid).unwrap().try_into().unwrap();
        let tx = mempool.get_transaction(&txid_bytes).unwrap().clone();
        let relayed = submitted
            .try_recv()
            .expect("funding submitted for peer relay");
        assert_eq!(relayed.txid(), txid_bytes);
        let script = hex::decode(script_hex).unwrap();
        assert!(tx
            .outputs
            .iter()
            .any(|out| out.value == COIN && out.script_pubkey == script));
        drop(mempool);

        let mut chain = handle.rpc_state.chain.lock().await;
        let previous = chain.best_hash().unwrap();
        let modifier = chain.get_block(&previous).unwrap().header.stake_modifier;
        let mut confirmation = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: previous,
                merkle_root: [0; 32],
                utxo_root: [0; 32],
                timestamp: 1_700_000_002,
                bits: GENESIS_BITS,
                nonce: 2,
                stake_modifier: compute_stake_modifier(modifier, &previous),
            },
            transactions: vec![
                Transaction {
                    version: 1,
                    tx_type: TxType::Coinbase,
                    inputs: vec![TxInput {
                        prev_txid: [0; 32],
                        prev_vout: u32::MAX,
                        script_sig: vec![1],
                        sequence: u32::MAX,
                    }],
                    outputs: vec![TxOutput {
                        value: 50 * COIN,
                        script_pubkey: script.clone(),
                    }],
                    lock_time: 2,
                    claim_address: None,
                    claim_signature: None,
                },
                tx,
            ],
        };
        confirmation.header.merkle_root = confirmation.compute_merkle_root();
        chain.add_block(confirmation).unwrap();
        let confirmed = chain
            .get_utxo(&txid_bytes, 0)
            .expect("confirmed funding output");
        assert_eq!(confirmed.height, 2);
        assert_eq!(confirmed.value, COIN);
        assert_eq!(confirmed.script_pubkey, script);
        let _ = std::fs::remove_dir_all(&registry_dir);
    }

    #[tokio::test]
    async fn create_cold_stake_records_the_script() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!("vtr-tauri-cold-stake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        *state.wallet_path.lock().unwrap() = Some(dir.join("wallet.vtr"));
        *state.wallet.lock().unwrap() = Some(Wallet::create("pw").unwrap());
        let keys = create_cold_stake(state.clone(), 0, None).await.unwrap();
        assert_eq!(keys.script_pubkey.len(), 120);
        assert_eq!(keys.script_hash.len(), 40);
        let script = hex::decode(&keys.script_pubkey).unwrap();
        assert!(state.cold_stake_scripts.lock().unwrap().contains(&script));
        assert!(state
            .wallet
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .has_cold_stake_script(&script));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn broadcast_btc_tx_requires_a_peer_for_non_mainnet() {
        let rpc = vtorrent_rpc::state::AppState::new();
        *rpc.btc_network.write().await = bitcoin::Network::Regtest;
        let error = broadcast_btc_tx(&rpc, &[]).await.unwrap_err();
        assert!(error.contains("peer required"), "{error}");
    }

    #[tokio::test]
    async fn quote_vtr_reports_fee_and_change_without_broadcasting() {
        use crate::state::NodeHandle;
        use std::sync::Arc;
        use vtorrent_node::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};
        use vtorrent_node::chain::Chain;
        use vtorrent_node::consensus::{compute_stake_modifier, COIN};
        use vtorrent_node::genesis::GENESIS_BITS;
        use vtorrent_node::mempool::Mempool;
        use vtorrent_rpc::state::AppState as RpcAppState;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();

        let wallet = Wallet::create("pw").unwrap();
        let address = wallet.default_address().unwrap().to_string();
        *state.wallet.lock().unwrap() = Some(wallet);

        let mut chain = Chain::new_regtest().unwrap();
        let genesis = chain.best_hash().unwrap();
        let script = vtorrent_wallet::tx_builder::p2pkh_script_pubkey(&address).unwrap();
        let mut funding = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: genesis,
                merkle_root: [0; 32],
                utxo_root: [0; 32],
                timestamp: 1_700_000_001,
                bits: GENESIS_BITS,
                nonce: 1,
                stake_modifier: compute_stake_modifier(0, &genesis),
            },
            transactions: vec![Transaction {
                version: 1,
                tx_type: TxType::Coinbase,
                inputs: vec![TxInput {
                    prev_txid: [0; 32],
                    prev_vout: u32::MAX,
                    script_sig: vec![1],
                    sequence: u32::MAX,
                }],
                outputs: vec![TxOutput {
                    value: 10 * COIN,
                    script_pubkey: script,
                }],
                lock_time: 1,
                claim_address: None,
                claim_signature: None,
            }],
        };
        funding.header.merkle_root = funding.compute_merkle_root();
        chain.add_block(funding).unwrap();

        let chain = Arc::new(tokio::sync::Mutex::new(chain));
        let mempool = Arc::new(tokio::sync::Mutex::new(Mempool::new(10_000)));
        let mut rpc = RpcAppState::new_with_shared(chain, mempool);
        let (sender, mut submitted) = tokio::sync::mpsc::channel(1);
        rpc.tx_submit = Some(sender);
        *state.node.lock().await = Some(NodeHandle {
            rpc_state: rpc,
            network: "vtorrent-regtest".to_string(),
            start_time: std::time::Instant::now(),
            mempool_path: None,
        });

        let recipient = Wallet::create("other")
            .unwrap()
            .default_address()
            .unwrap()
            .to_string();
        let quote = quote_vtr(state.clone(), recipient.clone(), COIN)
            .await
            .unwrap();
        assert_eq!(quote.from_address, address);
        assert_eq!(quote.to_address, recipient);
        assert_eq!(quote.amount_satoshis, COIN);
        assert!(quote.fee_satoshis > 0);
        assert_eq!(quote.total_debit_satoshis, COIN + quote.fee_satoshis);
        assert!(quote.change_satoshis > 0);
        assert_eq!(quote.input_count, 1);
        assert_eq!(quote.network, "vtorrent-regtest");
        assert!(
            submitted.try_recv().is_err(),
            "a quote must not admit or broadcast the transaction"
        );
        assert!(!quote.quote_id.is_empty());

        let missing = send_vtr(
            state.clone(),
            recipient.clone(),
            COIN,
            Some("missing-quote".into()),
        )
        .await
        .unwrap_err();
        assert!(matches!(missing, TauriError::InvalidInput(_)));

        let txid = send_vtr(state.clone(), recipient.clone(), COIN, Some(quote.quote_id))
            .await
            .unwrap();
        assert_eq!(txid.len(), 64);
        assert!(
            submitted.try_recv().is_ok(),
            "the reviewed transaction must be relayed"
        );
    }

    #[test]
    fn staged_2fa_enrollment_persists_only_after_confirmation() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!("vtr-tauri-2fa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        *state.wallet.lock().unwrap() = Some(Wallet::create("password").unwrap());
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let result = enable_2fa(state.clone()).unwrap();
        assert!(!state.wallet.lock().unwrap().as_ref().unwrap().has_2fa());
        assert!(
            !path.exists(),
            "setup must not persist an active 2FA config"
        );

        let secret = vtorrent_wallet::otp::TotpSecret::from_base32(&result.secret).unwrap();
        let code = secret.current_code().unwrap();
        assert!(verify_2fa(state.clone(), code.clone()).unwrap());
        assert!(state.wallet.lock().unwrap().as_ref().unwrap().has_2fa());
        assert!(path.exists(), "confirmation must persist the active config");

        let loaded = Wallet::load(&path, "password", Some(&code)).unwrap();
        assert!(loaded.has_2fa());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn create_cold_stake_requires_an_active_wallet() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let error = create_cold_stake(state.clone(), 0, None).await.unwrap_err();
        assert!(matches!(error, TauriError::WalletNotInitialized));
    }

    #[tokio::test]
    async fn lock_wallet_clears_session_cold_stake_scripts() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        state.cold_stake_scripts.lock().unwrap().insert(vec![0x51]);
        lock_wallet(state.clone()).await.unwrap();
        assert!(state.cold_stake_scripts.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_wallet_rejects_a_symlink_outside_app_data() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let root =
            std::env::temp_dir().join(format!("vtr-tauri-symlink-root-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!(
            "vtr-tauri-symlink-outside-{}.vtr",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, b"not a wallet").unwrap();
        let link = root.join("wallet.vtr");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        state.set_app_data_dir(root.clone());
        let error = open_wallet(
            state.clone(),
            link.to_string_lossy().to_string(),
            "password".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&outside);
    }

    #[test]
    fn create_wallet_validates_before_creating_parents() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let root =
            std::env::temp_dir().join(format!("vtr-tauri-confine-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        state.set_app_data_dir(root.clone());
        let outside =
            std::env::temp_dir().join(format!("vtr-tauri-confine-outside-{}", std::process::id()));
        let path = outside.join("nested").join("wallet.vtr");
        let error = create_wallet(
            state.clone(),
            "password".into(),
            path.to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        assert!(!outside.exists(), "rejected path must not create parents");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_wallet_refuses_while_the_node_is_running() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir =
            std::env::temp_dir().join(format!("vtr-tauri-node-running-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        state.set_app_data_dir(dir.clone());
        tauri::async_runtime::block_on(async {
            *state.node.lock().await = Some(crate::state::NodeHandle {
                rpc_state: vtorrent_rpc::state::AppState::new(),
                network: "vtorrent-regtest".to_string(),
                start_time: std::time::Instant::now(),
                mempool_path: None,
            });
        });
        let error = create_wallet(
            state.clone(),
            "password".into(),
            dir.join("wallet.vtr").to_string_lossy().to_string(),
        )
        .unwrap_err();
        assert!(matches!(error, TauriError::NodeError(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_wallet_refuses_to_overwrite_an_existing_wallet() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir =
            std::env::temp_dir().join(format!("vtr-tauri-create-wallet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        state.set_app_data_dir(dir.clone());
        let path_string = path.to_string_lossy().to_string();
        create_wallet(state.clone(), "password".into(), path_string.clone()).unwrap();
        let error = create_wallet(state.clone(), "password".into(), path_string).unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
