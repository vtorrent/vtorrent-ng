/// Wallet encryption module.
///
/// Uses Argon2id for key derivation and ChaCha20-Poly1305 for authenticated
/// encryption of the wallet file. This is significantly stronger than the
/// legacy AES-256-CBC + SHA-512 scheme.
///
/// Encryption scheme:
/// 1. Derive a 32-byte key from passphrase using Argon2id(m=65536, t=3, p=4)
/// 2. Encrypt wallet data with ChaCha20-Poly1305 using a random 96-bit nonce
/// 3. Store: [version(1)][salt(32)][nonce(12)][ciphertext+tag]

use argon2::{Argon2, Params, Version, Algorithm};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce, Key,
};
use zeroize::{Zeroize, ZeroizeOnDrop};
use serde::{Deserialize, Serialize};
use crate::error::{Result, WalletError};

/// Current encryption format version.
const ENCRYPTION_VERSION: u8 = 1;

/// Argon2id parameters (OWASP recommended for wallet encryption).
const ARGON2_MEMORY_KB: u32 = 65536; // 64 MB
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 4;

/// A derived encryption key, zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DerivedKey {
    key: [u8; 32],
}

/// An encrypted wallet blob, ready to be written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedWallet {
    /// Format version byte.
    pub version: u8,
    /// Argon2id salt (hex-encoded).
    pub salt: String,
    /// ChaCha20-Poly1305 nonce (hex-encoded).
    pub nonce: String,
    /// Encrypted wallet data + authentication tag (hex-encoded).
    pub ciphertext: String,
}

/// Derive a 32-byte encryption key from a passphrase using Argon2id.
pub fn derive_key(passphrase: &str, salt: &[u8; 32]) -> Result<DerivedKey> {
    let params = Params::new(
        ARGON2_MEMORY_KB,
        ARGON2_ITERATIONS,
        ARGON2_PARALLELISM,
        Some(32),
    )
    .map_err(|e| WalletError::EncryptionError(format!("Argon2 params error: {}", e)))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| WalletError::EncryptionError(format!("Argon2 hash error: {}", e)))?;

    Ok(DerivedKey { key })
}

/// Encrypt wallet data with the given passphrase.
pub fn encrypt_wallet(plaintext: &[u8], passphrase: &str) -> Result<EncryptedWallet> {
    use rand::RngCore;

    // Generate a random 32-byte salt
    let mut salt = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut salt);

    // Derive the encryption key
    let derived = derive_key(passphrase, &salt)?;
    let key = Key::from_slice(&derived.key);
    let cipher = ChaCha20Poly1305::new(key);

    // Generate a random 12-byte nonce
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);

    // Encrypt
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| WalletError::EncryptionError(format!("Encryption failed: {}", e)))?;

    Ok(EncryptedWallet {
        version: ENCRYPTION_VERSION,
        salt: hex::encode(salt),
        nonce: hex::encode(nonce),
        ciphertext: hex::encode(ciphertext),
    })
}

/// Decrypt wallet data with the given passphrase.
pub fn decrypt_wallet(encrypted: &EncryptedWallet, passphrase: &str) -> Result<Vec<u8>> {
    if encrypted.version != ENCRYPTION_VERSION {
        return Err(WalletError::CorruptedWallet);
    }

    let salt_bytes = hex::decode(&encrypted.salt)
        .map_err(|_| WalletError::CorruptedWallet)?;
    let nonce_bytes = hex::decode(&encrypted.nonce)
        .map_err(|_| WalletError::CorruptedWallet)?;
    let ciphertext_bytes = hex::decode(&encrypted.ciphertext)
        .map_err(|_| WalletError::CorruptedWallet)?;

    if salt_bytes.len() != 32 || nonce_bytes.len() != 12 {
        return Err(WalletError::CorruptedWallet);
    }

    let mut salt = [0u8; 32];
    salt.copy_from_slice(&salt_bytes);

    let derived = derive_key(passphrase, &salt)?;
    let key = Key::from_slice(&derived.key);
    let cipher = ChaCha20Poly1305::new(key);
    let nonce = Nonce::from_slice(&nonce_bytes);

    cipher
        .decrypt(nonce, ciphertext_bytes.as_ref())
        .map_err(|_| WalletError::IncorrectPassphrase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let plaintext = b"This is a test wallet payload with some sensitive key data";
        let passphrase = "correct-horse-battery-staple";

        let encrypted = encrypt_wallet(plaintext, passphrase).expect("Encryption failed");
        let decrypted = decrypt_wallet(&encrypted, passphrase).expect("Decryption failed");

        assert_eq!(plaintext.as_slice(), decrypted.as_slice());
    }

    #[test]
    fn test_wrong_passphrase_fails() {
        let plaintext = b"Secret wallet data";
        let encrypted = encrypt_wallet(plaintext, "correct-passphrase").expect("Encryption failed");

        let result = decrypt_wallet(&encrypted, "wrong-passphrase");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WalletError::IncorrectPassphrase));
    }

    #[test]
    fn test_different_salts_produce_different_keys() {
        let salt1 = [1u8; 32];
        let salt2 = [2u8; 32];
        let passphrase = "same-passphrase";

        let key1 = derive_key(passphrase, &salt1).expect("Key derivation failed");
        let key2 = derive_key(passphrase, &salt2).expect("Key derivation failed");

        assert_ne!(key1.key, key2.key);
    }
}
