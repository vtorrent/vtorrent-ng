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

pub(crate) fn ensure_node_stopped(
    state: &AppState,
) -> Result<tokio::sync::MutexGuard<'_, Option<crate::state::NodeHandle>>> {
    match state.node.try_lock() {
        Ok(guard) if guard.is_some() => Err(TauriError::NodeError(
            "Stop the node before switching wallets".into(),
        )),
        Ok(guard) => Ok(guard),
        Err(_) => Err(TauriError::NodeError(
            "Node is busy; stop it before switching wallets".into(),
        )),
    }
}

/// Verify the desktop wallet's passphrase and, when enrolled, a fresh TOTP code.
///
/// The desktop wallet is a seed-based `WalletData` file, which the RPC
/// `verify_wallet_auth` helper cannot parse (it expects an imported hot-wallet
/// payload). Desktop commands must authorize against the wallet they actually
/// hold and pass the resulting key explicitly.
pub(crate) fn authorize_wallet(
    state: &AppState,
    passphrase: &str,
    otp_code: Option<&str>,
) -> Result<zeroize::Zeroizing<String>> {
    let wallet_path = state
        .wallet_path
        .lock()
        .map_err(|_| TauriError::WalletLocked)?
        .clone();
    let mut guard = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
    verify_wallet_credentials(wallet, wallet_path.as_deref(), passphrase, otp_code)?;
    let wif = wallet
        .get_default_wif()
        .ok_or(TauriError::WalletNotInitialized)?;
    Ok(zeroize::Zeroizing::new(wif.to_string()))
}

fn verify_wallet_credentials(
    wallet: &mut Wallet,
    wallet_path: Option<&std::path::Path>,
    passphrase: &str,
    otp_code: Option<&str>,
) -> Result<()> {
    if !wallet.verify_passphrase(passphrase) {
        return Err(TauriError::Unauthorized(
            "Incorrect wallet passphrase".into(),
        ));
    }
    if wallet.has_2fa() {
        let code = otp_code.ok_or(TauriError::TwoFAFailed)?;
        let path = wallet_path.ok_or(TauriError::WalletLocked)?;
        let valid = wallet
            .verify_2fa_once(code, path)
            .map_err(|error| TauriError::Unauthorized(error.to_string()))?;
        if !valid {
            return Err(TauriError::TwoFAFailed);
        }
    }
    Ok(())
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
    pub default_address: String,
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
    /// True while `balance_satoshis` is withheld pending a rescan or blocked by
    /// the history guard, so the UI never renders a hidden balance as zero.
    pub balance_hidden: bool,
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
    let _node_guard = ensure_node_stopped(state.inner())?;
    let _selection = state.lock_wallet_selection()?;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    let passphrase = zeroize::Zeroizing::new(passphrase);
    if let Err(error) = vtorrent_wallet::wallet::validate_new_passphrase(&passphrase) {
        return Err(TauriError::InvalidInput(error.to_string()));
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

    {
        use zeroize::Zeroizing;
        let seed = Zeroizing::new(
            crate::commands::node::wallet_btc_seed(&wallet)
                .ok_or_else(|| TauriError::Wallet("Created wallet has no BTC seed".into()))?,
        );
        crate::commands::node::create_initial_btc_states(&path, &seed)?;
    }
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
    let path = std::path::PathBuf::from(&wallet_path);
    ensure_wallet_path_allowed(state.inner(), &path)?;

    // Locking clears the in-memory wallet but leaves the node running. Allow
    // reopening the same wallet while the node runs; switching to a different
    // wallet file still requires a stopped node.
    let node_guard = state.node.lock().await;
    if node_guard.is_some() {
        let current = state.wallet_path.lock().unwrap().clone();
        if current.as_deref() != Some(path.as_path()) {
            return Err(TauriError::NodeError(
                "Stop the node before switching wallets".into(),
            ));
        }
    }
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    let selection = state.lock_wallet_selection()?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);

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
    drop(selection);

    if let Some(handle) = node_guard.as_ref() {
        handle.rpc_state.lock_wallet().await;
        if let Some(control) = &handle.rpc_state.staking_control {
            let _ = control
                .send(vtorrent_node::staking::StakingCommand::Stop)
                .await;
        }
        state.sync_swap_wallet(&handle.rpc_state).await?;
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
    let node = state.node.lock().await;
    let selection = state.lock_wallet_selection()?;
    *state.wallet.lock().unwrap() = None;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    drop(selection);
    if let Some(handle) = node.as_ref() {
        handle.rpc_state.lock_wallet().await;
        if let Some(control) = &handle.rpc_state.staking_control {
            let _ = control
                .send(vtorrent_node::staking::StakingCommand::Stop)
                .await;
        }
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
    const MAX_LEGACY_WALLET_BASE64: usize = 32 * 1024 * 1024;
    if wallet_dat_base64.len() > MAX_LEGACY_WALLET_BASE64 {
        return Err(TauriError::InvalidInput(
            "wallet.dat payload is too large".into(),
        ));
    }
    let _node_guard = ensure_node_stopped(state.inner())?;
    let _selection = state.lock_wallet_selection()?;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();
    let wallet_dat_base64 = zeroize::Zeroizing::new(wallet_dat_base64);
    let passphrase = passphrase.map(zeroize::Zeroizing::new);
    let new_wallet_passphrase = zeroize::Zeroizing::new(new_wallet_passphrase);
    let wallet_bytes = zeroize::Zeroizing::new(
        B64.decode(&wallet_dat_base64)
            .map_err(|e| TauriError::InvalidInput(format!("Invalid base64: {}", e)))?,
    );

    if let Err(error) = vtorrent_wallet::wallet::validate_new_passphrase(&new_wallet_passphrase) {
        return Err(TauriError::InvalidInput(error.to_string()));
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
            .map_err(TauriError::from)?
            .to_string();
        if !addresses.contains(&addr) {
            addresses.push(addr);
        }
    }

    // The UI, `authorize_wallet`, and the swap identity must all use the same
    // key. Bind the first imported legacy address instead of leaving the
    // generated, unfunded primary address as the default.
    let default_address = addresses
        .first()
        .cloned()
        .ok_or_else(|| TauriError::Migration("No keys found in wallet.dat".into()))?;
    new_wallet
        .set_default_address(&default_address)
        .map_err(TauriError::from)?;

    {
        use zeroize::Zeroizing;
        let seed = Zeroizing::new(
            crate::commands::node::wallet_btc_seed(&new_wallet)
                .ok_or_else(|| TauriError::Wallet("Imported wallet has no BTC seed".into()))?,
        );
        crate::commands::node::create_initial_btc_states(&path, &seed)?;
    }
    new_wallet.save(&path).map_err(TauriError::from)?;

    let claimable_balance = lookup_snapshot_balances(&addresses);

    *state.wallet.lock().unwrap() = Some(new_wallet);
    *state.wallet_path.lock().unwrap() = Some(path);

    Ok(ImportResult {
        keys_found: extraction.keys.len(),
        addresses,
        default_address,
        had_encryption: extraction.was_encrypted,
        had_2fa: extraction.had_2fa,
        claimable_balance,
    })
}

// ─── BTC state recovery ──────────────────────────────────────────────────────

/// Create the attested BTC state for a wallet imported before pass 13.
///
/// The operator must explicitly attest that the wallet has no prior BTC
/// history; this is a one-time recovery for a missing state file, not a
/// silent migration.
#[tauri::command]
pub fn repair_btc_state(
    state: State<AppState>,
    passphrase: String,
    otp_code: Option<String>,
    attest_no_prior_btc_history: bool,
) -> Result<String> {
    if !attest_no_prior_btc_history {
        return Err(TauriError::InvalidInput(
            "BTC state repair requires an explicit no-prior-history attestation".into(),
        ));
    }
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    // Wallet selection is fixed from this authenticated snapshot until the
    // repair finishes, so a concurrent switch cannot redirect its writes.
    let _selection = state.lock_wallet_selection()?;
    let (paths, wallet_id) = authorize_btc_repair(
        state.inner(),
        &passphrase,
        otp_code.as_deref().map(|c| c.as_str()),
    )?;
    #[cfg(test)]
    {
        let hook = state.repair_authorized_hook.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
    paths
        .run(&wallet_id)
        .map_err(|error| btc_repair_error(error, &paths))?;
    Ok("BTC state is ready for mainnet and regtest; restart the node".into())
}

fn authorize_btc_repair(
    state: &AppState,
    passphrase: &str,
    otp_code: Option<&str>,
) -> Result<(vtorrent_btc::repair::RepairPaths, [u8; 32])> {
    let wallet_path = state
        .wallet_path
        .lock()
        .map_err(|_| TauriError::WalletLocked)?
        .clone()
        .ok_or(TauriError::WalletLocked)?;
    let mut guard = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
    verify_wallet_credentials(wallet, Some(&wallet_path), passphrase, otp_code)?;
    let seed = zeroize::Zeroizing::new(
        crate::commands::node::wallet_btc_seed(wallet)
            .ok_or_else(|| TauriError::Wallet("Wallet has no BTC identity key".into()))?,
    );
    let mainnet =
        crate::commands::node::btc_utxo_path(Some(&wallet_path), bitcoin::Network::Bitcoin, &seed);
    let regtest =
        crate::commands::node::btc_utxo_path(Some(&wallet_path), bitcoin::Network::Regtest, &seed);
    let paths = vtorrent_btc::repair::RepairPaths::new(mainnet, regtest).map_err(|error| {
        TauriError::Wallet(format!(
            "BTC repair cannot be bound to the selected wallet: {error}"
        ))
    })?;
    Ok((paths, crate::commands::node::btc_wallet_id(&seed)))
}

fn btc_repair_error(
    error: std::io::Error,
    paths: &vtorrent_btc::repair::RepairPaths,
) -> TauriError {
    if vtorrent_btc::repair::requires_assisted_recovery(&error) {
        TauriError::BtcAssistedRecovery(format!(
            "BTC state conflicts with this repair; preserve all files and use assisted recovery: {error}"
        ))
    } else if vtorrent_btc::repair::is_writer_contention(&error) {
        match paths.ensure_idle() {
            Err(pending) if vtorrent_btc::repair::is_repair_pending(&pending) => {
                TauriError::BtcRepairPending(format!(
                    "Another process holds this wallet's state lock and a BTC repair is pending; close other app instances or quit them, then retry the authenticated repair: {error}"
                ))
            }
            _ => TauriError::Wallet(format!(
                "BTC state is in use by another process; close other app instances or quit them, then retry the repair: {error}"
            )),
        }
    } else {
        TauriError::Io(format!("BTC repair failed: {error}"))
    }
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
pub fn enable_2fa(state: State<AppState>, passphrase: String) -> Result<Enable2FAResult> {
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
    if !wallet.verify_passphrase(&passphrase) {
        return Err(TauriError::Unauthorized(
            "Incorrect wallet passphrase".into(),
        ));
    }

    let config = wallet.enable_2fa().map_err(TauriError::from)?;

    Ok(Enable2FAResult {
        uri: config.to_uri("vTorrent-Wallet"),
        secret: config.secret_base32(),
        qr_data: config.to_uri("vTorrent-Wallet"),
    })
}

#[tauri::command]
pub fn verify_2fa(state: State<AppState>, code: String, passphrase: String) -> Result<bool> {
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
    if !wallet.verify_passphrase(&passphrase) {
        return Err(TauriError::Unauthorized(
            "Incorrect wallet passphrase".into(),
        ));
    }

    if wallet.has_pending_2fa() {
        let pending = wallet.pending_2fa_config();
        let step = pending
            .as_ref()
            .ok_or(TauriError::TwoFAFailed)?
            .verify_step(&code)
            .map_err(TauriError::from)?;
        let Some(step) = step else { return Ok(false) };
        let path = state
            .wallet_path
            .lock()
            .unwrap()
            .clone()
            .ok_or(TauriError::WalletNotInitialized)?;
        let _replay_guard = pending
            .as_ref()
            .ok_or(TauriError::TwoFAFailed)?
            .consume_step(&path, step, None)
            .map_err(TauriError::from)?;
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

    let path = state
        .wallet_path
        .lock()
        .unwrap()
        .clone()
        .ok_or(TauriError::WalletNotInitialized)?;
    let valid = wallet
        .verify_2fa_once(&code, &path)
        .map_err(TauriError::from)?;
    Ok(valid)
}

#[tauri::command]
pub fn disable_2fa(state: State<AppState>, code: String) -> Result<()> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let path = state
        .wallet_path
        .lock()
        .unwrap()
        .clone()
        .ok_or(TauriError::WalletNotInitialized)?;
    if !wallet
        .verify_2fa_once(&code, &path)
        .map_err(TauriError::from)?
    {
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
    let (wifs, addresses, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let from = wallet
            .default_address()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        let addresses: Vec<String> = wallet.addresses().iter().map(|a| a.to_string()).collect();
        let wifs: Vec<zeroize::Zeroizing<String>> = wallet
            .all_wifs()
            .into_iter()
            .map(|w| zeroize::Zeroizing::new(w.to_string()))
            .collect();
        (wifs, addresses, from)
    };

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    // The chain does not reflect mempool spends; without this filter a second
    // send before the first confirms re-selects the same outpoint and is
    // rejected as a conflict.
    let utxos = {
        let chain = handle.rpc_state.chain.lock().await;
        let mempool = handle.rpc_state.mempool.lock().await;
        let mut utxos = Vec::new();
        for address in &addresses {
            utxos.extend(
                chain
                    .get_utxos_for_address(address)
                    .into_iter()
                    .filter(|utxo| !mempool.is_outpoint_spent(&utxo.txid, utxo.vout)),
            );
        }
        utxos
    };
    let fee_rate = {
        let mempool = handle.rpc_state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let wif_refs: Vec<&str> = wifs.iter().map(|wif| wif.as_str()).collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let staking_address = handle.rpc_state.staking_address.read().await.clone();
    let structural = {
        let chain = handle.rpc_state.chain.lock().await;
        vtorrent_wallet_service::structural_stakeable(&chain, staking_address.as_deref(), now)
    };
    let (tx, _used_stake) =
        vtorrent_wallet_service::build_with_reserve(&utxos, &structural, |candidates| {
            vtorrent_wallet_service::build_payment_with_keys(
                candidates,
                &to_address,
                &from_address,
                amount_satoshis,
                fee_rate,
                &wif_refs,
            )
            .map_err(vtorrent_wallet_service::IncentivePaymentError::from)
        })
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
                from_address: from_address.clone(),
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
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let wallet_path = state.wallet_path.lock().unwrap().clone();
    let (wifs, addresses, from_address) = {
        let mut guard = state.wallet.lock().unwrap();
        let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;
        if !wallet.verify_passphrase(&passphrase) {
            return Err(TauriError::Unauthorized(
                "Incorrect wallet passphrase".into(),
            ));
        }
        if wallet.has_2fa() {
            let code = otp_code.as_deref().ok_or(TauriError::TwoFAFailed)?;
            let path = wallet_path.as_ref().ok_or(TauriError::WalletLocked)?;
            let valid = wallet
                .verify_2fa_once(code, path)
                .map_err(|error| TauriError::Unauthorized(error.to_string()))?;
            if !valid {
                return Err(TauriError::TwoFAFailed);
            }
        }
        let from = wallet
            .default_address()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        let addresses: Vec<String> = wallet.addresses().iter().map(|a| a.to_string()).collect();
        let wifs: Vec<zeroize::Zeroizing<String>> = wallet
            .all_wifs()
            .into_iter()
            .map(|w| zeroize::Zeroizing::new(w.to_string()))
            .collect();
        (wifs, addresses, from)
    };

    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;

    // The chain does not reflect mempool spends; without this filter a second
    // send before the first confirms re-selects the same outpoint and is
    // rejected as a conflict.
    let utxos = {
        let chain = handle.rpc_state.chain.lock().await;
        let mempool = handle.rpc_state.mempool.lock().await;
        let mut utxos = Vec::new();
        for address in &addresses {
            utxos.extend(
                chain
                    .get_utxos_for_address(address)
                    .into_iter()
                    .filter(|utxo| !mempool.is_outpoint_spent(&utxo.txid, utxo.vout)),
            );
        }
        utxos
    };

    let fee_rate = {
        let mempool = handle.rpc_state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let tx = if let Some(quote_id) = quote_id {
        let mut prepared = state.prepared_sends.lock().unwrap();
        prepared
            .retain(|_, entry| entry.created_at.elapsed() < std::time::Duration::from_secs(300));
        let authorized = prepared
            .get(&quote_id)
            .map(|entry| entry.from_address == from_address)
            .ok_or_else(|| {
                TauriError::InvalidInput(
                    "Send quote expired or not found; review the transaction again".into(),
                )
            })?;
        if !authorized {
            return Err(TauriError::Unauthorized(
                "Send quote was prepared by a different wallet".into(),
            ));
        }
        prepared
            .remove(&quote_id)
            .ok_or_else(|| {
                TauriError::InvalidInput(
                    "Send quote expired or not found; review the transaction again".into(),
                )
            })?
            .tx
    } else {
        let wif_refs: Vec<&str> = wifs.iter().map(|wif| wif.as_str()).collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let staking_address = handle.rpc_state.staking_address.read().await.clone();
        let structural = {
            let chain = handle.rpc_state.chain.lock().await;
            vtorrent_wallet_service::structural_stakeable(&chain, staking_address.as_deref(), now)
        };
        vtorrent_wallet_service::build_with_reserve(&utxos, &structural, |candidates| {
            vtorrent_wallet_service::build_payment_with_keys(
                candidates,
                &to_address,
                &from_address,
                amount_satoshis,
                fee_rate,
                &wif_refs,
            )
            .map_err(vtorrent_wallet_service::IncentivePaymentError::from)
        })
        .map_err(|e| {
            let message = format!("Transaction build failed: {}", e);
            crate::activity_journal::record_failed(state.inner(), &message);
            TauriError::NodeError(message)
        })?
        .0
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

    // A quote is signed when it is prepared; staking may start or the staking
    // address may change before the send. Re-check the current reserve against
    // the quote's fixed inputs instead of trusting the state at quote time.
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let staking_address = handle.rpc_state.staking_address.read().await.clone();
        let structural = {
            let chain = handle.rpc_state.chain.lock().await;
            vtorrent_wallet_service::structural_stakeable(&chain, staking_address.as_deref(), now)
        };
        let available: std::collections::HashSet<([u8; 32], u32)> = structural
            .iter()
            .filter(|outpoint| {
                utxos
                    .iter()
                    .any(|utxo| (utxo.txid, utxo.vout) == **outpoint)
            })
            .copied()
            .collect();
        if vtorrent_wallet_service::spends_entire_reserve(&tx, &available) {
            return Err(TauriError::InvalidInput(
                "The stake reserve changed since this quote was prepared; review a new quote"
                    .into(),
            ));
        }
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
    pub spending_key_file: String,
    pub staking_address: String,
    pub spending_address: String,
    pub locktime: u32,
}

/// Generate a P2CS key pair and export the spending key to an encrypted file.
#[tauri::command]
pub async fn create_cold_stake(
    state: State<'_, AppState>,
    locktime: u32,
    label: Option<String>,
    spending_key_passphrase: String,
    export_path: Option<String>,
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
    let wallet_path = state.wallet_path.lock().unwrap().clone();
    let export_path = export_path
        .map(std::path::PathBuf::from)
        .or_else(|| {
            wallet_path
                .as_ref()
                .map(|path| path.with_extension(format!("{}.spending-key", keys.spending_address)))
        })
        .ok_or_else(|| {
            TauriError::InvalidInput(
                "An export path is required when no wallet file path is set".into(),
            )
        })?;
    vtorrent_wallet::cold_stake_export::export_spending_key(
        &export_path,
        keys.spending_wif.as_str(),
        &keys.spending_address,
        &spending_key_passphrase,
    )?;
    // The export is the only copy of the spending key. Verify it decrypts
    // before the wallet and registry advertise the script as fundable.
    let (loaded_wif, loaded_address) = vtorrent_wallet::cold_stake_export::load_spending_key(
        &export_path,
        &spending_key_passphrase,
    )
    .map_err(|error| {
        let _ = std::fs::remove_file(&export_path);
        TauriError::from(error)
    })?;
    if loaded_wif.as_str() != keys.spending_wif.as_str() || loaded_address != keys.spending_address
    {
        let _ = std::fs::remove_file(&export_path);
        return Err(TauriError::InvalidInput(
            "Spending-key export did not round-trip; no keys were recorded".into(),
        ));
    }
    let script_hash = hex::encode(vtorrent_core::crypto::hash160(&keys.script_pubkey));
    if let Some(wallet_path) = wallet_path {
        vtorrent_wallet::cold_stake_registry::add(&wallet_path, &script_hash).map_err(|e| {
            TauriError::Io(format!("Could not persist cold-stake script hash: {e}"))
        })?;
    }
    wallet.record_cold_stake(&keys);
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
        spending_key_file: export_path.display().to_string(),
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
    passphrase: String,
    otp_code: Option<String>,
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
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let wallet_path = state.wallet_path.lock().unwrap().clone();
    {
        let mut wallet_guard = state.wallet.lock().unwrap();
        let wallet = wallet_guard
            .as_mut()
            .ok_or(TauriError::WalletNotInitialized)?;
        if !wallet.verify_passphrase(&passphrase) {
            return Err(TauriError::Unauthorized(
                "Incorrect wallet passphrase".into(),
            ));
        }
        if wallet.has_2fa() {
            let code = otp_code.as_deref().ok_or(TauriError::TwoFAFailed)?;
            let path = wallet_path.as_ref().ok_or(TauriError::WalletLocked)?;
            if !wallet
                .verify_2fa_once(code, path)
                .map_err(|error| TauriError::Unauthorized(error.to_string()))?
            {
                return Err(TauriError::TwoFAFailed);
            }
        }
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
    let (wifs, addresses, from_address) = {
        let guard = state.wallet.lock().unwrap();
        let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;
        let from = wallet
            .default_address()
            .ok_or(TauriError::WalletNotInitialized)?
            .to_string();
        let addresses: Vec<String> = wallet.addresses().iter().map(|a| a.to_string()).collect();
        let wifs: Vec<zeroize::Zeroizing<String>> = wallet
            .all_wifs()
            .into_iter()
            .map(|w| zeroize::Zeroizing::new(w.to_string()))
            .collect();
        (wifs, addresses, from)
    };

    // The chain does not reflect mempool spends; without this filter a second
    // send before the first confirms re-selects the same outpoint and is
    // rejected as a conflict.
    let utxos = {
        let chain = handle.rpc_state.chain.lock().await;
        let mempool = handle.rpc_state.mempool.lock().await;
        let mut utxos = Vec::new();
        for address in &addresses {
            utxos.extend(
                chain
                    .get_utxos_for_address(address)
                    .into_iter()
                    .filter(|utxo| !mempool.is_outpoint_spent(&utxo.txid, utxo.vout)),
            );
        }
        utxos
    };
    let fee_rate = {
        let mempool = handle.rpc_state.mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };
    let wif_refs: Vec<&str> = wifs.iter().map(|wif| wif.as_str()).collect();
    // Explicit restake: the user chose this amount for cold staking, so the
    // hot wallet's structural stake coin may be spent. Ordinary sends and
    // automatic incentive payments keep the reserve.
    let tx = vtorrent_wallet_service::build_payment_to_script_with_keys(
        &utxos,
        script,
        &from_address,
        amount_satoshis,
        fee_rate,
        &wif_refs,
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
            balance_hidden: false,
        }),
        Some(wallet) => Ok(BtcStatus {
            initialized: true,
            balance_satoshis: wallet.balance(),
            address: wallet.current_address().ok(),
            best_height: wallet.best_height(),
            synced: wallet.synced(),
            balance_hidden: wallet.balance_hidden(),
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
    state.sync_swap_wallet(&handle.rpc_state).await?;

    // Spending the node's BTC wallet requires an unlocked wallet, matching the
    // VTR path. The desktop command must enforce this too: it talks to the
    // wallet directly and would otherwise bypass the RPC handler's check.
    authorize_wallet(state.inner(), &passphrase, otp_code.as_deref())?;

    let (txid_hex, raw, _) = {
        let mut btc = handle.rpc_state.btc_wallet.write().await;
        let wallet = btc
            .as_mut()
            .ok_or_else(|| TauriError::NodeError("BTC wallet not initialized".into()))?;
        wallet
            .send_to(&to_address, amount_satoshis, 1_000)
            .map_err(|e| TauriError::NodeError(e.to_string()))?
    };

    if let Err(e) = broadcast_btc_tx(&handle.rpc_state, &raw).await {
        return Err(TauriError::NodeError(format!(
            "BTC broadcast status is uncertain for {txid_hex}: {e}. The signed transaction is retained and its inputs are unavailable; retry with the rebroadcast action."
        )));
    }

    tracing::info!("BTC tx broadcast: {}", txid_hex);
    Ok(txid_hex)
}

#[tauri::command]
pub async fn rebroadcast_btc(
    state: State<'_, AppState>,
    txid: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<String> {
    let guard = state.node.lock().await;
    let handle = guard
        .as_ref()
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))?;
    authorize_wallet(state.inner(), &passphrase, otp_code.as_deref())?;
    let raw = {
        let btc = handle.rpc_state.btc_wallet.read().await;
        let wallet = btc
            .as_ref()
            .ok_or_else(|| TauriError::NodeError("BTC wallet not initialized".into()))?;
        wallet
            .pending_send_raw(&txid)
            .ok_or_else(|| TauriError::InvalidInput(format!("No retained BTC send {txid}")))?
    };
    broadcast_btc_tx(&handle.rpc_state, &raw)
        .await
        .map_err(TauriError::NodeError)?;
    Ok(txid)
}

#[tauri::command]
pub async fn list_btc_pending_sends(
    state: State<'_, AppState>,
) -> Result<Vec<vtorrent_rpc::models::BtcPendingSendResponse>> {
    let guard = state.node.lock().await;
    let Some(handle) = guard.as_ref() else {
        return Ok(Vec::new());
    };
    let btc = handle.rpc_state.btc_wallet.read().await;
    let Some(wallet) = btc.as_ref() else {
        return Ok(Vec::new());
    };
    Ok(wallet
        .pending_sends()
        .into_iter()
        .map(|send| vtorrent_rpc::models::BtcPendingSendResponse {
            txid: send.txid,
            has_raw_tx: send.raw.is_some(),
            inputs: send
                .inputs
                .into_iter()
                .map(|input| vtorrent_rpc::models::BtcPendingInput {
                    txid: input.txid,
                    vout: input.vout,
                    value_satoshis: input.value,
                })
                .collect(),
        })
        .collect())
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

    fn unique_wallet_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vtr-tauri-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn arm_selection_probe(
        state: &AppState,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Receiver<()>) {
        let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        *state.selection_probe.lock().unwrap() = Some(attempt_tx);
        *state.selection_acquired_probe.lock().unwrap() = Some(acquired_tx);
        (attempt_rx, acquired_rx)
    }

    fn wait_for_selection_attempt(
        state: &AppState,
        attempt: &std::sync::mpsc::Receiver<()>,
        acquired: &std::sync::mpsc::Receiver<()>,
    ) {
        attempt
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the command must acquire the wallet selection guard");
        assert!(
            state.try_lock_wallet_selection().is_err(),
            "the repair must still hold the selection guard when the command attempts to enter"
        );
        assert!(
            acquired
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err(),
            "the competing command acquired the selection guard before repair released it"
        );
        *state.selection_probe.lock().unwrap() = None;
        *state.selection_acquired_probe.lock().unwrap() = None;
    }

    struct RepairRelease<'a> {
        barrier: &'a std::sync::Barrier,
        armed: bool,
    }

    impl RepairRelease<'_> {
        fn release(&mut self) {
            if self.armed {
                self.armed = false;
                self.barrier.wait();
            }
        }
    }

    impl Drop for RepairRelease<'_> {
        fn drop(&mut self) {
            if self.armed {
                self.barrier.wait();
            }
        }
    }

    fn spawn_command<'scope, 'env, F, T>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        command: F,
    ) -> (
        std::thread::ScopedJoinHandle<'scope, T>,
        std::sync::mpsc::Receiver<()>,
    )
    where
        F: FnOnce() -> T + Send + 'scope,
        T: Send + 'scope,
    {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = scope.spawn(move || {
            let result = command();
            let _ = done_tx.send(());
            result
        });
        (handle, done_rx)
    }

    fn finish_command<T>(
        handle: std::thread::ScopedJoinHandle<'_, T>,
        done: std::sync::mpsc::Receiver<()>,
    ) -> T {
        done.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the competing command must finish after the repair releases the guard");
        handle.join().unwrap()
    }

    /// Run a repair paused after authorization and invoke `attempt` while its
    /// selection guard is held. `attempt` must release the barrier when done.
    fn with_paused_repair(
        state: &tauri::State<'_, AppState>,
        passphrase: &str,
        attempt: impl FnOnce(&mut RepairRelease<'_>),
    ) -> String {
        use std::sync::{Arc, Barrier};
        let (authorized_tx, authorized_rx) = std::sync::mpsc::channel();
        let release = Arc::new(Barrier::new(2));
        {
            let release = Arc::clone(&release);
            *state.repair_authorized_hook.lock().unwrap() = Some(Box::new(move || {
                let _ = authorized_tx.send(());
                release.wait();
            }));
        }
        std::thread::scope(|scope| {
            let repair = scope.spawn({
                let state = state.clone();
                let passphrase = passphrase.to_owned();
                move || repair_btc_state(state, passphrase, None, true)
            });
            authorized_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the repair must reach the authorization hook");
            let mut release = RepairRelease {
                barrier: release.as_ref(),
                armed: true,
            };
            attempt(&mut release);
            release.release();
            repair.join().unwrap().unwrap()
        })
    }

    fn assert_repair_states(wallet_path: &std::path::Path, seed: &[u8; 64]) {
        for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Regtest] {
            let path = crate::commands::node::btc_utxo_path(Some(wallet_path), network, seed);
            assert!(path.is_file(), "missing {}", path.display());
            assert_eq!(
                std::fs::read(path.with_extension("initialized")).unwrap(),
                b"vtorrent-btc-initialized-v1\n"
            );
        }
    }

    #[test]
    fn repair_authorization_failures_create_no_intent() {
        use vtorrent_wallet::otp::TotpSecret;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-auth");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        let mut wallet = Wallet::create("test-pass").unwrap();
        let config = wallet.enable_2fa().unwrap();
        let secret = TotpSecret::from_base32(&config.secret_base32()).unwrap();
        let code = secret.current_code().unwrap();
        assert!(wallet.confirm_pending_2fa(&code).unwrap());
        wallet.save(&path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let mainnet =
            crate::commands::node::btc_utxo_path(Some(&path), bitcoin::Network::Bitcoin, &seed);
        let regtest =
            crate::commands::node::btc_utxo_path(Some(&path), bitcoin::Network::Regtest, &seed);
        let intent = mainnet.with_extension("repair-pending");
        let assert_untouched = |label: &str| {
            assert!(!intent.exists(), "{label} created a repair intent");
            for state_path in [&mainnet, &regtest] {
                assert!(
                    !state_path.exists(),
                    "{label} created {}",
                    state_path.display()
                );
                assert!(
                    !state_path.with_extension("initialized").exists(),
                    "{label} created an initialization marker"
                );
            }
            assert!(
                !std::fs::read_dir(&dir).unwrap().any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".btc-repair-")),
                "{label} left repair staging"
            );
        };

        let error = repair_btc_state(state.clone(), "wrong-pass".into(), Some(code.clone()), true)
            .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error}");
        assert_untouched("a failed passphrase");

        let error = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap_err();
        assert!(matches!(error, TauriError::TwoFAFailed), "{error}");
        assert_untouched("a missing OTP");

        let error =
            repair_btc_state(state.clone(), "test-pass".into(), Some(code), false).unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)), "{error}");
        assert_untouched("a false attestation");

        // Failed authorization must not rewrite existing states or a pending record.
        crate::commands::node::create_initial_btc_states(&path, &seed).unwrap();
        std::fs::write(&intent, b"pending-intent-bytes").unwrap();
        let snapshot = || {
            let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|entry| entry.unwrap())
                .filter(|entry| entry.path().is_file())
                .map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            entries.sort();
            entries
        };
        let before = snapshot();
        for (passphrase, otp, attest) in [
            ("wrong-pass", Some("000000"), true),
            ("test-pass", None, true),
            ("test-pass", Some("000000"), false),
        ] {
            assert!(repair_btc_state(
                state.clone(),
                passphrase.into(),
                otp.map(str::to_owned),
                attest
            )
            .is_err());
            assert_eq!(
                snapshot(),
                before,
                "failed authorization changed wallet files"
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn authorized_repair_stays_bound_to_the_authenticated_wallet() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-bound");
        std::fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("first.vtr");
        let second_path = dir.join("second.vtr");
        let first = Wallet::create("first-pass").unwrap();
        let second = Wallet::create("second-pass").unwrap();
        first.save(&first_path).unwrap();
        second.save(&second_path).unwrap();
        let first_seed = crate::commands::node::wallet_btc_seed(&first).unwrap();
        let second_seed = crate::commands::node::wallet_btc_seed(&second).unwrap();
        *state.wallet.lock().unwrap() = Some(first);
        *state.wallet_path.lock().unwrap() = Some(first_path.clone());
        state.set_app_data_dir(dir.clone());

        let message = with_paused_repair(&state, "first-pass", |release| {
            let (receiver, acquired) = arm_selection_probe(state.inner());
            std::thread::scope(|scope| {
                let (switch, done) = spawn_command(scope, {
                    let state = state.clone();
                    let second_path = second_path.to_string_lossy().into_owned();
                    move || {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap()
                            .block_on(open_wallet(state, second_path, "second-pass".into(), None))
                    }
                });
                wait_for_selection_attempt(state.inner(), &receiver, &acquired);
                assert_eq!(
                    state.wallet_path.lock().unwrap().as_deref(),
                    Some(first_path.as_path()),
                    "open_wallet changed the selection before repair released it"
                );
                release.release();
                finish_command(switch, done).unwrap();
            });
        });
        assert!(message.contains("restart the node"));
        assert_eq!(
            state.wallet_path.lock().unwrap().as_deref(),
            Some(second_path.as_path())
        );
        assert_repair_states(&first_path, &first_seed);
        for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Regtest] {
            let untouched =
                crate::commands::node::btc_utxo_path(Some(&second_path), network, &second_seed);
            assert!(
                !untouched.exists(),
                "the repair was redirected to {}",
                untouched.display()
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repair_btc_state_creates_missing_state_and_refuses_overwrite() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-btc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        let wallet = Wallet::create("test-pass").unwrap();
        wallet.save(&path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let mainnet =
            crate::commands::node::btc_utxo_path(Some(&path), bitcoin::Network::Bitcoin, &seed);
        let held = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap();
        let error = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap_err();
        assert!(matches!(error, TauriError::Wallet(_)), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("close other app instances or quit"),
            "{error}"
        );
        assert!(
            !mainnet.with_extension("repair-pending").exists(),
            "a refused repair must not publish an intent"
        );
        drop(held);

        let message = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap();
        assert!(message.contains("ready"));
        assert!(!message.contains("created"), "{message}");
        for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Regtest] {
            let state_path = crate::commands::node::btc_utxo_path(Some(&path), network, &seed);
            assert!(state_path.exists(), "missing {}", state_path.display());
            let opened =
                crate::commands::node::open_btc_wallet_for_startup(&path, &seed, network).unwrap();
            drop(opened);
        }
        let error = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap_err();
        assert!(
            matches!(error, TauriError::BtcAssistedRecovery(_)),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("preserve all files and use assisted recovery"),
            "an existing state must require review: {error}"
        );
        assert!(
            repair_btc_state(state.clone(), "test-pass".into(), None, false).is_err(),
            "repair requires an explicit attestation"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repair_success_with_totp_consumes_the_step_and_reports_ready() {
        use totp_rs::{Algorithm, TOTP};
        use vtorrent_wallet::otp::TotpSecret;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-totp-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        let mut wallet = Wallet::create("test-pass").unwrap();
        let config = wallet.enable_2fa().unwrap();
        let secret = TotpSecret::from_base32(&config.secret_base32()).unwrap();
        let enrollment_code = secret.current_code().unwrap();
        assert!(wallet.confirm_pending_2fa(&enrollment_code).unwrap());
        wallet.save(&path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1,
            30,
            secret.as_bytes().to_vec(),
            Some("vTorrent".into()),
            "vTorrent-Wallet".into(),
        )
        .unwrap();
        let repair_code = totp.generate((now / 30 + 1) * 30);

        let message = repair_btc_state(
            state.clone(),
            "test-pass".into(),
            Some(repair_code.clone()),
            true,
        )
        .unwrap();
        assert!(message.contains("ready"), "{message}");
        assert!(!message.contains("created"), "{message}");
        assert_repair_states(&path, &seed);

        let error = repair_btc_state(state.clone(), "test-pass".into(), Some(repair_code), true)
            .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error:?}");
        assert!(error.to_string().contains("already been used"), "{error}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn contended_repair_with_a_pending_record_reports_the_pending_class() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-contended");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        let wallet = Wallet::create("test-pass").unwrap();
        wallet.save(&path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let mainnet =
            crate::commands::node::btc_utxo_path(Some(&path), bitcoin::Network::Bitcoin, &seed);
        let regtest =
            crate::commands::node::btc_utxo_path(Some(&path), bitcoin::Network::Regtest, &seed);
        let intent = mainnet.with_extension("repair-pending");
        std::fs::write(&intent, b"pending").unwrap();
        let main_lock = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap();
        let reg_lock = vtorrent_btc::utxo::lock_state_path(&regtest).unwrap();
        let error = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap_err();
        assert!(
            matches!(error, TauriError::BtcRepairPending(_)),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("process holds this wallet's state lock"),
            "{error}"
        );
        assert!(error.to_string().contains("pending"), "{error}");
        assert!(
            error
                .to_string()
                .contains("close other app instances or quit"),
            "{error}"
        );
        drop(main_lock);
        drop(reg_lock);
        std::fs::remove_file(&intent).unwrap();

        let main_lock = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap();
        let reg_lock = vtorrent_btc::utxo::lock_state_path(&regtest).unwrap();
        let error = repair_btc_state(state.clone(), "test-pass".into(), None, true).unwrap_err();
        assert!(matches!(error, TauriError::Wallet(_)), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("close other app instances or quit"),
            "{error}"
        );
        drop(main_lock);
        drop(reg_lock);
        assert!(!mainnet.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repair_error_classes_use_wrapper_text_and_variants() {
        let dir = unique_wallet_dir("repair-error-classes");
        std::fs::create_dir_all(&dir).unwrap();
        let mainnet = dir.join("btc_utxos-mainnet-0000000000000000.json");
        let regtest = dir.join("btc_utxos-regtest-0000000000000000.json");
        let paths = vtorrent_btc::repair::RepairPaths::new(mainnet.clone(), regtest).unwrap();
        std::fs::write(mainnet.with_extension("repair-pending"), b"pending").unwrap();
        let held = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap();
        let pending_contention = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap_err();
        let writer_contention = vtorrent_btc::utxo::lock_state_path(&mainnet).unwrap_err();
        drop(held);

        let error = btc_repair_error(pending_contention, &paths);
        assert!(
            matches!(error, TauriError::BtcRepairPending(_)),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("process holds this wallet's state lock"),
            "{error}"
        );
        assert!(error.to_string().contains("pending"), "{error}");
        assert!(
            error
                .to_string()
                .contains("close other app instances or quit"),
            "{error}"
        );

        std::fs::remove_file(mainnet.with_extension("repair-pending")).unwrap();
        let error = btc_repair_error(writer_contention, &paths);
        assert!(matches!(error, TauriError::Wallet(_)), "{error:?}");
        assert!(
            error
                .to_string()
                .contains("close other app instances or quit"),
            "{error}"
        );

        // A bare errno/kind is not the lock layer's typed marker; Unix errno
        // 33 is EDOM and must not be reported as contention.
        let error = btc_repair_error(std::io::Error::from_raw_os_error(33), &paths);
        assert!(matches!(error, TauriError::Io(_)), "{error:?}");
        let error = btc_repair_error(std::io::Error::from(std::io::ErrorKind::WouldBlock), &paths);
        assert!(matches!(error, TauriError::Io(_)), "{error:?}");

        std::fs::write(dir.join("btc_utxos.json"), b"legacy").unwrap();
        let assisted = paths.run(&[0u8; 32]).unwrap_err();
        assert!(vtorrent_btc::repair::requires_assisted_recovery(&assisted));
        let error = btc_repair_error(assisted, &paths);
        assert!(
            matches!(error, TauriError::BtcAssistedRecovery(_)),
            "{error:?}"
        );
        assert!(error.to_string().contains("preserve all files"), "{error}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_waits_for_an_authorized_repair() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-startup");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.vtr");
        let wallet = Wallet::create("test-pass").unwrap();
        wallet.save(&path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(path.clone());

        let message = with_paused_repair(&state, "test-pass", |release| {
            let (receiver, acquired) = arm_selection_probe(state.inner());
            std::thread::scope(|scope| {
                let (startup, done) = spawn_command(scope, {
                    let state = state.clone();
                    move || {
                        let mut opened = true;
                        for network in [bitcoin::Network::Bitcoin, bitcoin::Network::Regtest] {
                            opened &= crate::commands::node::load_btc_wallet_for_node(
                                state.inner(),
                                network,
                            )
                            .map(|loaded| loaded.is_some())
                            .unwrap_or(false);
                        }
                        opened
                    }
                });
                wait_for_selection_attempt(state.inner(), &receiver, &acquired);
                release.release();
                assert!(
                    finish_command(startup, done),
                    "startup must open both networks after the repair completes"
                );
            });
        });
        assert!(message.contains("restart the node"));
        assert_repair_states(&path, &seed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_wallet_waits_for_an_authorized_repair() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-create-race");
        std::fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("first.vtr");
        let created_path = dir.join("created.vtr");
        let first = Wallet::create("first-pass").unwrap();
        first.save(&first_path).unwrap();
        let first_seed = crate::commands::node::wallet_btc_seed(&first).unwrap();
        *state.wallet.lock().unwrap() = Some(first);
        *state.wallet_path.lock().unwrap() = Some(first_path.clone());
        state.set_app_data_dir(dir.clone());

        let message = with_paused_repair(&state, "first-pass", |release| {
            let (receiver, acquired) = arm_selection_probe(state.inner());
            std::thread::scope(|scope| {
                let (command, done) = spawn_command(scope, {
                    let state = state.clone();
                    let created_path = created_path.to_string_lossy().into_owned();
                    move || create_wallet(state, "new-pass".into(), created_path)
                });
                wait_for_selection_attempt(state.inner(), &receiver, &acquired);
                assert_eq!(
                    state.wallet_path.lock().unwrap().as_deref(),
                    Some(first_path.as_path()),
                    "create_wallet changed the selection before repair released it"
                );
                release.release();
                finish_command(command, done).unwrap();
            });
        });
        assert!(message.contains("restart the node"));
        assert_eq!(
            state.wallet_path.lock().unwrap().as_deref(),
            Some(created_path.as_path())
        );
        assert!(created_path.exists());
        assert_repair_states(&first_path, &first_seed);
        let created_seed = {
            let guard = state.wallet.lock().unwrap();
            crate::commands::node::wallet_btc_seed(guard.as_ref().unwrap()).unwrap()
        };
        assert_repair_states(&created_path, &created_seed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_legacy_wallet_waits_for_an_authorized_repair() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-import-race");
        std::fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("first.vtr");
        let imported_path = dir.join("imported.vtr");
        let first = Wallet::create("first-pass").unwrap();
        first.save(&first_path).unwrap();
        let first_seed = crate::commands::node::wallet_btc_seed(&first).unwrap();
        *state.wallet.lock().unwrap() = Some(first);
        *state.wallet_path.lock().unwrap() = Some(first_path.clone());
        state.set_app_data_dir(dir.clone());
        let payload = B64.encode(b"not a berkeley wallet");

        let message = with_paused_repair(&state, "first-pass", |release| {
            let (receiver, acquired) = arm_selection_probe(state.inner());
            std::thread::scope(|scope| {
                let (command, done) = spawn_command(scope, {
                    let state = state.clone();
                    let imported_path = imported_path.to_string_lossy().into_owned();
                    move || {
                        import_legacy_wallet(state, payload, None, "new-pass".into(), imported_path)
                    }
                });
                wait_for_selection_attempt(state.inner(), &receiver, &acquired);
                assert_eq!(
                    state.wallet_path.lock().unwrap().as_deref(),
                    Some(first_path.as_path()),
                    "import changed the selection before repair released it"
                );
                release.release();
                let error = finish_command(command, done).unwrap_err();
                assert!(matches!(error, TauriError::Migration(_)), "{error:?}");
            });
        });
        assert!(message.contains("restart the node"));
        assert_eq!(
            state.wallet_path.lock().unwrap().as_deref(),
            Some(first_path.as_path())
        );
        assert!(!imported_path.exists());
        assert_repair_states(&first_path, &first_seed);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restore_backup_waits_for_an_authorized_repair() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("repair-restore-race");
        let source = dir.join("source");
        let target = dir.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let source_path = source.join("wallet.vtr");
        let source_wallet = Wallet::create("source-pass").unwrap();
        source_wallet.save(&source_path).unwrap();
        *state.wallet.lock().unwrap() = Some(source_wallet);
        *state.wallet_path.lock().unwrap() = Some(source_path);
        state.set_app_data_dir(source.clone());
        let backup =
            crate::commands::backup::create_backup(state.clone(), "source-pass".into(), None)
                .unwrap();
        let encoded = B64.encode(std::fs::read(backup.path).unwrap());

        let first_path = dir.join("first.vtr");
        let first = Wallet::create("first-pass").unwrap();
        first.save(&first_path).unwrap();
        let first_seed = crate::commands::node::wallet_btc_seed(&first).unwrap();
        *state.wallet.lock().unwrap() = Some(first);
        *state.wallet_path.lock().unwrap() = Some(first_path.clone());
        state.set_app_data_dir(target.clone());

        let message = with_paused_repair(&state, "first-pass", |release| {
            let (receiver, acquired) = arm_selection_probe(state.inner());
            std::thread::scope(|scope| {
                let (command, done) = spawn_command(scope, {
                    let state = state.clone();
                    move || {
                        crate::commands::backup::restore_backup(
                            state,
                            encoded,
                            "source-pass".into(),
                            None,
                            false,
                        )
                    }
                });
                wait_for_selection_attempt(state.inner(), &receiver, &acquired);
                assert_eq!(
                    state.wallet_path.lock().unwrap().as_deref(),
                    Some(first_path.as_path()),
                    "restore changed the selection before repair released it"
                );
                release.release();
                finish_command(command, done).unwrap();
            });
        });
        assert!(message.contains("restart the node"));
        assert!(target.join("wallet.vtr").exists());
        assert!(state.wallet_path.lock().unwrap().is_none());
        assert_repair_states(&first_path, &first_seed);
        let _ = std::fs::remove_dir_all(dir);
    }

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

        let registry_dir = unique_wallet_dir("cold-stake-fund");
        std::fs::create_dir_all(&registry_dir).unwrap();
        let wallet_path = registry_dir.join("wallet.vtr");
        *state.wallet_path.lock().unwrap() = Some(wallet_path.clone());

        let foreign = vtorrent_wallet::wallet::generate_cold_stake_keys(0, None).unwrap();
        let error = fund_cold_stake(
            state.clone(),
            hex::encode(foreign.script_pubkey),
            COIN,
            "pw".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        assert!(submitted.try_recv().is_err());

        let keys = invoke(
            "create_cold_stake",
            serde_json::json!({
                "locktime": 0,
                "spendingKeyPassphrase": "test-pass",
            }),
        )
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
        assert!(
            invoke(
                "fund_cold_stake",
                serde_json::json!({ "scriptPubkey": script_hex, "amountSatoshis": COIN }),
            )
            .is_err(),
            "cold-stake funding must require a wallet passphrase"
        );
        assert!(invoke(
            "fund_cold_stake",
            serde_json::json!({ "scriptPubkey": script_hex, "amountSatoshis": COIN, "passphrase": "wrong" }),
        ).is_err(), "cold-stake funding must reject an incorrect wallet passphrase");
        assert!(submitted.try_recv().is_err());
        let otp = {
            use vtorrent_wallet::otp::TotpSecret;
            let mut guard = state.wallet.lock().unwrap();
            let wallet = guard.as_mut().unwrap();
            let config = wallet.enable_2fa().unwrap();
            let secret = TotpSecret::from_base32(&config.secret_base32()).unwrap();
            assert!(wallet
                .confirm_pending_2fa(&secret.current_code().unwrap())
                .unwrap());
            let totp = totp_rs::TOTP::new(
                totp_rs::Algorithm::SHA1,
                6,
                1,
                30,
                secret.as_bytes().to_vec(),
                Some("vTorrent".into()),
                "test".into(),
            )
            .unwrap();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            totp.generate(now + 30)
        };
        let missing = fund_cold_stake(state.clone(), script_hex.into(), COIN, "pw".into(), None)
            .await
            .unwrap_err();
        assert!(matches!(missing, TauriError::TwoFAFailed));
        let invalid = fund_cold_stake(
            state.clone(),
            script_hex.into(),
            COIN,
            "pw".into(),
            Some("invalid".into()),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            invalid,
            TauriError::TwoFAFailed | TauriError::Unauthorized(_)
        ));
        assert!(submitted.try_recv().is_err());
        let txid = invoke(
            "fund_cold_stake",
            serde_json::json!({ "scriptPubkey": script_hex, "amountSatoshis": COIN, "passphrase": "pw", "otpCode": otp }),
        )
        .expect("fund command accepts frontend camelCase arguments")
        .deserialize::<String>()
        .unwrap();
        let replay = fund_cold_stake(
            state.clone(),
            script_hex.into(),
            COIN,
            "pw".into(),
            Some(otp),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            replay,
            TauriError::TwoFAFailed | TauriError::Unauthorized(_)
        ));

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
        let dir = unique_wallet_dir("cold-stake-records");
        std::fs::create_dir_all(&dir).unwrap();
        *state.wallet_path.lock().unwrap() = Some(dir.join("wallet.vtr"));
        *state.wallet.lock().unwrap() = Some(Wallet::create("pw").unwrap());
        let keys = create_cold_stake(state.clone(), 0, None, "test-pass".into(), None)
            .await
            .unwrap();
        assert!(std::path::Path::new(&keys.spending_key_file).exists());
        let second = create_cold_stake(state.clone(), 0, None, "test-pass".into(), None)
            .await
            .unwrap();
        assert_ne!(keys.spending_key_file, second.spending_key_file);
        for entry in [&keys, &second] {
            let (_, address) = vtorrent_wallet::cold_stake_export::load_spending_key(
                std::path::Path::new(&entry.spending_key_file),
                "test-pass",
            )
            .unwrap();
            assert_eq!(address, entry.spending_address);
        }
        // Locktime 0 uses the minimal OP_0 encoding, so the script is shorter
        // than the legacy fixed PUSH4 form.
        assert!(keys.script_pubkey.len() < 120);
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
    async fn dex_and_swap_commands_require_desktop_wallet_auth() {
        use crate::state::NodeHandle;
        use vtorrent_rpc::state::AppState as RpcAppState;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = unique_wallet_dir("desktop-swap-auth");
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("pw").unwrap();
        let address = wallet.default_address().unwrap().to_string();
        let wallet_path = dir.join("wallet.vtr");
        wallet.save(&wallet_path).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        *state.node.lock().await = Some(NodeHandle {
            rpc_state: RpcAppState::new(),
            network: "vtorrent-regtest".to_string(),
            start_time: std::time::Instant::now(),
            mempool_path: None,
        });

        let error = crate::commands::dex::match_dex_order(
            state.clone(),
            "00".repeat(32),
            address.clone(),
            "wrong".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error:?}");

        let error = crate::commands::dex::vtr_claim(
            state.clone(),
            "00".repeat(32),
            "00".repeat(32),
            "wrong".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error:?}");

        let error = crate::commands::dex::btc_fund(
            state.clone(),
            "00".repeat(32),
            "bcrt1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh".into(),
            "wrong".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error:?}");

        let foreign = Wallet::create("other")
            .unwrap()
            .default_address()
            .unwrap()
            .to_string();
        let error = crate::commands::dex::place_dex_order(
            state.clone(),
            foreign,
            None,
            100_000_000,
            "BTC".into(),
            100_000_000,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)), "{error:?}");

        // Correct credentials must pass authorization; the missing order is the
        // next failure, proving the guard is not a blanket refusal.
        let error = crate::commands::dex::match_dex_order(
            state.clone(),
            "00".repeat(32),
            address.clone(),
            "pw".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::NotFound(_)), "{error:?}");

        // A 2FA wallet must reject a swap action without a code.
        {
            use vtorrent_wallet::otp::TotpSecret;
            let mut guard = state.wallet.lock().unwrap();
            let wallet = guard.as_mut().unwrap();
            let config = wallet.enable_2fa().unwrap();
            let secret = TotpSecret::from_base32(&config.secret_base32()).unwrap();
            assert!(wallet
                .confirm_pending_2fa(&secret.current_code().unwrap())
                .unwrap());
        }
        let error = crate::commands::dex::match_dex_order(
            state.clone(),
            "00".repeat(32),
            address,
            "pw".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, TauriError::TwoFAFailed), "{error:?}");

        let _ = std::fs::remove_dir_all(&dir);
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
                    script_pubkey: script.clone(),
                }],
                lock_time: 1,
                claim_address: None,
                claim_signature: None,
            }],
        };
        funding.header.merkle_root = funding.compute_merkle_root();
        chain.add_block(funding).unwrap();

        // A second independent funding block. Sends correctly exclude inputs
        // already spent in the mempool, so the 2FA assertion below needs a
        // second spendable coin once the first send is broadcast.
        let second = {
            let prev = chain.best_hash().unwrap();
            let parent = chain
                .get_header_at_height(chain.best_height())
                .expect("parent header");
            let timestamp = parent.timestamp.saturating_add(1);
            let mut block = Block {
                header: BlockHeader {
                    version: 1,
                    prev_block_hash: prev,
                    merkle_root: [0; 32],
                    utxo_root: [0; 32],
                    timestamp,
                    bits: GENESIS_BITS,
                    nonce: 2,
                    stake_modifier: compute_stake_modifier(parent.stake_modifier, &prev),
                },
                transactions: vec![Transaction {
                    version: 1,
                    tx_type: TxType::Coinbase,
                    inputs: vec![TxInput {
                        prev_txid: [0; 32],
                        prev_vout: u32::MAX,
                        script_sig: vec![2],
                        sequence: u32::MAX,
                    }],
                    outputs: vec![TxOutput {
                        value: 10 * COIN,
                        script_pubkey: script.clone(),
                    }],
                    lock_time: 2,
                    claim_address: None,
                    claim_signature: None,
                }],
            };
            block.header.merkle_root = block.compute_merkle_root();
            block
        };
        chain.add_block(second).unwrap();

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
            "pw".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(missing, TauriError::InvalidInput(_)));

        let wrong = send_vtr(
            state.clone(),
            recipient.clone(),
            COIN,
            Some(quote.quote_id.clone()),
            "wrong".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(wrong, TauriError::Unauthorized(_)));

        let original_wallet = state.wallet.lock().unwrap().take().unwrap();
        *state.wallet.lock().unwrap() = Some(Wallet::create("other-pass").unwrap());
        let cross_wallet = send_vtr(
            state.clone(),
            recipient.clone(),
            COIN,
            Some(quote.quote_id.clone()),
            "other-pass".into(),
            None,
        )
        .await;
        *state.wallet.lock().unwrap() = Some(original_wallet);
        let cross_wallet =
            cross_wallet.expect_err("a quote prepared by another wallet must not be broadcast");
        assert!(
            matches!(
                cross_wallet,
                TauriError::InvalidInput(_) | TauriError::Unauthorized(_)
            ),
            "{cross_wallet:?}"
        );
        assert!(
            submitted.try_recv().is_err(),
            "a quote prepared by another wallet must not be relayed"
        );

        let txid = send_vtr(
            state.clone(),
            recipient.clone(),
            COIN,
            Some(quote.quote_id),
            "pw".into(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(txid.len(), 64);
        assert!(
            submitted.try_recv().is_ok(),
            "the reviewed transaction must be relayed"
        );

        // A 2FA wallet must not send without a code, even with the correct
        // passphrase.
        {
            use vtorrent_wallet::otp::TotpSecret;
            let mut guard = state.wallet.lock().unwrap();
            let wallet = guard.as_mut().unwrap();
            let config = wallet.enable_2fa().unwrap();
            let secret = TotpSecret::from_base32(&config.secret_base32()).unwrap();
            let code = secret.current_code().unwrap();
            assert!(wallet.confirm_pending_2fa(&code).unwrap());
        }
        let second_quote = quote_vtr(state.clone(), recipient.clone(), COIN)
            .await
            .unwrap();
        let missing_otp = send_vtr(
            state.clone(),
            recipient.clone(),
            COIN,
            Some(second_quote.quote_id),
            "pw".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(missing_otp, TauriError::TwoFAFailed));
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

        let error = enable_2fa(state.clone(), "wrong".into()).unwrap_err();
        assert!(matches!(error, TauriError::Unauthorized(_)));
        let result = enable_2fa(state.clone(), "password".into()).unwrap();
        assert!(!state.wallet.lock().unwrap().as_ref().unwrap().has_2fa());
        assert!(
            !path.exists(),
            "setup must not persist an active 2FA config"
        );

        let secret = vtorrent_wallet::otp::TotpSecret::from_base32(&result.secret).unwrap();
        let code = secret.current_code().unwrap();
        assert!(verify_2fa(state.clone(), code.clone(), "wrong".into()).is_err());
        assert!(verify_2fa(state.clone(), code.clone(), "password".into()).unwrap());
        assert!(state.wallet.lock().unwrap().as_ref().unwrap().has_2fa());
        assert!(path.exists(), "confirmation must persist the active config");

        assert!(
            verify_2fa(state.clone(), code.clone(), "password".into()).is_err(),
            "enrollment code must not be accepted twice"
        );
        assert!(
            disable_2fa(state.clone(), code.clone()).is_err(),
            "enrollment code must not disable 2FA"
        );
        assert!(
            Wallet::load(&path, "password", Some(&code)).is_err(),
            "enrollment code must not unlock after restart"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn create_cold_stake_requires_an_active_wallet() {
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let error = create_cold_stake(state.clone(), 0, None, "test-pass".into(), None)
            .await
            .unwrap_err();
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
    fn stopped_node_guard_excludes_concurrent_startup() {
        let state = AppState::new();
        let _guard = ensure_node_stopped(&state).unwrap();
        assert!(
            state.node.try_lock().is_err(),
            "wallet lifecycle must retain exclusive node access"
        );
    }

    #[tokio::test]
    async fn lock_wallet_waits_for_node_startup_before_clearing_wallet() {
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        *state.wallet.lock().unwrap() = Some(Wallet::create("password").unwrap());
        let startup_guard = state.node.lock().await;
        let operation = lock_wallet(state.clone());
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut operation)
                .await
                .is_err()
        );
        assert!(state.wallet.lock().unwrap().is_some());
        drop(startup_guard);
        operation.await.unwrap();
        assert!(state.wallet.lock().unwrap().is_none());
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
