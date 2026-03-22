/// Pure-Rust BerkeleyDB B-tree page parser.
///
/// BerkeleyDB wallet.dat files use the B-tree access method.
/// This parser reads the raw page data and extracts all key-value records
/// without requiring the BerkeleyDB library to be installed.
///
/// Reference: BerkeleyDB internal file format documentation and
/// the bitcoin-wallet tool by Jonas Schnelli.

use std::io::{Cursor, Read};
use crate::error::{MigrateError, Result};
use crate::types::RawRecord;

/// BerkeleyDB file magic number (little-endian at offset 12).
const BDB_MAGIC: u32 = 0x00053162;

/// BerkeleyDB B-tree page type.
const P_LBTREE: u8 = 5;
/// BerkeleyDB overflow page type.
const P_OVERFLOW: u8 = 7;
/// BerkeleyDB internal B-tree page type.
const P_IBTREE: u8 = 3;

/// Parse all key-value records from a BerkeleyDB wallet.dat file.
pub fn parse_wallet(data: &[u8]) -> Result<Vec<RawRecord>> {
    // Validate BDB magic at offset 12 in the metadata page
    if data.len() < 512 {
        return Err(MigrateError::NotBerkeleyDb);
    }

    // The metadata page is always the first page.
    // Magic is at offset 12 within the metadata page.
    let magic = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if magic != BDB_MAGIC {
        // Try big-endian
        let magic_be = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        if magic_be != BDB_MAGIC {
            return Err(MigrateError::NotBerkeleyDb);
        }
    }

    // Page size is at offset 20 in the metadata page.
    let page_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    if page_size < 512 || page_size > 65536 || (page_size & (page_size - 1)) != 0 {
        return Err(MigrateError::UnsupportedPageSize(page_size));
    }

    let page_size = page_size as usize;
    let num_pages = data.len() / page_size;
    let mut records = Vec::new();

    // Collect overflow page data indexed by page number
    let mut overflow_pages: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    
    // First pass: collect overflow pages
    for page_idx in 0..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];
        
        if page.len() < 26 {
            continue;
        }
        
        let page_type = page[25];
        if page_type == P_OVERFLOW {
            let page_num = u32::from_le_bytes([page[4], page[5], page[6], page[7]]);
            // Overflow data starts at offset 26, length is at offset 16
            let data_len = u32::from_le_bytes([page[16], page[17], page[18], page[19]]) as usize;
            let available = page_size.saturating_sub(26);
            let copy_len = data_len.min(available);
            if copy_len > 0 && 26 + copy_len <= page.len() {
                overflow_pages.insert(page_num, page[26..26 + copy_len].to_vec());
            }
        }
    }

    // Second pass: parse leaf B-tree pages
    for page_idx in 0..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];

        if page.len() < 26 {
            continue;
        }

        let page_type = page[25];
        if page_type != P_LBTREE {
            continue;
        }

        // Number of entries on this page (at offset 20)
        let num_entries = u16::from_le_bytes([page[20], page[21]]) as usize;
        if num_entries == 0 {
            continue;
        }

        // Entry offsets start at byte 26 (after the 26-byte page header)
        // Each entry offset is a 2-byte little-endian value
        let offsets_start = 26usize;
        let offsets_end = offsets_start + num_entries * 2;
        if offsets_end > page.len() {
            continue;
        }

        let mut entry_offsets = Vec::with_capacity(num_entries);
        for i in 0..num_entries {
            let off = u16::from_le_bytes([
                page[offsets_start + i * 2],
                page[offsets_start + i * 2 + 1],
            ]) as usize;
            entry_offsets.push(off);
        }

        // Entries come in key-value pairs
        let mut i = 0;
        while i + 1 < entry_offsets.len() {
            let key_off = entry_offsets[i];
            let val_off = entry_offsets[i + 1];

            let key_data = read_entry(page, key_off, &overflow_pages);
            let val_data = read_entry(page, val_off, &overflow_pages);

            if let (Some(k), Some(v)) = (key_data, val_data) {
                if !k.is_empty() {
                    records.push(RawRecord {
                        key_data: k,
                        value_data: v,
                    });
                }
            }

            i += 2;
        }
    }

    Ok(records)
}

/// Read a single entry from a B-tree leaf page.
/// Returns None if the entry is invalid or out of bounds.
fn read_entry(
    page: &[u8],
    offset: usize,
    overflow_pages: &std::collections::HashMap<u32, Vec<u8>>,
) -> Option<Vec<u8>> {
    if offset + 3 > page.len() {
        return None;
    }

    // Entry header: 2 bytes length, 1 byte type
    let data_len = u16::from_le_bytes([page[offset], page[offset + 1]]) as usize;
    let entry_type = page[offset + 2];

    // Type 1 = BKEYDATA (regular data), Type 3 = BOVERFLOW (overflow page reference)
    match entry_type {
        1 => {
            // Regular data: bytes follow immediately after the 3-byte header
            let data_start = offset + 3;
            let data_end = data_start + data_len;
            if data_end > page.len() {
                return None;
            }
            Some(page[data_start..data_end].to_vec())
        }
        3 => {
            // Overflow reference: next 4 bytes are the overflow page number
            if offset + 7 > page.len() {
                return None;
            }
            let overflow_page = u32::from_le_bytes([
                page[offset + 3],
                page[offset + 4],
                page[offset + 5],
                page[offset + 6],
            ]);
            overflow_pages.get(&overflow_page).cloned()
        }
        _ => None,
    }
}

/// Decode the record type string from the key_data bytes.
/// In BerkeleyDB wallet.dat, the key starts with a compact-size length prefix
/// followed by the ASCII record type string.
pub fn decode_record_type(key_data: &[u8]) -> Option<(String, &[u8])> {
    if key_data.is_empty() {
        return None;
    }

    // Compact size encoding: if first byte < 0xfd, it's the length directly
    let (type_len, rest) = if key_data[0] < 0xfd {
        (key_data[0] as usize, &key_data[1..])
    } else if key_data[0] == 0xfd && key_data.len() >= 3 {
        let len = u16::from_le_bytes([key_data[1], key_data[2]]) as usize;
        (len, &key_data[3..])
    } else {
        return None;
    };

    if type_len > rest.len() {
        return None;
    }

    let type_str = std::str::from_utf8(&rest[..type_len]).ok()?.to_string();
    let remaining = &rest[type_len..];
    Some((type_str, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_record_type_key() {
        // Simulate: compact-size 3, then "key", then pubkey bytes
        let mut data = vec![3u8]; // length = 3
        data.extend_from_slice(b"key");
        data.extend_from_slice(&[0x04; 65]); // fake uncompressed pubkey
        
        let result = decode_record_type(&data);
        assert!(result.is_some());
        let (type_str, rest) = result.unwrap();
        assert_eq!(type_str, "key");
        assert_eq!(rest.len(), 65);
    }

    #[test]
    fn test_decode_record_type_mkey() {
        let mut data = vec![4u8]; // length = 4
        data.extend_from_slice(b"mkey");
        data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]); // master key id = 1
        
        let result = decode_record_type(&data);
        assert!(result.is_some());
        let (type_str, _) = result.unwrap();
        assert_eq!(type_str, "mkey");
    }
}
