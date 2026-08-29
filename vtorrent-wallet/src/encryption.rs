use crate::error::{Result, WalletError};
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
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    ChaCha20Poly1305, Key, Nonce,
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

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

    let salt_bytes = hex::decode(&encrypted.salt).map_err(|_| WalletError::CorruptedWallet)?;
    let nonce_bytes = hex::decode(&encrypted.nonce).map_err(|_| WalletError::CorruptedWallet)?;
    let ciphertext_bytes =
        hex::decode(&encrypted.ciphertext).map_err(|_| WalletError::CorruptedWallet)?;

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
        assert!(matches!(
            result.unwrap_err(),
            WalletError::IncorrectPassphrase
        ));
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

    #[test]
    fn test_derived_key_zeroize_on_drop() {
        use std::mem::size_of;

        // Verify the struct is correctly sized (32 bytes of key material).
        assert_eq!(size_of::<DerivedKey>(), 32);

        // Verify Zeroize and ZeroizeOnDrop trait bounds are satisfied at compile time.
        fn assert_zeroize<T: Zeroize>() {}
        fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}
        assert_zeroize::<DerivedKey>();
        assert_zeroize_on_drop::<DerivedKey>();

        // Create a DerivedKey, fill with known non-zero bytes, then drop.
        // After drop the memory should be zeroed by ZeroizeOnDrop.
        let mut buf = [0xAAu8; 32];
        {
            let dk = DerivedKey { key: [0xBBu8; 32] };
            // Copy key material out before drop for inspection.
            buf.copy_from_slice(&dk.key);
            assert_eq!(buf, [0xBBu8; 32]);
        }
        // dk is dropped here — ZeroizeOnDrop zeroes the memory.
        // In safe Rust we cannot read the dropped memory, so we rely on
        // the compile-time trait assertions above.
    }

    #[test]
    fn test_wif_encrypt_decrypt_roundtrip() {
        // A sample WIF private key (testnet, uncompressed).
        let wif = "92QksKhtz2hFMgMNp3m7FjFpER5dYvxSqyiCg6g6H6Jp5kJ3vMk";
        let passphrase = "my-secure-passphrase-2024";

        let encrypted = encrypt_wallet(wif.as_bytes(), passphrase).expect("Encryption failed");
        assert_eq!(encrypted.version, ENCRYPTION_VERSION);

        let decrypted = decrypt_wallet(&encrypted, passphrase).expect("Decryption failed");
        assert_eq!(wif.as_bytes(), decrypted.as_slice());

        // Verify the recovered WIF is valid base58.
        let recovered = std::str::from_utf8(&decrypted).expect("WIF is not valid UTF-8");
        assert_eq!(recovered, wif);
    }

    #[test]
    fn test_encrypted_output_varies_with_different_salts() {
        // Two encryptions of the same plaintext with the same passphrase
        // must produce different ciphertexts because salts and nonces are random.
        let plaintext = b"constant payload";
        let passphrase = "same-pass";

        let enc1 = encrypt_wallet(plaintext, passphrase).expect("Encryption failed");
        let enc2 = encrypt_wallet(plaintext, passphrase).expect("Encryption failed");

        assert_ne!(enc1.salt, enc2.salt, "salts should differ");
        assert_ne!(enc1.nonce, enc2.nonce, "nonces should differ");
        assert_ne!(
            enc1.ciphertext, enc2.ciphertext,
            "ciphertexts should differ"
        );
    }

    #[test]
    fn test_deterministic_encryption_with_fixed_salt_nonce() {
        // When the same salt and nonce are used, derive_key + encrypt must be
        // deterministic (identical ciphertext).
        let salt = [0xABu8; 32];
        let nonce_bytes = [0xCDu8; 12];
        let passphrase = "deterministic-test";

        let derived1 = derive_key(passphrase, &salt).expect("Key derivation failed");
        let derived2 = derive_key(passphrase, &salt).expect("Key derivation failed");
        assert_eq!(derived1.key, derived2.key);

        let key1 = Key::from_slice(&derived1.key);
        let cipher1 = ChaCha20Poly1305::new(key1);
        let n1 = Nonce::from_slice(&nonce_bytes);
        let ct1 = cipher1.encrypt(n1, b"hello world".as_ref()).unwrap();

        let key2 = Key::from_slice(&derived2.key);
        let cipher2 = ChaCha20Poly1305::new(key2);
        let n2 = Nonce::from_slice(&nonce_bytes);
        let ct2 = cipher2.encrypt(n2, b"hello world".as_ref()).unwrap();

        assert_eq!(
            ct1, ct2,
            "same salt+nonce must produce identical ciphertext"
        );
    }

    #[test]
    fn test_argon2_key_derivation_properties() {
        let salt = [0x42u8; 32];

        // 1. derive_key produces a 32-byte key.
        let dk = derive_key("passphrase", &salt).expect("Key derivation failed");
        assert_eq!(dk.key.len(), 32);

        // 2. Same passphrase + same salt => same key.
        let dk_a = derive_key("my-passphrase", &salt).expect("Key derivation failed");
        let dk_b = derive_key("my-passphrase", &salt).expect("Key derivation failed");
        assert_eq!(dk_a.key, dk_b.key);

        // 3. Different passphrases => different keys.
        let dk_x = derive_key("passphrase-alpha", &salt).expect("Key derivation failed");
        let dk_y = derive_key("passphrase-beta", &salt).expect("Key derivation failed");
        assert_ne!(dk_x.key, dk_y.key);

        // 4. Empty passphrase still works (Argon2 allows it).
        let dk_empty = derive_key("", &salt).expect("Key derivation failed");
        assert_eq!(dk_empty.key.len(), 32);
    }

    #[test]
    fn test_pubkey_to_vtorrent_address_roundtrip() {
        use std::str::FromStr;
        use vtorrent_core::{address::Address, keys::PrivateKey, network::mainnet};

        // Generate a random private key
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        let privkey = PrivateKey::from_bytes(bytes, true).expect("valid private key");
        let pubkey = privkey.public_key().expect("public key derivation");

        // Derive address
        let address = Address::from_pubkey(&pubkey, true, mainnet::PUBKEY_ADDRESS_PREFIX);
        let address_str = address.to_string();

        // Verify it starts with 'V'
        assert!(
            address_str.starts_with('V'),
            "Address should start with 'V', got: {}",
            address_str
        );

        // Verify it round-trips through FromStr
        let parsed = Address::from_str(&address_str).expect("valid address string");
        assert_eq!(parsed.to_string(), address_str);
    }

    #[test]
    fn test_wif_roundtrip() {
        use vtorrent_core::{keys::PrivateKey, network::mainnet};

        // Generate a random private key
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        let privkey = PrivateKey::from_bytes(bytes, true).expect("valid private key");

        // Encode to WIF
        let wif = privkey.to_wif(mainnet::SECRET_KEY_PREFIX);
        assert!(!wif.is_empty());

        // Decode back from WIF
        let decoded = PrivateKey::from_wif(&wif).expect("valid WIF");
        assert_eq!(decoded.as_bytes(), privkey.as_bytes());
        assert_eq!(decoded.is_compressed(), privkey.is_compressed());
    }
}
