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
///    - Method 1/2: scrypt (N=1<<14, r=8, p=1)
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

/// Derive the encryption key and IV from a passphrase using the legacy method.
/// Method 0: Iterated SHA-512 (Bitcoin/vTorrent default).
///
/// EVP_BytesToKey equivalent: SHA-512 iterated. The first 32 bytes of the
/// final digest are the AES key and the next 16 bytes are the IV.
///
/// The iteration count is capped to prevent a malicious wallet.dat from
/// forcing an unbounded (potentially hours-long) derivation loop.
pub fn derive_key_method0(passphrase: &[u8], salt: &[u8], iterations: u32) -> Result<[u8; 48]> {
    const MAX_ITERATIONS: u32 = 1_000_000;
    if iterations > MAX_ITERATIONS {
        return Err(MigrateError::ExcessiveIterations(iterations));
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(passphrase);
    buf.extend_from_slice(salt);

    let mut hash = Sha512::digest(&buf);

    for _ in 1..iterations {
        hash = Sha512::digest(hash);
    }

    let mut out = [0u8; 48];
    out.copy_from_slice(&hash[..48]);
    Ok(out)
}

/// Derive the encryption key and IV from a passphrase using scrypt.
///
/// Legacy scrypt method: N = 2^14, r = 8, p = 1, producing 48 bytes
/// (32-byte AES key + 16-byte IV), matching the legacy crypter.cpp scrypt
/// parameters.
pub fn derive_key_scrypt(passphrase: &[u8], salt: &[u8]) -> Result<[u8; 48]> {
    use scrypt::{scrypt, Params};
    let params = Params::new(14, 8, 1, 48).map_err(|e| MigrateError::KdfError(e.to_string()))?;
    let mut out = [0u8; 48];
    scrypt(passphrase, salt, &params, &mut out)
        .map_err(|e| MigrateError::KdfError(e.to_string()))?;
    Ok(out)
}

/// Decrypt the master key using AES-256-CBC.
pub fn decrypt_master_key(mkey: &MasterKey, passphrase: &str) -> Result<DecryptedMasterKey> {
    let passphrase_bytes = passphrase.as_bytes();

    let derived_key = match mkey.derivation_method {
        0 => derive_key_method0(passphrase_bytes, &mkey.salt, mkey.derive_iterations)?,
        // scrypt. Legacy Bitcoin numbers it method 1; the vTorrent wallet uses
        // method 2. Support both so scrypt-encrypted wallets migrate regardless.
        1 | 2 => derive_key_scrypt(passphrase_bytes, &mkey.salt)?,
        method => {
            return Err(MigrateError::UnsupportedDerivationMethod(method));
        }
    };

    // The IV for master key decryption is the second 16 bytes of the same
    // iterated SHA-512 output used for the key (EVP_BytesToKey semantics).
    let key: [u8; 32] = derived_key[..32].try_into().unwrap();
    let iv: [u8; 16] = derived_key[32..48].try_into().unwrap();

    // Decrypt using AES-256-CBC
    let mut ciphertext = mkey.encrypted_key.clone();
    // Pad to block boundary if needed
    while !ciphertext.len().is_multiple_of(16) {
        ciphertext.push(0);
    }

    let decryptor = Decryptor::<Aes256>::new(&key.into(), &iv.into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_key_scrypt_matches_reference() {
        // Reference vector computed with Python hashlib.scrypt
        // (n=16384, r=8, p=1, dklen=48) for password/salt below.
        let out = derive_key_scrypt(b"vtorrent-scrypt-test", b"12345678").unwrap();
        let expected = hex::decode(
            "3ec219e5d903d65f6aff959198952652771dd50d33151660c13e0000dc167610a065255376e42986e934a99da131c0d7",
        )
        .unwrap();
        assert_eq!(out.to_vec(), expected);
    }

    #[test]
    fn test_decrypt_master_key_scrypt_roundtrip() {
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};
        use cbc::Encryptor;

        let passphrase = "correct horse battery staple";
        let salt = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
        let derived = derive_key_scrypt(passphrase.as_bytes(), &salt).unwrap();

        // Encrypt a known master key with the scrypt-derived key + IV.
        let plaintext = [0x42u8; 32];
        let key: [u8; 32] = derived[..32].try_into().unwrap();
        let iv: [u8; 16] = derived[32..48].try_into().unwrap();
        let mut buf = plaintext.to_vec();
        let encryptor = Encryptor::<Aes256>::new(&key.into(), &iv.into());
        let ciphertext = encryptor
            .encrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf, plaintext.len())
            .unwrap()
            .to_vec();

        let mkey = MasterKey {
            encrypted_key: ciphertext,
            salt,
            derivation_method: 2,
            derive_iterations: 0,
            other_derivation_parameters: vec![],
        };

        let decrypted = decrypt_master_key(&mkey, passphrase).unwrap();
        assert_eq!(decrypted.key, plaintext);
    }
}
