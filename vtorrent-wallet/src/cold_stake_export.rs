//! Encrypted export of the cold-staking spending key.
//!
//! The spending key is never returned over IPC or HTTP. The backend writes it
//! to a separate encrypted file protected by a dedicated passphrase.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::encryption::{decrypt_wallet, encrypt_wallet, EncryptedWallet};
use crate::error::{Result, WalletError};
use crate::wallet::validate_new_passphrase;

pub const SPENDING_KEY_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct SpendingKeyFile {
    format_version: u32,
    created_at: u64,
    spending_address: String,
    encrypted: EncryptedWallet,
}

#[derive(Debug, Serialize, Deserialize)]
struct SpendingKeyPlaintext {
    wif: String,
    address: String,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Encrypt and atomically write the spending key to `path` with mode 0600.
pub fn export_spending_key(
    path: &Path,
    spending_wif: &str,
    spending_address: &str,
    passphrase: &str,
) -> Result<()> {
    validate_new_passphrase(passphrase)?;
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&SpendingKeyPlaintext {
            wif: spending_wif.to_string(),
            address: spending_address.to_string(),
        })
        .map_err(|error| WalletError::Serialization(error.to_string()))?,
    );
    let encrypted = encrypt_wallet(&plaintext, passphrase)?;
    let file = SpendingKeyFile {
        format_version: SPENDING_KEY_FORMAT_VERSION,
        created_at: now(),
        spending_address: spending_address.to_string(),
        encrypted,
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec_pretty(&file)
            .map_err(|error| WalletError::Serialization(error.to_string()))?,
    );

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("spending-key.tmp.{}.{}", std::process::id(), nanos));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options
        .open(&temp)
        .map_err(|error| WalletError::Io(error.to_string()))?;
    output
        .write_all(&bytes)
        .map_err(|error| WalletError::Io(error.to_string()))?;
    output
        .sync_all()
        .map_err(|error| WalletError::Io(error.to_string()))?;
    drop(output);
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(WalletError::Io(error.to_string()));
    }
    if let Some(parent) = path.parent() {
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

/// Decrypt a spending-key export for offline use.
pub fn load_spending_key(path: &Path, passphrase: &str) -> Result<(Zeroizing<String>, String)> {
    let bytes = std::fs::read(path).map_err(|error| WalletError::Io(error.to_string()))?;
    let file: SpendingKeyFile = serde_json::from_slice(&bytes)
        .map_err(|error| WalletError::Serialization(error.to_string()))?;
    if file.format_version != SPENDING_KEY_FORMAT_VERSION {
        return Err(WalletError::Serialization(
            "unsupported spending-key format".into(),
        ));
    }
    let plaintext = decrypt_wallet(&file.encrypted, passphrase)?;
    let key: SpendingKeyPlaintext = serde_json::from_slice(&plaintext)
        .map_err(|error| WalletError::Serialization(error.to_string()))?;
    Ok((Zeroizing::new(key.wif), key.address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_round_trips_and_rejects_a_wrong_passphrase() {
        let dir = std::env::temp_dir().join(format!(
            "vtr-cold-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wallet.spending-key");
        export_spending_key(&path, "wif-value", "spending-address", "strong-pass").unwrap();
        let (wif, address) = load_spending_key(&path, "strong-pass").unwrap();
        assert_eq!(wif.as_str(), "wif-value");
        assert_eq!(address, "spending-address");
        assert!(load_spending_key(&path, "wrong-pass").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_short_export_passphrase() {
        let path = std::env::temp_dir().join("vtr-cold-export-short");
        assert!(export_spending_key(&path, "wif", "address", "short").is_err());
        assert!(!path.exists());
    }
}
