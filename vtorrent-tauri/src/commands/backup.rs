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

fn collect_entries(wallet_path: &Path, wallet: &Wallet) -> Result<Vec<BackupEntry>> {
    let mut entries = Vec::new();
    add_entry(&mut entries, WALLET_ENTRY, wallet_path)?;

    let cold_stake = wallet_path.with_extension("cold_stake.json");
    if cold_stake.exists() {
        add_entry(&mut entries, "wallet.cold_stake.json", &cold_stake)?;
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
            if !path.exists() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            add_entry(&mut entries, file_name, &path)?;
        }
    }

    Ok(entries)
}

fn decode_and_verify(
    backup_base64: &str,
    passphrase: &str,
    otp_code: Option<&str>,
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
    let wallet = Wallet::load(&temp_path, passphrase, otp_code).map_err(|error| {
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
    crate::commands::wallet::ensure_node_stopped(state.inner())?;
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let otp_code = otp_code.map(zeroize::Zeroizing::new);
    let (bundle, wallet, files) = decode_and_verify(
        &backup_base64,
        passphrase.as_str(),
        otp_code.as_ref().map(|code| code.as_str()),
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
    Wallet::load(
        &temp_wallet,
        passphrase.as_str(),
        otp_code.as_ref().map(|code| code.as_str()),
    )
    .map_err(|error| {
        let _ = std::fs::remove_dir_all(&temp_dir);
        TauriError::InvalidInput(format!("Backup wallet could not be opened: {error}"))
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

    Ok(preview(&bundle, &wallet, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_round_trip_restores_wallet_and_recovery_state() {
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
        std::fs::write(source.join("wallet.cold_stake.json"), b"[]").unwrap();
        std::fs::create_dir_all(source.join("wallet.swaps")).unwrap();
        std::fs::write(source.join("wallet.swaps/order.json"), b"{}").unwrap();

        *state.wallet.lock().unwrap() = Some(wallet);
        *state.wallet_path.lock().unwrap() = Some(wallet_path);
        state.set_app_data_dir(source.clone());

        let info = create_backup(state.clone(), "password".into(), None).unwrap();
        assert_eq!(info.file_count, 4);
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
        assert_eq!(preview.file_count, 4);
        assert!(preview
            .files
            .contains(&"wallet.swaps/order.json".to_string()));

        state.set_app_data_dir(target.clone());
        let restored = restore_backup(
            state.clone(),
            backup_base64.clone(),
            "password".into(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(restored.wallet_address, address);
        assert!(target.join(WALLET_ENTRY).exists());
        assert!(target.join("wallet.cold_stake.json").exists());
        assert!(target.join("wallet.swaps/order.json").exists());
        assert!(target.join(btc_path.file_name().unwrap()).exists());
        let loaded = Wallet::load(&target.join(WALLET_ENTRY), "password", None).unwrap();
        assert_eq!(loaded.default_address().unwrap().to_string(), address);

        let error = restore_backup(
            state.clone(),
            backup_base64.clone(),
            "password".into(),
            None,
            false,
        )
        .unwrap_err();
        assert!(matches!(error, TauriError::InvalidInput(_)));
        restore_backup(state.clone(), backup_base64, "password".into(), None, true).unwrap();

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
    fn safe_relative_path_rejects_traversal() {
        assert!(safe_relative_path("../wallet.vtr").is_err());
        assert!(safe_relative_path("/wallet.vtr").is_err());
        assert!(safe_relative_path("swaps/../../wallet.vtr").is_err());
        assert!(safe_relative_path("wallet.vtr").is_ok());
        assert!(safe_relative_path("swaps/order.json").is_ok());
    }
}
