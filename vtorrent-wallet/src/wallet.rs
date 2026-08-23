use crate::{
    encryption::{decrypt_wallet, encrypt_wallet, EncryptedWallet},
    error::{Result, WalletError},
    otp::{OtpConfig, TotpSecret},
};
/// Main wallet management module.
///
/// Provides the `Wallet` struct which manages:
/// - Key generation and storage
/// - Passphrase-based encryption/decryption (Argon2id + ChaCha20-Poly1305)
/// - TOTP 2FA enable/disable/verify
/// - Import from legacy wallet.dat (via vtorrent-migrate)
/// - Serialization to/from the new encrypted wallet file format
use serde::{Deserialize, Serialize};
use vtorrent_core::{address::Address, keys::PrivateKey, network::mainnet};

/// The wallet file format version.
const WALLET_FORMAT_VERSION: u32 = 1;

/// A key entry stored in the wallet.
///
/// The `Debug` impl redacts the WIF private key so a wallet can never be
/// logged with key material exposed.
#[derive(Clone, Serialize, Deserialize)]
pub struct WalletKeyEntry {
    /// The new-chain vTorrent address (starts with 'V').
    pub address: String,
    /// WIF-encoded private key, zeroized on drop.
    pub wif: zeroize::Zeroizing<String>,
    /// Whether this key was imported from a legacy wallet.
    pub is_legacy_import: bool,
    /// The legacy address this key was imported from (if applicable).
    pub legacy_address: Option<String>,
    /// User-defined label for this address.
    pub label: Option<String>,
    /// Unix timestamp when this key was added.
    pub created_at: u64,
    /// Cached balance in satoshis (updated by the node sync layer).
    pub balance: u64,
}

impl std::fmt::Debug for WalletKeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletKeyEntry")
            .field("address", &self.address)
            .field("wif", &"[REDACTED]")
            .field("is_legacy_import", &self.is_legacy_import)
            .field("legacy_address", &self.legacy_address)
            .field("label", &self.label)
            .field("created_at", &self.created_at)
            .field("balance", &self.balance)
            .finish()
    }
}

/// The plaintext wallet data (serialized to JSON before encryption).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletData {
    pub version: u32,
    pub keys: Vec<WalletKeyEntry>,
    pub default_address: Option<String>,
    pub otp_config: Option<OtpConfig>,
    /// Optional HD account (BIP39 mnemonic) used as the shared seed.
    #[serde(default)]
    pub hd: Option<crate::hd::HdAccount>,
    pub created_at: u64,
    pub last_modified: u64,
}

/// The wallet file as stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletFile {
    pub format_version: u32,
    pub encrypted: EncryptedWallet,
}

/// The in-memory wallet, unlocked and ready to use.
pub struct Wallet {
    data: WalletData,
    /// Passphrase used to encrypt this wallet (held in memory while unlocked).
    passphrase: zeroize::Zeroizing<String>,
}

impl Drop for Wallet {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.passphrase.zeroize();
    }
}

impl Wallet {
    // ─── Construction ─────────────────────────────────────────────────────────

    /// Create a new empty wallet encrypted with the given passphrase.
    /// Automatically generates the first receiving address.
    pub fn create(passphrase: &str) -> Result<Self> {
        let now = unix_now();
        let mut wallet = Self {
            data: WalletData {
                version: WALLET_FORMAT_VERSION,
                keys: Vec::new(),
                default_address: None,
                otp_config: None,
                hd: None,
                created_at: now,
                last_modified: now,
            },
            passphrase: passphrase.to_string().into(),
        };
        // Generate the first receiving address
        wallet.generate_key(Some("Primary Address"))?;
        Ok(wallet)
    }

    /// Load and decrypt a wallet from a file path.
    /// If 2FA is enabled, `otp_code` must be provided to verify.
    pub fn load(path: &std::path::Path, passphrase: &str, otp_code: Option<&str>) -> Result<Self> {
        let json = std::fs::read_to_string(path).map_err(|e| WalletError::Io(e.to_string()))?;
        let wallet_file: WalletFile =
            serde_json::from_str(&json).map_err(|e| WalletError::Serialization(e.to_string()))?;
        let plaintext = decrypt_wallet(&wallet_file.encrypted, passphrase)?;
        let data: WalletData = serde_json::from_slice(&plaintext)
            .map_err(|e| WalletError::Serialization(e.to_string()))?;

        // If 2FA is enabled, verify the OTP code before returning the wallet
        if let Some(config) = &data.otp_config {
            if config.enabled {
                let code = otp_code.ok_or(WalletError::OtpRequired)?;
                config.verify_code(code)?;
            }
        }

        Ok(Self {
            data,
            passphrase: passphrase.to_string().into(),
        })
    }

    /// Save the wallet to a file path (encrypts with the stored passphrase).
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let plaintext = serde_json::to_vec(&self.data)
            .map_err(|e| WalletError::Serialization(e.to_string()))?;
        let encrypted = encrypt_wallet(&plaintext, &self.passphrase)?;
        let wallet_file = WalletFile {
            format_version: WALLET_FORMAT_VERSION,
            encrypted,
        };
        let json = serde_json::to_string_pretty(&wallet_file)
            .map_err(|e| WalletError::Serialization(e.to_string()))?;
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WalletError::Io(e.to_string()))?;
        }
        std::fs::write(path, json).map_err(|e| WalletError::Io(e.to_string()))?;
        Ok(())
    }

    // ─── Key management ───────────────────────────────────────────────────────

    /// Generate a new key and add it to the wallet. Returns the new address.
    pub fn generate_key(&mut self, label: Option<&str>) -> Result<String> {
        use rand::RngCore;
        use secp256k1::SecretKey;

        let mut bytes = [0u8; 32];
        let mut attempts = 0u32;
        loop {
            rand::thread_rng().fill_bytes(&mut bytes);
            if SecretKey::from_slice(&bytes).is_ok() {
                break;
            }
            attempts += 1;
            if attempts >= 1000 {
                return Err(WalletError::KeyGeneration(
                    "failed to generate valid key after 1000 attempts".into(),
                ));
            }
        }

        let privkey = PrivateKey::from_bytes(bytes, true)?;
        let pubkey = privkey.public_key().map_err(WalletError::Core)?;
        let address = Address::from_pubkey(&pubkey, true, mainnet::PUBKEY_ADDRESS_PREFIX);
        let address_str = address.to_string();
        let wif = privkey.to_wif(mainnet::SECRET_KEY_PREFIX);

        let entry = WalletKeyEntry {
            address: address_str.clone(),
            wif: wif.into(),
            is_legacy_import: false,
            legacy_address: None,
            label: label.map(|s| s.to_string()),
            created_at: unix_now(),
            balance: 0,
        };

        self.data.keys.push(entry);
        if self.data.default_address.is_none() {
            self.data.default_address = Some(address_str.clone());
        }
        self.data.last_modified = unix_now();
        Ok(address_str)
    }

    /// Import a WIF-encoded private key (from legacy wallet migration).
    /// Returns the new-chain address for this key.
    pub fn import_wif(&mut self, wif: &str, legacy_address: Option<&str>) -> Result<String> {
        let privkey = PrivateKey::from_wif(wif)?;
        let pubkey = privkey.public_key().map_err(WalletError::Core)?;
        let new_address = Address::from_pubkey(
            &pubkey,
            privkey.is_compressed(),
            mainnet::PUBKEY_ADDRESS_PREFIX,
        );
        let new_address_str = new_address.to_string();
        let new_wif = privkey.to_wif(mainnet::SECRET_KEY_PREFIX);

        // Avoid duplicate imports
        if self.data.keys.iter().any(|k| k.address == new_address_str) {
            return Ok(new_address_str);
        }

        let label = legacy_address
            .map(|a| {
                // Char-boundary-safe truncation: legacy addresses come from
                // parsed wallet.dat records and may not be ASCII.
                let end = a.char_indices().nth(8).map(|(i, _)| i).unwrap_or(a.len());
                format!("Legacy {}", &a[..end])
            })
            .unwrap_or_else(|| "Imported Key".to_string());

        let entry = WalletKeyEntry {
            address: new_address_str.clone(),
            wif: new_wif.into(),
            is_legacy_import: true,
            legacy_address: legacy_address.map(|s| s.to_string()),
            label: Some(label),
            created_at: unix_now(),
            balance: 0,
        };

        self.data.keys.push(entry);
        if self.data.default_address.is_none() {
            self.data.default_address = Some(new_address_str.clone());
        }
        self.data.last_modified = unix_now();
        Ok(new_address_str)
    }

    /// Get all addresses in the wallet.
    pub fn addresses(&self) -> Vec<&str> {
        self.data.keys.iter().map(|k| k.address.as_str()).collect()
    }

    /// Get the default (receive) address.
    pub fn default_address(&self) -> Option<&str> {
        self.data.default_address.as_deref()
    }

    /// Get the WIF private key for the default address.
    ///
    /// Used by the Tauri `send_vtr` command to sign transactions.
    /// The wallet must already be unlocked (loaded into memory) — no passphrase
    /// is required here because the WIF is stored in plaintext in `WalletData`
    /// after decryption.
    pub fn get_default_wif(&self) -> Option<&str> {
        let default_addr = self.data.default_address.as_deref()?;
        self.data
            .keys
            .iter()
            .find(|k| k.address == default_addr)
            .map(|k| k.wif.as_str())
    }

    /// Get the number of keys in the wallet.
    pub fn key_count(&self) -> usize {
        self.data.keys.len()
    }

    /// Alias for key_count (used by Tauri command layer).
    pub fn address_count(&self) -> usize {
        self.data.keys.len()
    }

    /// Get the number of imported legacy keys.
    pub fn legacy_import_count(&self) -> usize {
        self.data.keys.iter().filter(|k| k.is_legacy_import).count()
    }

    /// List all addresses with their labels, balances, and import status.
    /// Returns: Vec<(address, label, balance, is_legacy_import)>
    pub fn list_addresses(&self) -> Vec<(String, String, u64, bool)> {
        self.data
            .keys
            .iter()
            .map(|k| {
                (
                    k.address.clone(),
                    k.label.clone().unwrap_or_else(|| "Address".to_string()),
                    k.balance,
                    k.is_legacy_import,
                )
            })
            .collect()
    }

    // ─── 2FA / OTP ────────────────────────────────────────────────────────────

    /// Enable TOTP 2FA on the wallet.
    /// Returns the OtpConfig for the Tauri command layer to extract URI and secret.
    pub fn enable_2fa(&mut self) -> Result<OtpConfig> {
        let secret = TotpSecret::generate();
        let config = OtpConfig::new(&secret);
        self.data.otp_config = Some(config.clone());
        self.data.last_modified = unix_now();
        Ok(config)
    }

    /// Disable TOTP 2FA on the wallet.
    pub fn disable_2fa(&mut self) -> Result<()> {
        self.data.otp_config = None;
        self.data.last_modified = unix_now();
        Ok(())
    }

    /// Check whether 2FA is enabled.
    pub fn has_2fa(&self) -> bool {
        self.data
            .otp_config
            .as_ref()
            .map(|c| c.enabled)
            .unwrap_or(false)
    }

    /// Verify an OTP code against the wallet's 2FA secret.
    /// Returns Ok(true) if valid, Ok(false) if invalid, Err if 2FA not enabled.
    pub fn verify_2fa(&self, code: &str) -> Result<bool> {
        let config = self
            .data
            .otp_config
            .as_ref()
            .ok_or(WalletError::OtpNotEnabled)?;
        match config.verify_code(code) {
            Ok(()) => Ok(true),
            Err(WalletError::OtpInvalidCode) => Ok(false),
            Err(e) => Err(e),
        }
    }

    // ─── HD / seed ────────────────────────────────────────────────────────────

    /// Enable HD on this wallet by generating a BIP39 mnemonic.
    /// Returns the mnemonic phrase so the caller can display it for backup.
    pub fn enable_hd(&mut self) -> Result<String> {
        if let Some(hd) = &self.data.hd {
            return Ok(hd.mnemonic.to_string());
        }
        let mnemonic = crate::hd::Mnemonic::generate()?;
        let phrase = mnemonic.phrase().to_string();
        self.data.hd = Some(crate::hd::HdAccount {
            mnemonic: phrase.clone().into(),
            word_count: 24,
            created_at: unix_now(),
        });
        self.data.last_modified = unix_now();
        Ok(phrase)
    }

    /// Whether this wallet has an HD account (mnemonic) set.
    pub fn has_hd(&self) -> bool {
        self.data.hd.is_some()
    }

    /// Get the mnemonic phrase, if HD is enabled.
    pub fn mnemonic(&self) -> Option<&str> {
        self.data.hd.as_ref().map(|h| h.mnemonic.as_str())
    }

    // ─── Serialization helpers ────────────────────────────────────────────────

    /// Serialize the wallet to an in-memory JSON string (for testing).
    pub fn to_json_file(&self, passphrase: &str) -> Result<String> {
        let plaintext = serde_json::to_vec(&self.data)
            .map_err(|e| WalletError::Serialization(e.to_string()))?;
        let encrypted = encrypt_wallet(&plaintext, passphrase)?;
        let wallet_file = WalletFile {
            format_version: WALLET_FORMAT_VERSION,
            encrypted,
        };
        serde_json::to_string_pretty(&wallet_file)
            .map_err(|e| WalletError::Serialization(e.to_string()))
    }

    /// Load a wallet from a JSON string (for testing).
    pub fn from_json_file(json: &str, passphrase: &str, otp_code: Option<&str>) -> Result<Self> {
        let wallet_file: WalletFile =
            serde_json::from_str(json).map_err(|e| WalletError::Serialization(e.to_string()))?;
        let plaintext = decrypt_wallet(&wallet_file.encrypted, passphrase)?;
        let data: WalletData = serde_json::from_slice(&plaintext)
            .map_err(|e| WalletError::Serialization(e.to_string()))?;

        // If 2FA is enabled, verify the OTP code before returning the wallet
        if let Some(config) = &data.otp_config {
            if config.enabled {
                let code = otp_code.ok_or(WalletError::OtpRequired)?;
                config.verify_code(code)?;
            }
        }

        Ok(Self {
            data,
            passphrase: passphrase.to_string().into(),
        })
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallet_create_and_generate_key() {
        let wallet = Wallet::create("test-pass-123").expect("Create failed");
        // create() auto-generates the first key
        assert_eq!(wallet.key_count(), 1);
        let addr = wallet.default_address().expect("No default address");
        assert!(
            addr.starts_with('V'),
            "Address should start with 'V', got: {}",
            addr
        );
    }

    #[test]
    fn test_wallet_save_and_load() {
        let wallet = Wallet::create("test-passphrase-123").expect("Create failed");
        let passphrase = "test-passphrase-123";
        let json = wallet.to_json_file(passphrase).expect("Save failed");
        assert!(!json.is_empty());

        let loaded = Wallet::from_json_file(&json, passphrase, None).expect("Load failed");
        assert_eq!(loaded.key_count(), wallet.key_count());
        assert_eq!(loaded.addresses(), wallet.addresses());
    }

    #[test]
    fn test_wallet_wrong_passphrase_fails() {
        let wallet = Wallet::create("correct-pass").expect("Create failed");
        let json = wallet.to_json_file("correct-pass").expect("Save failed");
        let result = Wallet::from_json_file(&json, "wrong-pass", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_wallet_2fa_enable_and_verify() {
        let mut wallet = Wallet::create("test-pass").expect("Create failed");
        let config = wallet.enable_2fa().expect("Enable 2FA failed");

        let uri = config.to_uri("vTorrent-Wallet");
        let base32 = config.secret_base32();
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(!base32.is_empty());
        assert!(wallet.has_2fa());

        // Generate the current code and verify it
        let secret = TotpSecret::from_base32(&base32).expect("Base32 decode failed");
        let code = secret.current_code().expect("Code generation failed");
        let valid = wallet.verify_2fa(&code).expect("Verify failed");
        assert!(valid, "Valid code should be accepted");
    }

    #[test]
    fn test_wallet_2fa_required_on_load() {
        let mut wallet = Wallet::create("test-pass").expect("Create failed");
        let config = wallet.enable_2fa().expect("Enable 2FA failed");
        let base32 = config.secret_base32();

        let passphrase = "test-pass";
        let json = wallet.to_json_file(passphrase).expect("Save failed");

        // Loading without OTP code should fail
        let result = Wallet::from_json_file(&json, passphrase, None);
        assert!(matches!(result, Err(WalletError::OtpRequired)));

        // Loading with correct OTP code should succeed
        let secret = TotpSecret::from_base32(&base32).expect("Base32 decode failed");
        let code = secret.current_code().expect("Code generation failed");
        let loaded =
            Wallet::from_json_file(&json, passphrase, Some(&code)).expect("Load with OTP failed");
        assert_eq!(loaded.key_count(), wallet.key_count());
    }

    #[test]
    fn test_wallet_import_wif() {
        let mut wallet = Wallet::create("test-pass").expect("Create failed");
        let initial_count = wallet.key_count();

        // Generate a key in another wallet and import its WIF
        let other = Wallet::create("other-pass").expect("Create failed");
        let other_addr = other.default_address().unwrap().to_string();
        let other_wif = other.data.keys[0].wif.clone();

        let imported_addr = wallet
            .import_wif(&other_wif, Some(&other_addr))
            .expect("Import failed");
        assert_eq!(wallet.key_count(), initial_count + 1);
        assert_eq!(imported_addr, other_addr);

        // Importing the same key again should be idempotent
        wallet
            .import_wif(&other_wif, Some(&other_addr))
            .expect("Re-import failed");
        assert_eq!(
            wallet.key_count(),
            initial_count + 1,
            "Duplicate import should be ignored"
        );
    }

    #[test]
    fn test_wallet_list_addresses() {
        let wallet = Wallet::create("test-pass").expect("Create failed");
        let list = wallet.list_addresses();
        assert_eq!(list.len(), 1);
        let (addr, _label, balance, is_import) = &list[0];
        assert!(addr.starts_with('V'));
        assert_eq!(*balance, 0);
        assert!(!is_import);
    }

    #[test]
    fn test_enable_hd() {
        let mut wallet = Wallet::create("test-pass").expect("Create failed");
        assert!(wallet.data.hd.is_none());

        let mnemonic = wallet.enable_hd().expect("enable_hd failed");
        assert_eq!(mnemonic.split_whitespace().count(), 24);
        assert!(wallet.data.hd.is_some());
        assert!(wallet.has_hd());
    }
}
