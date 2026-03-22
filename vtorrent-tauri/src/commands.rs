/// vTorrent 2.0 Tauri IPC commands.
///
/// Each function decorated with `#[tauri::command]` becomes callable from
/// the React frontend via:
///   `import { invoke } from '@tauri-apps/api/core'`
///   `await invoke('command_name', { arg1, arg2 })`
///
/// All private key material is handled exclusively in Rust.
/// JavaScript only receives addresses, balances, and status flags.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use tauri::State;

use vtorrent_migrate::extractor::extract_wallet;
use vtorrent_wallet::wallet::Wallet;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

// ─── Request / Response types ───────────────────────────────────────────────

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

// ─── Wallet lifecycle commands ───────────────────────────────────────────────

/// Create a new wallet encrypted with the given passphrase.
///
/// Called from: `CreateWalletPage.tsx` → `invoke('create_wallet', { passphrase })`
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
    let wallet = Wallet::create(&passphrase)
        .map_err(TauriError::from)?;

    // Save the encrypted wallet to disk
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

/// Load and unlock an existing wallet.dat file.
///
/// Called from: `WelcomePage.tsx` → `invoke('open_wallet', { walletPath, passphrase, otpCode })`
#[tauri::command]
pub fn open_wallet(
    state: State<AppState>,
    wallet_path: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<WalletInfo> {
    let path = std::path::PathBuf::from(&wallet_path);

    let wallet = Wallet::load(&path, &passphrase)
        .map_err(TauriError::from)?;

    // Verify 2FA if the wallet has it enabled
    if wallet.has_2fa() {
        let code = otp_code.ok_or(TauriError::TwoFAFailed)?;
        if !wallet.verify_2fa(&code).map_err(TauriError::from)? {
            return Err(TauriError::TwoFAFailed);
        }
    }

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

/// Lock the wallet (clear keys from memory).
///
/// Called from: `Layout.tsx` → `invoke('lock_wallet')`
#[tauri::command]
pub fn lock_wallet(state: State<AppState>) -> Result<()> {
    *state.wallet.lock().unwrap() = None;
    Ok(())
}

/// Get the current wallet status without unlocking.
///
/// Called from: `App.tsx` on startup → `invoke('get_wallet_info')`
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

/// Parse and extract keys from a legacy vTorrent wallet.dat file.
///
/// This is the core of the wallet migration feature. The wallet.dat bytes
/// are passed as a base64 string from the frontend (since Tauri IPC uses JSON).
///
/// Called from: `ImportWizardPage.tsx` → `invoke('import_legacy_wallet', { walletDatBase64, passphrase })`
///
/// Security guarantees:
/// - Private keys are extracted in Rust and NEVER sent to JavaScript
/// - Only addresses and metadata are returned to the frontend
/// - The extracted keys are immediately imported into the new wallet
#[tauri::command]
pub fn import_legacy_wallet(
    state: State<AppState>,
    wallet_dat_base64: String,
    passphrase: Option<String>,
    new_wallet_passphrase: String,
    new_wallet_path: String,
) -> Result<ImportResult> {
    // Decode the base64 wallet.dat bytes
    let wallet_bytes = B64
        .decode(&wallet_dat_base64)
        .map_err(|e| TauriError::InvalidInput(format!("Invalid base64: {}", e)))?;

    // Extract keys from the legacy wallet.dat using the vtorrent-migrate crate
    let extraction = extract_wallet(
        &wallet_bytes,
        passphrase.as_deref(),
    ).map_err(TauriError::from)?;

    if extraction.keys.is_empty() {
        return Err(TauriError::Migration("No keys found in wallet.dat".into()));
    }

    // Create a new wallet and import the extracted keys
    let path = std::path::PathBuf::from(&new_wallet_path);
    let mut new_wallet = Wallet::create(&new_wallet_passphrase)
        .map_err(TauriError::from)?;

    // Import each extracted key into the new wallet
    let mut addresses = Vec::new();
    for key in &extraction.keys {
        let addr = new_wallet
            .import_wif(&key.wif, Some(&key.legacy_address))
            .map_err(TauriError::from)?;
        addresses.push(addr.to_string());
    }

    // Save the new wallet to disk
    new_wallet.save(&path).map_err(TauriError::from)?;

    // Look up claimable balances from the snapshot
    let claimable_balance = lookup_snapshot_balances(&addresses);

    // Store the wallet in app state
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

/// The embedded UTXO snapshot binary (2.4 MB, 59,375 entries, sorted for binary search).
///
/// Format: 4-byte magic "VTRS" + 4-byte version + 4-byte count +
///         N * (34-byte null-padded address + 8-byte u64 satoshi balance)
static UTXO_SNAPSHOT: &[u8] = include_bytes!("../utxo_snapshot.bin");

const SNAPSHOT_MAGIC: &[u8; 4] = b"VTRS";
const SNAPSHOT_HEADER_SIZE: usize = 12;
const ADDR_LEN: usize = 34;
const ENTRY_SIZE: usize = ADDR_LEN + 8; // 42 bytes per entry

/// Look up balances in the embedded UTXO snapshot for a list of legacy addresses.
///
/// Uses binary search on the sorted snapshot for O(log n) lookup per address.
fn lookup_snapshot_balances(addresses: &[String]) -> u64 {
    if UTXO_SNAPSHOT.len() < SNAPSHOT_HEADER_SIZE {
        return 0;
    }
    if &UTXO_SNAPSHOT[0..4] != SNAPSHOT_MAGIC {
        return 0;
    }

    let entry_count = u32::from_le_bytes([
        UTXO_SNAPSHOT[8], UTXO_SNAPSHOT[9], UTXO_SNAPSHOT[10], UTXO_SNAPSHOT[11],
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

// ─── Address management commands ─────────────────────────────────────────────

/// Generate a new receiving address in the current wallet.
///
/// Called from: `DashboardPage.tsx` → `invoke('generate_address', { label })`
#[tauri::command]
pub fn generate_address(
    state: State<AppState>,
    label: Option<String>,
) -> Result<AddressInfo> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    let address = wallet
        .generate_key(label.as_deref())
        .map_err(TauriError::from)?;

    Ok(AddressInfo {
        address: address.to_string(),
        label: label.unwrap_or_else(|| "New Address".into()),
        balance: 0,
        is_legacy_import: false,
    })
}

/// Get all addresses in the current wallet.
///
/// Called from: `DashboardPage.tsx` → `invoke('get_addresses')`
#[tauri::command]
pub fn get_addresses(state: State<AppState>) -> Result<Vec<AddressInfo>> {
    let guard = state.wallet.lock().unwrap();
    let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;

    let addresses = wallet
        .list_addresses()
        .iter()
        .map(|(addr, label, balance, is_import)| AddressInfo {
            address: addr.to_string(),
            label: label.clone(),
            balance: *balance,
            is_legacy_import: *is_import,
        })
        .collect();

    Ok(addresses)
}

// ─── 2FA commands ─────────────────────────────────────────────────────────────

/// Enable TOTP 2FA on the current wallet.
///
/// Returns the TOTP URI and base32 secret for QR code display.
/// Called from: `SecurityCenterPage.tsx` → `invoke('enable_2fa')`
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

/// Verify a TOTP code against the wallet's 2FA secret.
///
/// Called from: `SecurityCenterPage.tsx` → `invoke('verify_2fa', { code })`
#[tauri::command]
pub fn verify_2fa(state: State<AppState>, code: String) -> Result<bool> {
    let guard = state.wallet.lock().unwrap();
    let wallet = guard.as_ref().ok_or(TauriError::WalletNotInitialized)?;

    let valid = wallet.verify_2fa(&code).map_err(TauriError::from)?;
    Ok(valid)
}

/// Disable 2FA after verifying the current OTP code.
///
/// Called from: `SecurityCenterPage.tsx` → `invoke('disable_2fa', { code })`
#[tauri::command]
pub fn disable_2fa(state: State<AppState>, code: String) -> Result<()> {
    let mut guard = state.wallet.lock().unwrap();
    let wallet = guard.as_mut().ok_or(TauriError::WalletNotInitialized)?;

    if !wallet.verify_2fa(&code).map_err(TauriError::from)? {
        return Err(TauriError::TwoFAFailed);
    }

    wallet.disable_2fa().map_err(TauriError::from)?;
    Ok(())
}
