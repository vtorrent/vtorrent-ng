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

/// Derive the effective wallet passphrase for OTP-enabled legacy wallets.
///
/// OTP builds (crypter_otp.cpp) store the TOTP secret in a `keyOTP` record,
/// encrypted with a SimpleCrypt-style XOR cipher keyed by the first four
/// characters of the raw passphrase. The wallet is then unlocked with
/// `mixedHash(otp_secret, passphrase)` = `hex(SHA256(otp_secret || passphrase))`.
///
/// `otp_record` is the raw DB value: `[compact_size][base64 otaCrypt blob]`.
/// Returns `None` when the record is malformed or the checksum fails
/// (e.g. a different passphrase was used to encrypt the OTP secret).
pub fn derive_otp_mixed_passphrase(otp_record: &[u8], passphrase: &str) -> Option<String> {
    // Strip the CDataStream compact-size prefix around the stored string.
    let stored_str = strip_compact_size(otp_record)?;
    // The stored string is base64 of the otaCrypt blob.
    let decoded = base64_decode(stored_str)?;
    let blob = decoded.as_slice();
    if blob.len() < 3 || blob[0] != 0x03 {
        return None;
    }
    let flags = blob[1];
    let body = &blob[2..];

    // SimpleCrypt key: first 4 passphrase chars (lowercased), packed
    // big-endian into a u64, then read little-endian into 8 key parts.
    let mut l = [0u64; 4];
    for (i, item) in l.iter_mut().enumerate().take(4) {
        if let Some(b) = passphrase.as_bytes().get(i) {
            *item = b.to_ascii_lowercase() as u64;
        }
    }
    let m_key = (l[0] << 48) | (l[1] << 32) | (l[2] << 16) | l[3];
    let key_parts: Vec<u8> = (0..8).map(|i| ((m_key >> (8 * i)) & 0xff) as u8).collect();

    let mut ba = body.to_vec();
    let mut last_char = 0u8;
    for pos in 0..ba.len() {
        let current = ba[pos];
        ba[pos] = current ^ last_char ^ key_parts[pos % 8];
        last_char = current;
    }
    // Drop the leading random char.
    let ba = ba.get(1..)?;

    let payload = if flags & 0x02 != 0 {
        // CryptoFlagChecksum: 2-byte big-endian CRC-16/X-25 of the payload
        // (QDataStream serializes quint16 big-endian).
        if ba.len() < 2 {
            return None;
        }
        let stored = u16::from_be_bytes([ba[0], ba[1]]);
        let data = &ba[2..];
        if crc16_x25(data) != stored {
            return None;
        }
        data
    } else {
        ba
    };

    if flags & 0x01 != 0 {
        // CryptoFlagCompression (qCompress) is never produced for the small
        // OTP payloads in practice (CompressionAuto only compresses when it
        // shrinks the input). Unsupported here.
        return None;
    }

    // The payload is the base64 text of the OTP secret
    // (encryptToString(otp_secret.toBase64())); the wallet unlocks with
    // mixedHash over the *decoded* secret bytes.
    let otp_secret = base64_decode(payload)?;
    otp_mixed_hash(&otp_secret, passphrase)
}

fn otp_mixed_hash(otp_secret: &[u8], passphrase: &str) -> Option<String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(otp_secret);
    hasher.update(passphrase.as_bytes());
    let digest = hasher.finalize();
    Some(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn crc16_x25(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff
}

/// Minimal standard base64 decoder (no padding required).
fn base64_decode(input: &[u8]) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u32> {
        match b {
            b'A'..=b'Z' => Some((b - b'A') as u32),
            b'a'..=b'z' => Some((b - b'a' + 26) as u32),
            b'0'..=b'9' => Some((b - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let trimmed: Vec<u8> = input
        .iter()
        .copied()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    for chunk in trimmed.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut acc: u32 = 0;
        for (i, &b) in chunk.iter().enumerate() {
            acc |= val(b)? << (18 - 6 * i);
        }
        out.push((acc >> 16) as u8);
        if chunk.len() > 2 {
            out.push((acc >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// Strip a CDataStream compact-size length prefix (string serialization).
fn strip_compact_size(data: &[u8]) -> Option<&[u8]> {
    if data.is_empty() {
        return None;
    }
    let first = data[0] as usize;
    if first < 0xfd {
        data.get(1..1 + first)
    } else if first == 0xfd && data.len() >= 3 {
        let len = u16::from_le_bytes([data[1], data[2]]) as usize;
        data.get(3..3 + len)
    } else {
        None
    }
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

    if std::env::var("VTORRENT_MIGRATE_DEBUG").is_ok() {
        eprintln!(
            "[debug] derived key+iv: {}",
            derived_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }

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
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut ciphertext)
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
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut ciphertext)
        .map_err(|_| MigrateError::IncorrectPassphrase)?;

    if decrypted.len() < 32 {
        return Err(MigrateError::IncorrectPassphrase);
    }

    // Validate the decrypted key is a valid secp256k1 scalar
    let key_bytes: [u8; 32] = decrypted[..32].try_into().unwrap();
    let secret = secp256k1::SecretKey::from_slice(&key_bytes)
        .map_err(|_| MigrateError::IncorrectPassphrase)?;

    // Validate the derived public key matches the record's public key.
    // Without this check, a wrong passphrase produces garbage that passes
    // the scalar check and silently yields bogus WIFs.
    let pubkey = secp256k1::PublicKey::from_secret_key(&secp256k1::Secp256k1::new(), &secret);
    if pubkey.serialize().as_slice() != public_key {
        return Err(MigrateError::IncorrectPassphrase);
    }

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
        let mut buf = vec![0u8; 48];
        buf[..32].copy_from_slice(&plaintext);
        let encryptor = Encryptor::<Aes256>::new(&key.into(), &iv.into());
        let ciphertext = encryptor
            .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, plaintext.len())
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

#[cfg(test)]
mod otp_tests {
    use super::*;

    #[test]
    fn test_otp_mixed_passphrase_real_vector() {
        // From the legacy wallet: keyOTP record value
        // 28 (compact size 40) + base64 otaCrypt blob
        let mut rec = vec![0x28u8];
        rec.extend_from_slice(b"AwKRzyl7fEl5KxV5HE1KLhdERHUGV1BlVAIoFR0=");
        let mixed = derive_otp_mixed_passphrase(&rec, "U75kj321")
            .expect("OTP record should decrypt with correct passphrase");
        assert_eq!(
            mixed,
            "c859634caefa3fdd035b656077e572eddbed84fe31f373eea96660ecbb9aa3c9"
        );
    }

    #[test]
    fn test_otp_mixed_passphrase_wrong_pw() {
        let mut rec = vec![0x28u8];
        rec.extend_from_slice(b"AwKRzyl7fEl5KxV5HE1KLhdERHUGV1BlVAIoFR0=");
        assert!(derive_otp_mixed_passphrase(&rec, "wrong").is_none());
    }
}
