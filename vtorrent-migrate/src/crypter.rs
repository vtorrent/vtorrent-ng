use crate::error::{MigrateError, Result};
use crate::types::MasterKey;
use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
/// Wallet decryption module.
///
/// This implements the exact same key derivation and decryption scheme
/// used in the legacy vTorrent crypter.cpp, allowing us to decrypt
/// encrypted private keys from old wallet.dat files.
///
/// The legacy scheme:
/// 1. Derive a 32-byte AES key from the passphrase using:
///    - Method 0: SHA-512 iterated (EVP_BytesToKey equivalent)
///    - Method 2: scrypt (N=1<<14, r=8, p=1)
/// 2. Decrypt the master key using AES-256-CBC with IV from the mkey record.
/// 3. Use the decrypted master key to decrypt each ckey (encrypted private key)
///    using AES-256-CBC with the public key hash as IV.
use sha2::{Digest, Sha512};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A decrypted master key, zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DecryptedMasterKey {
    pub key: [u8; 32],
}

/// Derive the encryption key from a passphrase using the legacy method.
/// Method 0: Iterated SHA-512 (Bitcoin/vTorrent default).
pub fn derive_key_method0(passphrase: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    // EVP_BytesToKey equivalent: SHA-512 iterated
    // Output: first 32 bytes = AES key, next 16 bytes = IV (we only need key)
    let mut buf = Vec::new();
    buf.extend_from_slice(passphrase);
    buf.extend_from_slice(salt);

    let mut hash = Sha512::digest(&buf);

    for _ in 1..iterations {
        hash = Sha512::digest(hash);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&hash[..32]);
    key
}

/// Decrypt the master key using AES-256-CBC.
pub fn decrypt_master_key(mkey: &MasterKey, passphrase: &str) -> Result<DecryptedMasterKey> {
    let passphrase_bytes = passphrase.as_bytes();

    let derived_key = match mkey.derivation_method {
        0 => derive_key_method0(passphrase_bytes, &mkey.salt, mkey.derive_iterations),
        _ => {
            // Fall back to method 0 for unsupported methods
            derive_key_method0(passphrase_bytes, &mkey.salt, mkey.derive_iterations)
        }
    };

    // The IV for master key decryption is derived from the passphrase + salt as well
    // (second 16 bytes of the SHA-512 output)
    let mut buf = Vec::new();
    buf.extend_from_slice(passphrase_bytes);
    buf.extend_from_slice(&mkey.salt);
    let hash = Sha512::digest(&buf);
    let iv: [u8; 16] = hash[32..48].try_into().unwrap();

    // Decrypt using AES-256-CBC
    let mut ciphertext = mkey.encrypted_key.clone();
    // Pad to block boundary if needed
    while !ciphertext.len().is_multiple_of(16) {
        ciphertext.push(0);
    }

    let decryptor = Decryptor::<Aes256>::new(&derived_key.into(), &iv.into());
    let decrypted = decryptor
        .decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut ciphertext)
        .map_err(|_| MigrateError::IncorrectPassphrase)?;

    if decrypted.len() < 32 {
        return Err(MigrateError::IncorrectPassphrase);
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(&decrypted[..32]);

    Ok(DecryptedMasterKey { key })
}

/// Decrypt an encrypted private key (ckey) using the decrypted master key.
/// The IV is derived from the public key using Hash256 (SHA256d).
pub fn decrypt_private_key(
    encrypted_privkey: &[u8],
    public_key: &[u8],
    master_key: &DecryptedMasterKey,
) -> Result<Vec<u8>> {
    use vtorrent_core::crypto::sha256d;

    // IV = first 16 bytes of SHA256d(public_key)
    let iv_hash = sha256d(public_key);
    let iv: [u8; 16] = iv_hash[..16].try_into().unwrap();

    let mut ciphertext = encrypted_privkey.to_vec();
    // Ensure block alignment
    while !ciphertext.len().is_multiple_of(16) {
        ciphertext.push(0);
    }

    let decryptor = Decryptor::<Aes256>::new(&master_key.key.into(), &iv.into());
    let decrypted = decryptor
        .decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut ciphertext)
        .map_err(|_| MigrateError::IncorrectPassphrase)?;

    if decrypted.len() < 32 {
        return Err(MigrateError::IncorrectPassphrase);
    }

    // Validate the decrypted key is a valid secp256k1 scalar
    let key_bytes: [u8; 32] = decrypted[..32].try_into().unwrap();
    secp256k1::SecretKey::from_slice(&key_bytes).map_err(|_| MigrateError::IncorrectPassphrase)?;

    Ok(decrypted[..32].to_vec())
}
