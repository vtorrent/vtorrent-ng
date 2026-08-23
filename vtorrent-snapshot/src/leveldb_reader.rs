use crate::error::{Result, SnapshotError};
/// LevelDB reader for the legacy vTorrent chainstate database.
///
/// The legacy vTorrent client (based on Bitcoin 0.8.x / PPCoin) stores its
/// UTXO set in a LevelDB database at `<datadir>/txleveldb/` or `<datadir>/chainstate/`.
///
/// The key/value format is:
///   Key:   [1 byte type 'c'] [txid: 32 bytes] [vout index: varint]
///   Value: [height+coinbase: varint] [compressed_amount: varint] [script_type: varint] [script_data]
///
/// This module provides an iterator over all UTXO records in the database.
use rusty_leveldb::{LdbIterator, Options, DB};

/// A raw UTXO record as read from the LevelDB chainstate.
#[derive(Debug, Clone)]
pub struct RawUtxo {
    /// The transaction ID (32 bytes, as stored in the key).
    pub txid: [u8; 32],
    /// The output index within the transaction.
    pub vout: u32,
    /// The raw value bytes from LevelDB (height, coinbase flag, amount, script).
    pub value_bytes: Vec<u8>,
}

/// Open the legacy chainstate LevelDB and iterate over all UTXO records.
///
/// # Arguments
/// * `chainstate_path` - Path to the `chainstate/` or `txleveldb/` directory.
///
/// # Returns
/// A vector of all raw UTXO records found in the database.
pub fn read_all_utxos(chainstate_path: &std::path::Path) -> Result<Vec<RawUtxo>> {
    let opts = Options {
        create_if_missing: false,
        ..Options::default()
    };

    let mut db = DB::open(chainstate_path, opts)
        .map_err(|e| SnapshotError::LevelDb(format!("Failed to open chainstate DB: {}", e)))?;

    let mut iter = db
        .new_iter()
        .map_err(|e| SnapshotError::LevelDb(format!("Failed to create iterator: {}", e)))?;

    let mut utxos = Vec::new();

    // Seek to first entry and iterate
    iter.seek(&[]);

    let mut key_buf = Vec::new();
    let mut val_buf = Vec::new();

    while iter.valid() {
        key_buf.clear();
        val_buf.clear();

        if !iter.current(&mut key_buf, &mut val_buf) {
            break;
        }

        // The UTXO key format in Bitcoin 0.8.x chainstate:
        // 'c' prefix byte + txid (32 bytes) + vout (varint)
        // In older versions (pre-0.15), it was just txid + vout
        if key_buf.len() >= 33 {
            let (txid_start, vout_start) = if key_buf[0] == b'c' {
                // Bitcoin 0.15+ format with 'c' prefix
                (1usize, 33usize)
            } else {
                // Legacy format without prefix (Bitcoin 0.8.x / PPCoin style)
                (0usize, 32usize)
            };

            if key_buf.len() >= vout_start {
                let mut txid = [0u8; 32];
                txid.copy_from_slice(&key_buf[txid_start..txid_start + 32]);

                let (vout, _) = decode_varint_base128(&key_buf[vout_start..]).unwrap_or((0, 0));

                utxos.push(RawUtxo {
                    txid,
                    vout: vout as u32,
                    value_bytes: val_buf.clone(),
                });
            }
        }

        iter.advance();
    }

    if utxos.is_empty() {
        return Err(SnapshotError::NoUtxosFound);
    }

    tracing::info!("Read {} UTXOs from chainstate database", utxos.len());
    Ok(utxos)
}

/// Decode a Bitcoin-style varint from a byte slice.
/// Returns (value, bytes_consumed).
pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        0..=0xfc => Some((data[0] as u64, 1)),
        0xfd => {
            if data.len() < 3 {
                return None;
            }
            let v = u16::from_le_bytes([data[1], data[2]]) as u64;
            Some((v, 3))
        }
        0xfe => {
            if data.len() < 5 {
                return None;
            }
            let v = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as u64;
            Some((v, 5))
        }
        0xff => {
            if data.len() < 9 {
                return None;
            }
            let v = u64::from_le_bytes([
                data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8],
            ]);
            Some((v, 9))
        }
    }
}

/// Decode a base-128 `VARINT` as written by Bitcoin 0.8.x `serialize.h`
/// (`WriteVarInt`/`ReadVarInt`) — the encoding actually used inside the
/// chainstate DB for the height/code, compressed amount, and script size.
///
/// Continuation bytes have the high bit set and contribute `+1` when shifting
/// in; the final byte has no high bit. Example encodings: 256 → `[0x81,
/// 0x00]`, 65535 → `[0x82, 0xFE, 0x7F]`.
///
/// Returns (value, bytes_consumed).
pub fn decode_varint_base128(data: &[u8]) -> Option<(u64, usize)> {
    let mut n: u64 = 0;
    let mut consumed = 0usize;
    loop {
        let byte = *data.get(consumed)?;
        consumed += 1;
        n = n.checked_shl(7)?.checked_add((byte & 0x7F) as u64)?;
        if byte & 0x80 != 0 {
            n = n.checked_add(1)?;
        } else {
            return Some((n, consumed));
        }
        if consumed > 10 {
            return None; // longer than any u64 can require
        }
    }
}

/// Decode a Bitcoin-style "compressed amount" as used in the chainstate DB.
/// See: https://github.com/bitcoin/bitcoin/blob/master/src/compressor.cpp
pub fn decompress_amount(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut x = x - 1;
    let e = x % 10;
    x /= 10;
    let mut n = if e < 9 {
        let d = x % 9 + 1;
        x /= 9;
        x * 10 + d
    } else {
        x + 1
    };
    for _ in 0..e {
        n *= 10;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_varint_single_byte() {
        assert_eq!(decode_varint(&[0x00]), Some((0, 1)));
        assert_eq!(decode_varint(&[0x42]), Some((0x42, 1)));
        assert_eq!(decode_varint(&[0xfc]), Some((0xfc, 1)));
    }

    #[test]
    fn test_decode_varint_two_bytes() {
        assert_eq!(decode_varint(&[0xfd, 0x00, 0x01]), Some((256, 3)));
    }

    #[test]
    fn test_decode_varint_base128_known_encodings() {
        // Encodings verified against Bitcoin Core serialize.h WriteVarInt.
        assert_eq!(decode_varint_base128(&[0x00]), Some((0, 1)));
        assert_eq!(decode_varint_base128(&[0x2a]), Some((42, 1)));
        // 256 = [0x81, 0x00]
        assert_eq!(decode_varint_base128(&[0x81, 0x00]), Some((256, 2)));
        // 65535 = [0x82, 0xFE, 0x7F]
        assert_eq!(decode_varint_base128(&[0x82, 0xFE, 0x7F]), Some((65535, 3)));
        // Truncated input rejected
        assert_eq!(decode_varint_base128(&[0x81]), None);
        assert_eq!(decode_varint_base128(&[]), None);
    }

    #[test]
    fn test_decompress_amount_zero() {
        assert_eq!(decompress_amount(0), 0);
    }

    #[test]
    fn test_decompress_amount_one_satoshi() {
        // 1 satoshi = compressed value 1
        assert_eq!(decompress_amount(1), 1);
    }
}
