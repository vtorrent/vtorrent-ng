//! Encrypted, verified wallet backups.
//!
//! A backup contains the encrypted wallet file plus the recovery state that
//! lives next to it: the cold-stake registry, swap recovery records, and the
//! wallet-scoped BTC UTXO files. The bundle is encrypted with the wallet
//! passphrase and every entry is hashed before restore.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use tauri::State;
use vtorrent_wallet::encryption::{decrypt_wallet, encrypt_wallet, EncryptedWallet};
use vtorrent_wallet::wallet::Wallet;

use crate::error::{Result, TauriError};
use crate::state::AppState;

const BACKUP_FORMAT_VERSION: u32 = 1;
const MAX_BACKUP_BYTES: usize = 16 * 1024 * 1024;
const WALLET_ENTRY: &str = "wallet.vtr";

#[derive(Debug, Serialize, Deserialize)]
struct BackupBundle {
    format_version: u32,
    created_at: u64,
    files: Vec<BackupEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupEntry {
    name: String,
    sha256: String,
    data_base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupFile {
    format_version: u32,
    encrypted: EncryptedWallet,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub path: String,
    pub size: u64,
    pub file_count: usize,
    pub created_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreview {
    pub wallet_address: String,
    pub has_2fa: bool,
    pub file_count: usize,
    pub created_at: u64,
    pub files: Vec<String>,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn safe_relative_path(name: &str) -> Result<PathBuf> {
    if name.is_empty() {
        return Err(TauriError::InvalidInput(
            "Backup entry name is empty".into(),
        ));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(TauriError::InvalidInput(
            "Backup entry must be a relative path".into(),
        ));
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            _ => {
                return Err(TauriError::InvalidInput(format!(
                    "Backup entry contains an unsafe path: {name}"
                )))
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(TauriError::InvalidInput(
            "Backup entry path is empty".into(),
        ));
    }
    Ok(safe)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TauriError::Io(e.to_string()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| TauriError::Io(e.to_string()))?;
    file.write_all(bytes)
        .map_err(|e| TauriError::Io(e.to_string()))?;
    file.sync_all().map_err(|e| TauriError::Io(e.to_string()))?;
    Ok(())
}

fn add_entry(entries: &mut Vec<BackupEntry>, name: &str, path: &Path) -> Result<()> {
    safe_relative_path(name)?;
    let bytes = std::fs::read(path).map_err(|e| TauriError::Io(e.to_string()))?;
    if bytes.len() > MAX_BACKUP_BYTES {
        return Err(TauriError::InvalidInput(format!(
            "Backup entry {name} is too large"
        )));
    }
    entries.push(BackupEntry {
        name: name.to_string(),
        sha256: hex::encode(Sha256::digest(&bytes)),
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(TauriError::Io(error.to_string())),
    }
}

fn collect_entries(wallet_path: &Path, wallet: &Wallet) -> Result<Vec<BackupEntry>> {
    let mut entries = Vec::new();
    let mut btc_locks = Vec::new();
    add_entry(&mut entries, WALLET_ENTRY, wallet_path)?;

    // Cold-stake spending keys are exported once, beside the wallet. A
    // wallet-only restore would leave funded P2CS scripts unspendable until
    // their locktime, so include the exports in the encrypted backup.
    let stem = wallet_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("wallet");
    let prefix = format!("{stem}.");
    let parent = wallet_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent.exists() {
        let mut exports = Vec::new();
        for entry in std::fs::read_dir(parent).map_err(|e| TauriError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| TauriError::Io(e.to_string()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(&prefix) || !name.ends_with(".spending-key") {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|e| TauriError::Io(e.to_string()))?;
            if !file_type.is_file() {
                return Err(TauriError::InvalidInput(format!(
                    "Cold-stake export {name} must be a regular file"
                )));
            }
            exports.push((name.to_string(), entry.path()));
        }
        exports.sort_by(|a, b| a.0.cmp(&b.0));
        if exports.len() > 64 {
            return Err(TauriError::InvalidInput(
                "Too many cold-stake exports to back up".into(),
            ));
        }
        for (name, path) in exports {
            add_entry(&mut entries, &name, &path)?;
        }
    }

    let cold_stake = wallet_path.with_extension("cold_stake.json");
    if cold_stake.exists() {
        add_entry(&mut entries, "wallet.cold_stake.json", &cold_stake)?;
    }
    // New registries are append-only directories. Include every entry so a
    // backup still carries all script hashes if the JSON snapshot is stale.
    let cold_dir = wallet_path.with_extension("cold_stake.d");
    if cold_dir.is_dir() {
        let dir_name = cold_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("wallet.cold_stake.d")
            .to_string();
        let mut hashes = Vec::new();
        for entry in std::fs::read_dir(&cold_dir).map_err(|e| TauriError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| TauriError::Io(e.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| TauriError::Io(e.to_string()))?;
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.len() != 40 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            hashes.push((name, entry.path()));
        }
        hashes.sort_by(|a, b| a.0.cmp(&b.0));
        if hashes.len() > 64 {
            return Err(TauriError::InvalidInput(
                "Too many cold-stake registry entries to back up".into(),
            ));
        }
        for (name, path) in hashes {
            add_entry(&mut entries, &format!("{dir_name}/{name}"), &path)?;
        }
    }

    let swaps = wallet_path.with_extension("swaps");
    if swaps.is_dir() {
        for entry in std::fs::read_dir(&swaps).map_err(|e| TauriError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| TauriError::Io(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            add_entry(&mut entries, &format!("wallet.swaps/{file_name}"), &path)?;
        }
    }

    if let Some(seed) = crate::commands::node::wallet_btc_seed(wallet) {
        for network in [
            bitcoin::Network::Bitcoin,
            bitcoin::Network::Testnet,
            bitcoin::Network::Regtest,
        ] {
            let path = crate::commands::node::btc_utxo_path(Some(wallet_path), network, &seed);
            let marker = path.with_extension("initialized");
            btc_locks.push(
                vtorrent_btc::utxo::lock_state_path(&path)
                    .map_err(|error| TauriError::Io(error.to_string()))?,
            );
            let state_exists = path_entry_exists(&path)?;
            let marker_exists = path_entry_exists(&marker)?;
            if !state_exists && !marker_exists {
                continue;
            }
            if !state_exists {
                return Err(TauriError::InvalidInput(format!(
                    "BTC state is missing for {}; preserve its initialization record for recovery",
                    path.display()
                )));
            }
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            add_entry(&mut entries, file_name, &path)?;
            if marker_exists {
                if !std::fs::symlink_metadata(&marker)
                    .map_err(|error| TauriError::Io(error.to_string()))?
                    .file_type()
                    .is_file()
                {
                    return Err(TauriError::InvalidInput(
                        "BTC initialization record must be a regular file".into(),
                    ));
                }
                let marker_name = marker
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        TauriError::InvalidInput(
                            "BTC initialization record has an invalid name".into(),
                        )
                    })?;
                add_entry(&mut entries, marker_name, &marker)?;
            }
        }
    }

    Ok(entries)
}

fn decode_and_verify(
    backup_base64: &str,
    passphrase: &str,
    otp_code: Option<&str>,
    consume_otp: bool,
) -> Result<(BackupBundle, Wallet, Vec<String>)> {
    if backup_base64.len() > MAX_BACKUP_BYTES * 2 {
        return Err(TauriError::InvalidInput("Backup file is too large".into()));
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(backup_base64)
        .map_err(|_| TauriError::InvalidInput("Backup is not valid base64".into()))?;
    if raw.len() > MAX_BACKUP_BYTES * 2 {
        return Err(TauriError::InvalidInput("Backup file is too large".into()));
    }
    let backup: BackupFile = serde_json::from_slice(&raw)
        .map_err(|_| TauriError::InvalidInput("Backup file is malformed".into()))?;
    if backup.format_version != BACKUP_FORMAT_VERSION {
        return Err(TauriError::InvalidInput(
            "Unsupported backup format version".into(),
        ));
    }
    let plaintext = decrypt_wallet(&backup.encrypted, passphrase)
        .map_err(|_| TauriError::InvalidInput("Incorrect backup passphrase".into()))?;
    let bundle: BackupBundle = serde_json::from_slice(&plaintext)
        .map_err(|_| TauriError::InvalidInput("Backup payload is malformed".into()))?;
    if bundle.format_version != BACKUP_FORMAT_VERSION {
        return Err(TauriError::InvalidInput(
            "Unsupported backup payload version".into(),
        ));
    }

    let mut names = std::collections::HashSet::new();
    let mut wallet_bytes = None;
    let mut files = Vec::new();
    for entry in &bundle.files {
        let safe = safe_relative_path(&entry.name)?;
        if !names.insert(safe.clone()) {
            return Err(TauriError::InvalidInput(format!(
                "Backup contains a duplicate entry: {}",
                entry.name
            )));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&entry.data_base64)
            .map_err(|_| TauriError::InvalidInput("Backup entry is not valid base64".into()))?;
        if bytes.len() > MAX_BACKUP_BYTES {
            return Err(TauriError::InvalidInput(format!(
                "Backup entry {} is too large",
                entry.name
            )));
        }
        if hex::encode(Sha256::digest(&bytes)) != entry.sha256 {
            return Err(TauriError::InvalidInput(format!(
                "Backup entry failed its integrity check: {}",
                entry.name
            )));
        }
        if safe == Path::new(WALLET_ENTRY) {
            wallet_bytes = Some(bytes);
        }
        files.push(entry.name.clone());
    }

    let wallet_bytes = wallet_bytes
        .ok_or_else(|| TauriError::InvalidInput("Backup does not contain a wallet".into()))?;
    let temp_path = std::env::temp_dir().join(format!(
        "vtr-backup-wallet-{}-{}-{:08x}.vtr",
        std::process::id(),
        unix_now(),
        rand::random::<u32>()
    ));
    write_private(&temp_path, &wallet_bytes)?;
    let wallet = if consume_otp {
        Wallet::load(&temp_path, passphrase, otp_code)
    } else {
        Wallet::load_verify_only(&temp_path, passphrase, otp_code)
    }
    .map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        TauriError::InvalidInput(format!("Backup wallet could not be opened: {error}"))
    })?;
    let _ = std::fs::remove_file(&temp_path);

    Ok((bundle, wallet, files))
}

fn preview(bundle: &BackupBundle, wallet: &Wallet, files: Vec<String>) -> BackupPreview {
    BackupPreview {
        wallet_address: wallet
            .default_address()
            .map(|address| address.to_string())
            .unwrap_or_default(),
        has_2fa: wallet.has_2fa(),
        file_count: bundle.files.len(),
        created_at: bundle.created_at,
        files,
    }
}

/// Create an encrypted backup next to the wallet and return its path.
#[tauri::command]
pub fn create_backup(
    state: State<AppState>,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<BackupInfo> {
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let wallet_path = state
        .wallet_path
        .lock()
        .unwrap()
        .clone()
        .ok_or(TauriError::WalletNotInitialized)?;
    let wallet = Wallet::load(
        &wallet_path,
        passphrase.as_str(),
        otp_code.as_ref().map(|code| code.as_str()),
    )
    .map_err(TauriError::from)?;
    let entries = collect_entries(&wallet_path, &wallet)?;
    let created_at = unix_now();
    let bundle = BackupBundle {
        format_version: BACKUP_FORMAT_VERSION,
        created_at,
        files: entries,
    };
    let plaintext = serde_json::to_vec(&bundle).map_err(|e| TauriError::Io(e.to_string()))?;
    let encrypted = encrypt_wallet(&plaintext, passphrase.as_str()).map_err(TauriError::from)?;
    let backup = BackupFile {
        format_version: BACKUP_FORMAT_VERSION,
        encrypted,
    };
    let json = serde_json::to_vec_pretty(&backup).map_err(|e| TauriError::Io(e.to_string()))?;
    let backup_dir = wallet_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups");
    let backup_path = backup_dir.join(format!(
        "vtorrent-backup-{created_at}-{:08x}.vtrbackup",
        rand::random::<u32>()
    ));
    write_private(&backup_path, &json)?;

    Ok(BackupInfo {
        path: backup_path.to_string_lossy().to_string(),
        size: json.len() as u64,
        file_count: bundle.files.len(),
        created_at,
    })
}

/// Decrypt and validate a backup without writing anything.
#[tauri::command]
pub fn verify_backup(
    _state: State<AppState>,
    backup_base64: String,
    passphrase: String,
    otp_code: Option<String>,
) -> Result<BackupPreview> {
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let (bundle, wallet, files) = decode_and_verify(
        &backup_base64,
        passphrase.as_str(),
        otp_code.as_ref().map(|code| code.as_str()),
        false,
    )?;
    Ok(preview(&bundle, &wallet, files))
}

/// Restore a backup into the app data directory.
///
/// The wallet file is moved last and the previous wallet is copied to
/// `backups/pre-restore-<timestamp>.vtr` first, so a failed restore leaves a
/// rollback copy.
#[tauri::command]
pub fn restore_backup(
    state: State<AppState>,
    backup_base64: String,
    passphrase: String,
    otp_code: Option<String>,
    confirm_overwrite: bool,
) -> Result<BackupPreview> {
    let _node_guard = crate::commands::wallet::ensure_node_stopped(state.inner())?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let (bundle, wallet, files) = decode_and_verify(
        &backup_base64,
        passphrase.as_str(),
        otp_code.as_ref().map(|code| code.as_str()),
        true,
    )?;

    let app_data_dir =
        state.app_data_dir.lock().unwrap().clone().ok_or_else(|| {
            TauriError::InvalidInput("Wallet directory is not initialized".into())
        })?;
    let target_wallet = app_data_dir.join(WALLET_ENTRY);
    if target_wallet.exists() && !confirm_overwrite {
        return Err(TauriError::InvalidInput(
            "A wallet already exists; confirm overwrite to restore this backup".into(),
        ));
    }

    let backup_names: std::collections::HashSet<&str> = bundle
        .files
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    for entry in &bundle.files {
        let safe = safe_relative_path(&entry.name)?;
        if safe.components().count() == 1
            && entry.name.starts_with("btc_utxos-")
            && safe
                .extension()
                .is_some_and(|extension| extension == "initialized")
        {
            let state_name = safe.with_extension("json");
            if !state_name
                .to_str()
                .is_some_and(|name| backup_names.contains(name))
            {
                return Err(TauriError::InvalidInput(format!(
                    "BTC initialization record {} has no matching state in the backup",
                    entry.name
                )));
            }
        }
    }
    let mut btc_locks = Vec::new();
    for entry in &bundle.files {
        let safe = safe_relative_path(&entry.name)?;
        if safe.components().count() == 1
            && entry.name.starts_with("btc_utxos-")
            && safe
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            let target = app_data_dir.join(&safe);
            btc_locks.push(
                vtorrent_btc::utxo::lock_state_path(&target)
                    .map_err(|error| TauriError::Io(error.to_string()))?,
            );
            if path_entry_exists(&target.with_extension("initialized"))? {
                return Err(TauriError::InvalidInput(format!("BTC initialization record already exists for {}; preserve it for reviewed recovery", entry.name)));
            }
            match std::fs::symlink_metadata(target) {
                Ok(_) => return Err(TauriError::InvalidInput(format!(
                    "BTC state already exists: {}; preserve it for reviewed recovery rather than overwriting it with a backup",
                    entry.name
                ))),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(TauriError::InvalidInput(format!(
                    "BTC state {} cannot be restored automatically: backup freshness and outstanding sends cannot be established; preserve the backup for reviewed recovery",
                    entry.name
                ))),
                Err(error) => return Err(TauriError::Io(error.to_string())),
            }
        }
    }

    for entry in &bundle.files {
        let safe = safe_relative_path(&entry.name)?;
        if safe.starts_with("wallet.swaps") {
            return Err(TauriError::InvalidInput(
                "Swap recovery records cannot be restored automatically: backup freshness cannot be established; preserve the backup for reviewed recovery".into(),
            ));
        }
        if safe == Path::new("wallet.cold_stake.json") {
            return Err(TauriError::InvalidInput(
                "Cold-stake registry cannot be restored automatically: backup freshness cannot be established; preserve the backup for reviewed recovery".into(),
            ));
        }
        if safe.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .ends_with(".cold_stake.d")
        }) {
            return Err(TauriError::InvalidInput(
                "Cold-stake registry cannot be restored automatically: backup freshness cannot be established; preserve the backup for reviewed recovery".into(),
            ));
        }
        if safe
            .extension()
            .is_some_and(|extension| extension == "spending-key")
        {
            return Err(TauriError::InvalidInput(
                "Cold-stake spending-key exports cannot be restored automatically: preserve the backup and recover the key under the reviewed runbook".into(),
            ));
        }
        if safe != Path::new(WALLET_ENTRY) {
            return Err(TauriError::InvalidInput(format!(
                "Backup contains an unrecognized entry: {}",
                entry.name
            )));
        }
    }
    if path_entry_exists(&app_data_dir.join("wallet.cold_stake.json"))? {
        return Err(TauriError::InvalidInput(
            "An existing cold-stake registry requires reviewed recovery before restoring a wallet"
                .into(),
        ));
    }
    if path_entry_exists(&app_data_dir.join("wallet.swaps"))? {
        return Err(TauriError::InvalidInput(
            "Existing swap recovery state requires reviewed recovery before restoring a wallet"
                .into(),
        ));
    }

    let temp_dir = app_data_dir.join(format!(".restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).map_err(|e| TauriError::Io(e.to_string()))?;
    for entry in &bundle.files {
        let safe = safe_relative_path(&entry.name)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&entry.data_base64)
            .map_err(|_| TauriError::InvalidInput("Backup entry is not valid base64".into()))?;
        write_private(&temp_dir.join(safe), &bytes)?;
    }
    let temp_wallet = temp_dir.join(WALLET_ENTRY);
    // `decode_and_verify` already decrypted this wallet and consumed the OTP
    // step. Re-loading it with the same code would hit replay protection.
    // Save the verified wallet so the advanced step is embedded in the
    // restored file.
    wallet.save(&temp_wallet).map_err(|error| {
        let _ = std::fs::remove_dir_all(&temp_dir);
        TauriError::InvalidInput(format!("Backup wallet could not be saved: {error}"))
    })?;

    if target_wallet.exists() {
        let rollback_dir = app_data_dir.join("backups");
        let rollback = rollback_dir.join(format!(
            "pre-restore-{}-{:08x}.vtr",
            unix_now(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&rollback_dir).map_err(|e| TauriError::Io(e.to_string()))?;
        std::fs::copy(&target_wallet, &rollback).map_err(|e| TauriError::Io(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&rollback, std::fs::Permissions::from_mode(0o600));
        }
    }

    let mut ordered: Vec<&BackupEntry> = bundle.files.iter().collect();
    ordered.sort_by_key(|entry| entry.name == WALLET_ENTRY);
    for entry in ordered {
        let safe = safe_relative_path(&entry.name)?;
        let source = temp_dir.join(&safe);
        let target = app_data_dir.join(&safe);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TauriError::Io(e.to_string()))?;
        }
        std::fs::rename(&source, &target).map_err(|e| TauriError::Io(e.to_string()))?;
    }
    let _ = std::fs::remove_dir_all(&temp_dir);

    *state.wallet.lock().unwrap() = None;
    *state.wallet_path.lock().unwrap() = None;
    state.cold_stake_scripts.lock().unwrap().clear();
    state.prepared_sends.lock().unwrap().clear();

    Ok(preview(&bundle, &wallet, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_with_btc_state_requires_reviewed_recovery_on_restore() {
        use base64::Engine;
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();

        let source = std::env::temp_dir().join(format!("vtr-backup-source-{}", std::process::id()));
        let target = std::env::temp_dir().join(format!("vtr-backup-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();

        let wallet = Wallet::create("password").unwrap();
        let address = wallet.default_address().unwrap().to_string();
        let wallet_path = source.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        let btc_path = crate::commands::node::btc_utxo_path(
            Some(&wallet_path),
            bitcoin::Network::Regtest,
            &seed,
        );
        std::fs::write(&btc_path, b"{}").unwrap();
        let marker = btc_path.with_extension("initialized");
        std::fs::write(&marker, b"vtorrent-btc-initialized-v1\n").unwrap();
        std::fs::write(source.join("wallet.cold_stake.json"), b"[]").unwrap();
        std::fs::create_dir_all(source.join("wallet.swaps")).unwrap();
        std::fs::write(source.join("wallet.swaps/order.json"), b"{}").unwrap();

        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source.clone());

        std::fs::remove_file(&btc_path).unwrap();
        let error = create_backup(state.clone(), "password".into(), None)
            .expect_err("backup must not silently omit missing BTC state");
        assert!(
            error.to_string().contains("BTC state is missing"),
            "{error}"
        );
        std::fs::write(&btc_path, b"{}").unwrap();
        let btc_lock = vtorrent_btc::utxo::lock_state_path(&btc_path).unwrap();
        let error = create_backup(state.clone(), "password".into(), None)
            .expect_err("backup must not read state held by another writer");
        assert!(error.to_string().contains("in use"), "{error}");
        drop(btc_lock);
        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        assert_eq!(info.file_count, 5);
        let backup = std::fs::read(&info.path).unwrap();
        let backup_base64 = base64::engine::general_purpose::STANDARD.encode(&backup);

        let preview = verify_backup(
            state.clone(),
            backup_base64.clone(),
            "password".into(),
            None,
        )
        .unwrap();
        assert_eq!(preview.wallet_address, address);
        assert_eq!(preview.file_count, 5);
        assert!(preview
            .files
            .contains(&"wallet.swaps/order.json".to_string()));

        state.set_app_data_dir(target.clone());
        let error = restore_backup(
            state.clone(),
            backup_base64.clone(),
            "password".into(),
            None,
            false,
        )
        .expect_err("a backup cannot establish BTC state freshness even on an empty destination");
        assert!(error.to_string().contains("freshness"), "{error}");
        assert!(!target.join(WALLET_ENTRY).exists());
        let target_btc = target.join(btc_path.file_name().unwrap());
        let restore_lock = vtorrent_btc::utxo::lock_state_path(&target_btc).unwrap();
        let error = restore_backup(
            state.clone(),
            backup_base64.clone(),
            "password".into(),
            None,
            true,
        )
        .expect_err("restore must honor another BTC writer's lock");
        assert!(error.to_string().contains("in use"), "{error}");
        drop(restore_lock);
        std::fs::write(&target_btc, b"newer pending send state").unwrap();
        let error = restore_backup(state.clone(), backup_base64, "password".into(), None, true)
            .expect_err("a backup must not overwrite newer BTC state");
        assert!(error.to_string().contains("BTC"), "{error}");
        assert_eq!(
            std::fs::read(&target_btc).unwrap(),
            b"newer pending send state"
        );
        assert!(!target.join(WALLET_ENTRY).exists());

        let wrong = verify_backup(
            state.clone(),
            base64::engine::general_purpose::STANDARD.encode(b"not a backup"),
            "password".into(),
            None,
        )
        .unwrap_err();
        assert!(matches!(wrong, TauriError::InvalidInput(_)));

        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn wallet_only_backup_can_still_be_restored() {
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!(
            "vtr-wallet-only-backup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = dir.join("source");
        let target = dir.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let address = wallet.default_address().unwrap().to_string();
        let wallet_path = source.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source);
        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(info.path).unwrap());
        state.set_app_data_dir(target.clone());
        let existing_registry = target.join("wallet.cold_stake.json");
        std::fs::write(&existing_registry, b"[\"existing\"]").unwrap();
        restore_backup(
            state.clone(),
            encoded.clone(),
            "password".into(),
            None,
            false,
        )
        .expect_err("a wallet restore must not inherit another registry");
        assert!(!target.join(WALLET_ENTRY).exists());
        std::fs::remove_file(existing_registry).unwrap();
        let existing_swaps = target.join("wallet.swaps");
        std::fs::create_dir(&existing_swaps).unwrap();
        std::fs::write(existing_swaps.join("order.json"), b"existing swap").unwrap();
        restore_backup(
            state.clone(),
            encoded.clone(),
            "password".into(),
            None,
            false,
        )
        .expect_err("a wallet restore must not inherit existing swap state");
        assert!(!target.join(WALLET_ENTRY).exists());
        std::fs::remove_file(existing_swaps.join("order.json")).unwrap();
        std::fs::remove_dir(existing_swaps).unwrap();
        let restored =
            restore_backup(state.clone(), encoded, "password".into(), None, false).unwrap();
        assert_eq!(restored.wallet_address, address);
        assert!(target.join(WALLET_ENTRY).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_does_not_replace_newer_swap_recovery_records() {
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-stale-swap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = dir.join("source");
        let target = dir.join("target");
        std::fs::create_dir_all(source.join("wallet.swaps")).unwrap();
        std::fs::create_dir_all(target.join("wallet.swaps")).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = source.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        std::fs::write(source.join("wallet.swaps/order.json"), b"old swap").unwrap();
        std::fs::write(target.join("wallet.swaps/order.json"), b"new swap").unwrap();
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source);
        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(info.path).unwrap());
        state.set_app_data_dir(target.clone());
        restore_backup(state, encoded, "password".into(), None, false)
            .expect_err("older recovery records must not replace newer swap state");
        assert_eq!(
            std::fs::read(target.join("wallet.swaps/order.json")).unwrap(),
            b"new swap"
        );
        assert!(!target.join(WALLET_ENTRY).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_rejects_unrecognized_backup_entries() {
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-unknown-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = dir.join("source");
        let target = dir.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = source.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source);
        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        let mut backup: BackupFile =
            serde_json::from_slice(&std::fs::read(info.path).unwrap()).unwrap();
        let plaintext = decrypt_wallet(&backup.encrypted, "password").unwrap();
        let mut bundle: BackupBundle = serde_json::from_slice(&plaintext).unwrap();
        let data = b"replacement";
        bundle.files.push(BackupEntry {
            name: "other-state.json".into(),
            sha256: hex::encode(Sha256::digest(data)),
            data_base64: base64::engine::general_purpose::STANDARD.encode(data),
        });
        backup.encrypted =
            encrypt_wallet(&serde_json::to_vec(&bundle).unwrap(), "password").unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&backup).unwrap());
        std::fs::write(target.join("other-state.json"), b"existing").unwrap();
        state.set_app_data_dir(target.clone());
        restore_backup(state, encoded, "password".into(), None, true)
            .expect_err("unknown backup entries must not write arbitrary files");
        assert_eq!(
            std::fs::read(target.join("other-state.json")).unwrap(),
            b"existing"
        );
        assert!(!target.join(WALLET_ENTRY).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restore_does_not_replace_newer_cold_stake_registry() {
        use tauri::Manager;

        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(AppState::new());
        let state = app.state::<AppState>();
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-stale-cold-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = dir.join("source");
        let target = dir.join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = source.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        std::fs::write(source.join("wallet.cold_stake.json"), b"[\"old\"]").unwrap();
        std::fs::write(target.join("wallet.cold_stake.json"), b"[\"new\"]").unwrap();
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source);
        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(std::fs::read(info.path).unwrap());
        state.set_app_data_dir(target.clone());
        restore_backup(state, encoded, "password".into(), None, true)
            .expect_err("older backup must not overwrite the cold-stake registry");
        assert_eq!(
            std::fs::read(target.join("wallet.cold_stake.json")).unwrap(),
            b"[\"new\"]"
        );
        assert!(!target.join(WALLET_ENTRY).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_refuses_a_btc_lock_held_by_another_process() {
        use std::io::{Read, Write};
        const CHILD_PATH: &str = "VTR_BACKUP_LOCK_TEST_PATH";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let path = PathBuf::from(path);
            let _lock = vtorrent_btc::utxo::lock_state_path(&path).unwrap();
            std::fs::write(path.with_extension("ready"), b"ready").unwrap();
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte).unwrap();
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-process-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = dir.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        let path = crate::commands::node::btc_utxo_path(
            Some(&wallet_path),
            bitcoin::Network::Regtest,
            &seed,
        );
        vtorrent_btc::utxo::UtxoSet::new().save(&path).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "commands::backup::tests::backup_refuses_a_btc_lock_held_by_another_process",
            ])
            .env(CHILD_PATH, &path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !path.with_extension("ready").exists() {
            if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not acquire BTC state lock");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let result = collect_entries(&wallet_path, &wallet);
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        assert!(child.wait().unwrap().success());
        let error = result.expect_err("backup must fail while another process holds the lock");
        assert!(error.to_string().contains("in use"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_refuses_btc_initialization_in_progress() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-initializing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = dir.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        let seed = crate::commands::node::wallet_btc_seed(&wallet).unwrap();
        let path = crate::commands::node::btc_utxo_path(
            Some(&wallet_path),
            bitcoin::Network::Regtest,
            &seed,
        );
        let lock = vtorrent_btc::utxo::lock_state_path(&path).unwrap();
        assert!(!path.exists());
        assert!(!path.with_extension("initialized").exists());
        let error = collect_entries(&wallet_path, &wallet)
            .expect_err("backup must not omit BTC state while initialization holds its lock");
        assert!(error.to_string().contains("in use"), "{error}");
        drop(lock);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn safe_relative_path_rejects_traversal() {
        assert!(safe_relative_path("../wallet.vtr").is_err());
        assert!(safe_relative_path("/wallet.vtr").is_err());
        assert!(safe_relative_path("swaps/../../wallet.vtr").is_err());
        assert!(safe_relative_path("wallet.vtr").is_ok());
        assert!(safe_relative_path("swaps/order.json").is_ok());
    }

    #[test]
    fn backup_includes_cold_stake_spending_key_exports() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-coldstake-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = dir.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        std::fs::write(dir.join("wallet.Vtest.spending-key"), b"encrypted").unwrap();

        let entries = collect_entries(&wallet_path, &wallet).unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.name.ends_with(".spending-key"))
            .expect("spending-key export must be in the backup");
        assert_eq!(entry.name, "wallet.Vtest.spending-key");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_includes_the_append_only_cold_stake_registry() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-coldreg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = dir.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        let hash = "ab".repeat(20);
        vtorrent_wallet::cold_stake_registry::add(&wallet_path, &hash).unwrap();

        let entries = collect_entries(&wallet_path, &wallet).unwrap();
        let name = format!("wallet.cold_stake.d/{hash}");
        assert!(
            entries.iter().any(|entry| entry.name == name),
            "append-only registry entry must be in the backup"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_rejects_a_symlinked_spending_key_export() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-backup-coldstake-link-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let wallet = Wallet::create("password").unwrap();
        let wallet_path = dir.join(WALLET_ENTRY);
        wallet.save(&wallet_path).unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, b"secret").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("wallet.Vtest.spending-key")).unwrap();

        let error = collect_entries(&wallet_path, &wallet).unwrap_err();
        assert!(error.to_string().contains("regular file"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
