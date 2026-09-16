use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A raw key-value record extracted from the BerkeleyDB wallet.dat.
#[derive(Debug, Clone)]
pub struct RawRecord {
    pub key_data: Vec<u8>,
    pub value_data: Vec<u8>,
}

/// The type prefix of a wallet record (first field of the key_data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordType {
    Key,        // Unencrypted private key
    CKey,       // Encrypted private key
    MKey,       // Master encryption key
    Name,       // Address label
    Purpose,    // Address purpose
    Tx,         // Transaction
    AcEntry,    // Account entry
    BestBlock,  // Best known block
    MinVersion, // Minimum client version
    Pool,       // Key pool entry
    Version,    // Wallet version
    CScript,    // Cached script
    WKey,       // Watch-only key
    DefaultKey, // Default receive key
    OtpSecret,  // OTP/2FA secret (vTorrent-specific)
    Unknown(String),
}

impl RecordType {
    pub fn parse(s: &str) -> Self {
        match s {
            "key" => Self::Key,
            "ckey" => Self::CKey,
            "mkey" => Self::MKey,
            "name" => Self::Name,
            "purpose" => Self::Purpose,
            "tx" => Self::Tx,
            "acentry" => Self::AcEntry,
            "bestblock" => Self::BestBlock,
            "minversion" => Self::MinVersion,
            "pool" => Self::Pool,
            "version" => Self::Version,
            "cscript" => Self::CScript,
            "wkey" => Self::WKey,
            "defaultkey" => Self::DefaultKey,
            "otp_SECRET" | "keyOTP" => Self::OtpSecret,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// A master key record (mkey) used to decrypt encrypted private keys.
/// Mirrors the CMasterKey struct in the legacy crypter.h.
#[derive(Debug, Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey {
    /// The encrypted master key bytes.
    pub encrypted_key: Vec<u8>,
    /// The salt used in key derivation.
    pub salt: Vec<u8>,
    /// Derivation method (0 = EVP_sha512, 2 = scrypt).
    pub derivation_method: u32,
    /// Number of derivation iterations.
    pub derive_iterations: u32,
    /// Other derivation parameters.
    pub other_derivation_parameters: Vec<u8>,
}

/// An unencrypted private key record.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct KeyRecord {
    /// The public key bytes (compressed or uncompressed).
    pub public_key: Vec<u8>,
    /// The private key bytes (32 bytes).
    pub private_key: Vec<u8>,
}

impl std::fmt::Debug for KeyRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyRecord")
            .field("public_key", &self.public_key)
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

/// An encrypted private key record (ckey).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct CKeyRecord {
    /// The public key bytes.
    pub public_key: Vec<u8>,
    /// The encrypted private key bytes (AES-256-CBC).
    pub encrypted_private_key: Vec<u8>,
}

impl std::fmt::Debug for CKeyRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CKeyRecord")
            .field("public_key", &self.public_key)
            .field("encrypted_private_key", &"[REDACTED]")
            .finish()
    }
}

/// A fully extracted and decrypted wallet key, ready for migration.
#[derive(Clone, Deserialize)]
pub struct ExtractedKey {
    /// The legacy vTorrent address (starts with 'V').
    pub legacy_address: String,
    /// The WIF-encoded private key (using legacy version byte 198).
    pub wif: String,
    /// Whether the public key was compressed.
    pub compressed: bool,
    /// The source of this key (from `key` record or decrypted `ckey` record).
    pub source: KeySource,
}

/// Serialize with the WIF redacted unless `VTORRENT_SHOW_WIF` is set.
///
/// The human-readable output already hides WIFs behind that env var; without
/// this the `--json` path would silently dump every decrypted private key to
/// stdout (and therefore into any log or CI artifact that captures it).
impl Serialize for ExtractedKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let show = std::env::var("VTORRENT_SHOW_WIF").is_ok();
        let mut s = serializer.serialize_struct("ExtractedKey", 4)?;
        s.serialize_field("legacy_address", &self.legacy_address)?;
        if show {
            s.serialize_field("wif", &self.wif)?;
        } else {
            s.serialize_field("wif", "[hidden - set VTORRENT_SHOW_WIF=1 to reveal]")?;
        }
        s.serialize_field("compressed", &self.compressed)?;
        s.serialize_field("source", &self.source)?;
        s.end()
    }
}

impl std::fmt::Debug for ExtractedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractedKey")
            .field("legacy_address", &self.legacy_address)
            .field("wif", &"[REDACTED]")
            .field("compressed", &self.compressed)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeySource {
    Unencrypted,
    DecryptedFromMasterKey,
}

/// Summary of a wallet.dat extraction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletExtraction {
    /// All extracted keys.
    pub keys: Vec<ExtractedKey>,
    /// Whether the wallet was encrypted.
    pub was_encrypted: bool,
    /// Whether the wallet had 2FA enabled.
    pub had_2fa: bool,
    /// Address labels from the wallet.
    pub labels: std::collections::HashMap<String, String>,
    /// Wallet version number.
    pub wallet_version: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key() -> ExtractedKey {
        ExtractedKey {
            legacy_address: "VTestAddress".to_string(),
            wif: "7SecretWifValue".to_string(),
            compressed: true,
            source: KeySource::Unencrypted,
        }
    }

    #[test]
    fn json_redacts_wif_unless_opted_in() {
        // Single test: the two states share a process-wide env var, so
        // separate tests would race under the parallel test harness.
        std::env::remove_var("VTORRENT_SHOW_WIF");
        let hidden = serde_json::to_string(&sample_key()).unwrap();
        assert!(
            !hidden.contains("7SecretWifValue"),
            "WIF must not appear in JSON output by default: {hidden}"
        );
        assert!(hidden.contains("hidden"));

        std::env::set_var("VTORRENT_SHOW_WIF", "1");
        let shown = serde_json::to_string(&sample_key()).unwrap();
        std::env::remove_var("VTORRENT_SHOW_WIF");
        assert!(shown.contains("7SecretWifValue"));
    }
}
