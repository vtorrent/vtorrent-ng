use crate::{
    bdb::{decode_record_type, parse_wallet},
    crypter::{decrypt_master_key, decrypt_private_key, DecryptedMasterKey},
    error::{MigrateError, Result},
    types::{
        CKeyRecord, ExtractedKey, KeyRecord, KeySource, MasterKey, RecordType, WalletExtraction,
    },
};
/// Main wallet extraction logic.
///
/// Ties together the BDB parser, crypter, and key derivation to produce
/// a clean list of extracted keys and their legacy vTorrent addresses.
use std::collections::HashMap;
use vtorrent_core::{address::Address, keys::PrivateKey, network::legacy};

/// Extract all keys from a wallet.dat file.
///
/// # Arguments
/// * `wallet_data` - The raw bytes of the wallet.dat file.
/// * `passphrase` - Optional passphrase if the wallet is encrypted.
///
/// # Returns
/// A `WalletExtraction` containing all extracted keys and metadata.
pub fn extract_wallet(wallet_data: &[u8], passphrase: Option<&str>) -> Result<WalletExtraction> {
    // Step 1: Parse all raw records from the BerkeleyDB file
    let raw_records = parse_wallet(wallet_data)?;

    // Step 2: Categorize records
    let mut unencrypted_keys: Vec<KeyRecord> = Vec::new();
    let mut encrypted_keys: Vec<CKeyRecord> = Vec::new();
    let mut master_keys: HashMap<u32, MasterKey> = HashMap::new();
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut wallet_version: Option<u32> = None;
    let mut had_2fa = false;

    for record in &raw_records {
        let Some((type_str, rest)) = decode_record_type(&record.key_data) else {
            continue;
        };

        match RecordType::parse(&type_str) {
            RecordType::Key => {
                // Key record: key_data = [type][pubkey], value_data = [privkey_bytes]
                if let Some(key_rec) = parse_key_record(rest, &record.value_data) {
                    unencrypted_keys.push(key_rec);
                }
            }
            RecordType::CKey => {
                // CKey record: key_data = [type][pubkey], value_data = [encrypted_privkey]
                if let Some(ckey_rec) = parse_ckey_record(rest, &record.value_data) {
                    encrypted_keys.push(ckey_rec);
                }
            }
            RecordType::MKey => {
                // MKey record: key_data = [type][master_key_id], value_data = [mkey_struct]
                if let Some((id, mkey)) = parse_mkey_record(rest, &record.value_data) {
                    master_keys.insert(id, mkey);
                }
            }
            RecordType::Name => {
                // Name record: key_data = [type][address], value_data = [label_string]
                if let (Ok(addr), Ok(label)) = (
                    std::str::from_utf8(rest),
                    std::str::from_utf8(&record.value_data),
                ) {
                    labels.insert(addr.to_string(), label.to_string());
                }
            }
            RecordType::Version => {
                if record.value_data.len() >= 4 {
                    wallet_version = Some(u32::from_le_bytes([
                        record.value_data[0],
                        record.value_data[1],
                        record.value_data[2],
                        record.value_data[3],
                    ]));
                }
            }
            RecordType::OtpSecret => {
                had_2fa = true;
            }
            _ => {}
        }
    }

    let was_encrypted = !encrypted_keys.is_empty() || !master_keys.is_empty();

    // Step 3: Decrypt encrypted keys if passphrase provided
    let mut extracted_keys: Vec<ExtractedKey> = Vec::new();

    // Process unencrypted keys
    for key_rec in &unencrypted_keys {
        if let Some(extracted) = derive_extracted_key(
            &key_rec.public_key,
            &key_rec.private_key,
            KeySource::Unencrypted,
        ) {
            extracted_keys.push(extracted);
        }
    }

    // Process encrypted keys
    if !encrypted_keys.is_empty() {
        let passphrase = passphrase.ok_or(MigrateError::EncryptedWalletNoPassphrase)?;

        // Try every stored master key: Bitcoin-family wallets write the
        // first mkey under ID 0 (nMasterKeyMaxID starts at 0), while some
        // derivatives use 1. Rotated wallets may hold several — the first
        // one that successfully decrypts any ckey wins.
        if master_keys.is_empty() {
            return Err(MigrateError::EncryptedWalletNoPassphrase);
        }
        let mut ordered_ids: Vec<u32> = master_keys.keys().copied().collect();
        ordered_ids.sort_unstable();

        let mut decrypted_master: Option<DecryptedMasterKey> = None;
        for id in &ordered_ids {
            let mkey = &master_keys[id];
            match decrypt_master_key(mkey, passphrase) {
                Ok(master) => {
                    // Validate this candidate by checking whether it decrypts
                    // at least one ckey below; keep the first candidate that
                    // yields a usable key.
                    let works = encrypted_keys.iter().any(|ckey_rec| {
                        decrypt_private_key(
                            &ckey_rec.encrypted_private_key,
                            &ckey_rec.public_key,
                            &master,
                        )
                        .is_ok()
                    });
                    if works {
                        decrypted_master = Some(master);
                        break;
                    }
                    // Remember the first decryptable candidate as fallback.
                    if decrypted_master.is_none() {
                        decrypted_master = Some(master);
                    }
                }
                Err(_) => continue,
            }
        }
        let decrypted_master = decrypted_master.ok_or(MigrateError::IncorrectPassphrase)?;

        let mut decrypted_any = false;
        for ckey_rec in &encrypted_keys {
            if let Ok(privkey_bytes) = decrypt_private_key(
                &ckey_rec.encrypted_private_key,
                &ckey_rec.public_key,
                &decrypted_master,
            ) {
                decrypted_any = true;
                if let Some(extracted) = derive_extracted_key(
                    &ckey_rec.public_key,
                    &privkey_bytes,
                    KeySource::DecryptedFromMasterKey,
                ) {
                    extracted_keys.push(extracted);
                }
            }
        }

        // A wrong passphrase produces a garbage master key, so every ckey
        // fails its secp256k1 scalar check. Report an incorrect passphrase
        // instead of returning an empty (but "successful") extraction.
        if !decrypted_any {
            return Err(MigrateError::IncorrectPassphrase);
        }
    }

    if extracted_keys.is_empty() && (unencrypted_keys.is_empty() && encrypted_keys.is_empty()) {
        return Err(MigrateError::NoKeysFound);
    }

    Ok(WalletExtraction {
        keys: extracted_keys,
        was_encrypted,
        had_2fa,
        labels,
        wallet_version,
    })
}

/// Parse a `key` record from the wallet.dat.
fn parse_key_record(pubkey_data: &[u8], value_data: &[u8]) -> Option<KeyRecord> {
    // Public key is the remaining bytes after the type string
    // Value data contains the private key (with possible compact-size prefix)
    let privkey = parse_privkey_value(value_data)?;
    Some(KeyRecord {
        public_key: pubkey_data.to_vec(),
        private_key: privkey,
    })
}

/// Parse a `ckey` record from the wallet.dat.
fn parse_ckey_record(pubkey_data: &[u8], value_data: &[u8]) -> Option<CKeyRecord> {
    if value_data.is_empty() {
        return None;
    }
    Some(CKeyRecord {
        public_key: pubkey_data.to_vec(),
        encrypted_private_key: value_data.to_vec(),
    })
}

/// Parse an `mkey` record from the wallet.dat.
fn parse_mkey_record(id_data: &[u8], value_data: &[u8]) -> Option<(u32, MasterKey)> {
    if id_data.len() < 4 || value_data.len() < 8 {
        return None;
    }

    let id = u32::from_le_bytes([id_data[0], id_data[1], id_data[2], id_data[3]]);

    // Parse the CMasterKey serialization:
    // [compact_size: encrypted_key_len][encrypted_key][compact_size: salt_len][salt]
    // [4 bytes: derivation_method][4 bytes: derive_iterations][compact_size: other_params_len][other_params]
    let mut cursor = std::io::Cursor::new(value_data);

    let enc_key = read_compact_bytes(&mut cursor)?;
    let salt = read_compact_bytes(&mut cursor)?;

    let mut method_buf = [0u8; 4];
    let mut iter_buf = [0u8; 4];
    std::io::Read::read_exact(&mut cursor, &mut method_buf).ok()?;
    std::io::Read::read_exact(&mut cursor, &mut iter_buf).ok()?;

    let derivation_method = u32::from_le_bytes(method_buf);
    let derive_iterations = u32::from_le_bytes(iter_buf);

    let other = read_compact_bytes(&mut cursor).unwrap_or_default();

    Some((
        id,
        MasterKey {
            encrypted_key: enc_key,
            salt,
            derivation_method,
            derive_iterations,
            other_derivation_parameters: other,
        },
    ))
}

/// Read a compact-size prefixed byte array from a cursor.
fn read_compact_bytes(cursor: &mut std::io::Cursor<&[u8]>) -> Option<Vec<u8>> {
    let mut len_byte = [0u8; 1];
    std::io::Read::read_exact(cursor, &mut len_byte).ok()?;

    let len = if len_byte[0] < 0xfd {
        len_byte[0] as usize
    } else if len_byte[0] == 0xfd {
        let mut buf = [0u8; 2];
        std::io::Read::read_exact(cursor, &mut buf).ok()?;
        u16::from_le_bytes(buf) as usize
    } else {
        return None;
    };

    let mut data = vec![0u8; len];
    std::io::Read::read_exact(cursor, &mut data).ok()?;
    Some(data)
}

/// Parse the private key from a `key` record value.
fn parse_privkey_value(value_data: &[u8]) -> Option<Vec<u8>> {
    if value_data.is_empty() {
        return None;
    }
    // The value may have a compact-size prefix
    if value_data[0] < 0xfd {
        let len = value_data[0] as usize;
        if len < value_data.len() && len >= 32 {
            return Some(value_data[1..1 + len].to_vec());
        }
    }
    // Or it may be raw 32 bytes
    if value_data.len() >= 32 {
        return Some(value_data[..32].to_vec());
    }
    None
}

/// Derive an ExtractedKey from raw public key and private key bytes.
fn derive_extracted_key(
    pubkey_bytes: &[u8],
    privkey_bytes: &[u8],
    source: KeySource,
) -> Option<ExtractedKey> {
    if privkey_bytes.len() < 32 {
        return None;
    }

    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&privkey_bytes[..32]);

    // Determine if the key was compressed based on the public key length
    let compressed = pubkey_bytes.len() == 33;

    let privkey = PrivateKey::from_bytes(key_bytes, compressed).ok()?;
    let pubkey = privkey.public_key().ok()?;

    // Derive the legacy vTorrent address
    let address = Address::from_pubkey(&pubkey, compressed, legacy::PUBKEY_ADDRESS_PREFIX);
    let legacy_address = address.to_string();

    // Encode to legacy WIF
    let wif = privkey.to_wif(legacy::SECRET_KEY_PREFIX);

    Some(ExtractedKey {
        legacy_address,
        wif,
        compressed,
        source,
    })
}
