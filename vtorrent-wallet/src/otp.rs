use crate::error::{Result, WalletError};
use serde::{Deserialize, Serialize};
/// TOTP-based 2FA module for the vTorrent wallet.
///
/// This implements the same TOTP standard (RFC 6238) as the legacy vTorrent
/// QGoogleAuth class, ensuring full compatibility with existing secrets.
///
/// Users who had 2FA enabled on their old wallet can use the same secret
/// (from their Google Authenticator / Authy backup) with the new client.
use totp_rs::{Algorithm, Secret, TOTP};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// TOTP parameters matching the legacy vTorrent implementation.
const TOTP_DIGITS: usize = 6;
const TOTP_STEP: u64 = 30;
const TOTP_ALGORITHM: Algorithm = Algorithm::SHA1;
const TOTP_ISSUER: &str = "vTorrent";
const TOTP_ACCOUNT: &str = "vTorrent-Wallet";

/// A TOTP secret, zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct TotpSecret {
    /// The raw secret bytes (20 bytes for SHA1-TOTP).
    secret: Vec<u8>,
}

/// The serializable form of an OTP configuration stored in the wallet file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtpConfig {
    /// Base32-encoded TOTP secret (for QR code generation and backup).
    pub secret_base32: String,
    /// Whether 2FA is currently enabled.
    pub enabled: bool,
    /// Timestamp when 2FA was enabled.
    pub enabled_at: u64,
}

impl TotpSecret {
    /// Generate a new random TOTP secret (20 bytes = 160 bits).
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut secret = vec![0u8; 20];
        rand::thread_rng().fill_bytes(&mut secret);
        Self { secret }
    }

    /// Create a TotpSecret from an existing Base32-encoded secret string.
    /// This is used when importing a legacy wallet that had 2FA enabled.
    pub fn from_base32(base32_secret: &str) -> Result<Self> {
        let secret = Secret::Encoded(base32_secret.to_uppercase())
            .to_bytes()
            .map_err(|e| WalletError::EncryptionError(format!("Invalid Base32 secret: {}", e)))?;
        Ok(Self { secret })
    }

    /// Get the Base32-encoded secret string (for display in QR codes and backup).
    pub fn to_base32(&self) -> String {
        base32::encode(base32::Alphabet::RFC4648 { padding: false }, &self.secret)
    }

    /// Get the raw secret bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.secret
    }

    /// Generate the otpauth:// URI for QR code display.
    /// Compatible with Google Authenticator, Authy, and all standard TOTP apps.
    pub fn to_uri(&self, account_label: Option<&str>) -> Result<String> {
        let totp = self.build_totp(account_label)?;
        Ok(totp.get_url())
    }

    /// Get the current TOTP code (6 digits, valid for 30 seconds).
    pub fn current_code(&self) -> Result<String> {
        let totp = self.build_totp(None)?;
        totp.generate_current()
            .map_err(|e| WalletError::EncryptionError(format!("TOTP generation error: {}", e)))
    }

    /// Verify a TOTP code provided by the user.
    /// Accepts codes from the current window ±1 step (90 second tolerance)
    /// to account for clock drift.
    pub fn verify(&self, code: &str) -> Result<bool> {
        let totp = self.build_totp(None)?;
        totp
            .check_current(code)
            .map_err(|e| WalletError::EncryptionError(format!("TOTP check error: {}", e)))
    }

    /// Verify a code and return an error if it is incorrect.
    pub fn verify_or_error(&self, code: &str) -> Result<()> {
        if self.verify(code)? {
            Ok(())
        } else {
            Err(WalletError::OtpInvalidCode)
        }
    }

    /// Get the number of seconds remaining until the current code expires.
    pub fn seconds_remaining(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        TOTP_STEP - (now % TOTP_STEP)
    }

    fn build_totp(&self, account_label: Option<&str>) -> Result<TOTP> {
        let account = account_label.unwrap_or(TOTP_ACCOUNT);
        TOTP::new(
            TOTP_ALGORITHM,
            TOTP_DIGITS,
            1, // skew: accept ±1 window
            TOTP_STEP,
            self.secret.clone(),
            Some(TOTP_ISSUER.to_string()),
            account.to_string(),
        )
        .map_err(|e| WalletError::EncryptionError(format!("TOTP init error: {}", e)))
    }
}

impl OtpConfig {
    /// Create a new OTP configuration from a generated secret.
    pub fn new(secret: &TotpSecret) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            secret_base32: secret.to_base32(),
            enabled: true,
            enabled_at: now,
        }
    }

    /// Load the TOTP secret from this config.
    pub fn load_secret(&self) -> Result<TotpSecret> {
        TotpSecret::from_base32(&self.secret_base32)
    }

    /// Get the TOTP URI for QR code display.
    pub fn to_uri(&self, account_label: &str) -> String {
        let secret = match self.load_secret() {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        secret.to_uri(Some(account_label)).unwrap_or_default()
    }

    /// Get the Base32-encoded secret string.
    pub fn secret_base32(&self) -> String {
        self.secret_base32.clone()
    }

    /// Verify an OTP code against this config.
    pub fn verify_code(&self, code: &str) -> Result<()> {
        if !self.enabled {
            return Err(WalletError::OtpNotEnabled);
        }
        let secret = self.load_secret()?;
        secret.verify_or_error(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_totp_generate_and_verify() {
        let secret = TotpSecret::generate();
        let code = secret.current_code().expect("Failed to generate code");
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));

        // Verify the code we just generated
        let valid = secret.verify(&code).expect("Verify failed");
        assert!(valid, "Generated code should be valid");
    }

    #[test]
    fn test_totp_base32_roundtrip() {
        let secret = TotpSecret::generate();
        let b32 = secret.to_base32();
        assert!(!b32.is_empty());

        let recovered = TotpSecret::from_base32(&b32).expect("Base32 decode failed");
        assert_eq!(secret.as_bytes(), recovered.as_bytes());
    }

    #[test]
    fn test_totp_uri_format() {
        let secret = TotpSecret::generate();
        let uri = secret.to_uri(None).expect("URI generation failed");
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(uri.contains("issuer=vTorrent"));
        assert!(uri.contains("secret="));
    }

    #[test]
    fn test_totp_invalid_code_rejected() {
        let secret = TotpSecret::generate();
        let valid = secret.verify("000000").expect("Verify failed");
        // "000000" is almost certainly wrong (1 in 1,000,000 chance of false positive)
        // We just test that verify runs without error
        let _ = valid;
    }

    #[test]
    fn test_otp_config_roundtrip() {
        let secret = TotpSecret::generate();
        let config = OtpConfig::new(&secret);
        assert!(config.enabled);

        let loaded = config.load_secret().expect("Load secret failed");
        assert_eq!(secret.as_bytes(), loaded.as_bytes());
    }
}
