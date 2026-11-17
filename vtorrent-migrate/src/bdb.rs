use crate::error::{MigrateError, Result};
use crate::types::RawRecord;
use std::io::Write;

/// BerkeleyDB file magic number (little-endian at offset 12).
const BDB_MAGIC: u32 = 0x00053162;

/// Minimum records the native parser should find to be considered working.
/// If fewer records are found, we fall back to db5.3_dump.
const MIN_RECORDS_FOR_NATIVE: usize = 1000;

/// Parse all key-value records from a BerkeleyDB wallet.dat file.
///
/// Tries the native parser first. If it finds suspiciously few records
/// (indicating a BDB version mismatch), falls back to `db5.3_dump`.
pub fn parse_wallet(data: &[u8]) -> Result<Vec<RawRecord>> {
    parse_wallet_inner(data, None)
}

/// Parse all key-value records, optionally given the original file path.
///
/// When `file_path` is provided and points to a readable file, the db5.3_dump
/// fallback runs directly on the original file (avoids writing 700 MB to a
/// tempfile which can trigger BDB verification errors).
pub fn parse_wallet_with_path(
    data: &[u8],
    file_path: Option<&std::path::Path>,
) -> Result<Vec<RawRecord>> {
    parse_wallet_inner(data, file_path)
}

fn parse_wallet_inner(data: &[u8], file_path: Option<&std::path::Path>) -> Result<Vec<RawRecord>> {
    if data.len() < 512 {
        return Err(MigrateError::NotBerkeleyDb);
    }

    let magic = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if magic != BDB_MAGIC {
        let magic_be = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        if magic_be != BDB_MAGIC {
            return Err(MigrateError::NotBerkeleyDb);
        }
    }

    let native_records = match parse_wallet_native(data) {
        Ok(records) => records,
        Err(native_error) => {
            return parse_wallet_via_dbdump_path(data, file_path).map_err(|dump_error| {
                MigrateError::Other(format!(
                    "native BDB parser: {native_error}; dump: {dump_error}"
                ))
            });
        }
    };

    if native_records.len() < MIN_RECORDS_FOR_NATIVE {
        eprintln!(
            "Native parser found only {} records — trying db5.3_dump fallback...",
            native_records.len()
        );
        match parse_wallet_via_dbdump_path(data, file_path) {
            Ok(dump_records) if dump_records.len() >= native_records.len() => {
                eprintln!(
                    "db5.3_dump found {} records — using fallback parser.",
                    dump_records.len()
                );
                return Ok(dump_records);
            }
            Ok(dump_records) => {
                eprintln!(
                    "db5.3_dump found {} records, fewer than the native parser's {} — keeping native results.",
                    dump_records.len(),
                    native_records.len()
                );
                return Ok(native_records);
            }
            Err(e) => {
                eprintln!("db5.3_dump fallback failed: {e}");
                if native_records.is_empty() {
                    return Err(MigrateError::Other(format!(
                        "no records were recognized; db5.3_dump fallback failed: {e}"
                    )));
                }
                return Ok(native_records);
            }
        }
    }

    Ok(native_records)
}

struct OverflowPage {
    next: u32,
    data: Vec<u8>,
}

/// Native BDB parser for the btree page format used by legacy `wallet.dat`.
pub(crate) fn parse_wallet_native(data: &[u8]) -> Result<Vec<RawRecord>> {
    let page_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    if !(512..=65536).contains(&page_size) || (page_size & (page_size - 1)) != 0 {
        return Err(MigrateError::UnsupportedPageSize(page_size));
    }

    let page_size = page_size as usize;
    if !data.len().is_multiple_of(page_size) {
        return Err(MigrateError::Other(
            "BerkeleyDB file ends in a partial page".into(),
        ));
    }
    let num_pages = data.len() / page_size;
    if num_pages < 2 {
        return Err(MigrateError::Other("BerkeleyDB has no data pages".into()));
    }
    let mut records = Vec::new();

    let mut overflow_pages: std::collections::HashMap<u32, OverflowPage> =
        std::collections::HashMap::new();

    for page_idx in 1..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];
        if page[25] != 7 {
            continue;
        }
        let page_num = u32::from_le_bytes([page[8], page[9], page[10], page[11]]);
        if page_num as usize != page_idx {
            return Err(MigrateError::Other("BDB page number mismatch".into()));
        }
        overflow_pages.insert(
            page_num,
            OverflowPage {
                next: u32::from_le_bytes([page[16], page[17], page[18], page[19]]),
                data: page[26..].to_vec(),
            },
        );
    }

    let mut decoded_bytes = 0usize;
    for page_idx in 1..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];
        match page[25] {
            // Internal pages, metadata, duplicate-value pages, overflow pages,
            // and freed pages carry no keys of their own.
            0 | 3 | 7 | 9 | 12 => continue,
            5 => {}
            8 | 2 | 13 => {
                return Err(MigrateError::Other(
                    "hash BerkeleyDB files require db5.3_dump".into(),
                ))
            }
            4 | 6 => {
                return Err(MigrateError::Other(
                    "recno BerkeleyDB files require db5.3_dump".into(),
                ))
            }
            other => {
                return Err(MigrateError::Other(format!(
                    "unsupported BerkeleyDB page type {other}"
                )))
            }
        }
        let num_entries = u16::from_le_bytes([page[20], page[21]]) as usize;
        if num_entries == 0 {
            continue;
        }
        if !num_entries.is_multiple_of(2) {
            return Err(MigrateError::Other("unpaired BDB entries".into()));
        }
        let offsets_start = 26usize;
        let offsets_end = offsets_start + num_entries * 2;
        if offsets_end > page.len() {
            return Err(MigrateError::Other("invalid BDB offset table".into()));
        }
        let mut entry_offsets = Vec::with_capacity(num_entries);
        for i in 0..num_entries {
            let off =
                u16::from_le_bytes([page[offsets_start + i * 2], page[offsets_start + i * 2 + 1]])
                    as usize;
            // A crafted page can repeat offsets, making every pair re-copy the
            // same bytes and amplifying memory by orders of magnitude.
            if off < offsets_end || off >= page.len() || entry_offsets.contains(&off) {
                return Err(MigrateError::Other(
                    "invalid or duplicate BDB entry offset".into(),
                ));
            }
            entry_offsets.push(off);
        }
        let mut i = 0;
        while i + 1 < entry_offsets.len() {
            let key_off = entry_offsets[i];
            let val_off = entry_offsets[i + 1];
            let key_data = read_entry(
                page,
                key_off,
                &overflow_pages,
                data.len(),
                &mut decoded_bytes,
            )?;
            let val_data = read_entry(
                page,
                val_off,
                &overflow_pages,
                data.len(),
                &mut decoded_bytes,
            )?;
            if key_data.is_some() && val_data.is_none() {
                // Off-page duplicate values (for example labels that repeat a
                // name) are not key material.
                i += 2;
                continue;
            }
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

fn read_entry(
    page: &[u8],
    offset: usize,
    overflow_pages: &std::collections::HashMap<u32, OverflowPage>,
    file_len: usize,
    decoded_bytes: &mut usize,
) -> Result<Option<Vec<u8>>> {
    if offset + 3 > page.len() {
        return Err(MigrateError::Other("truncated BDB entry header".into()));
    }
    let data_len = u16::from_le_bytes([page[offset], page[offset + 1]]) as usize;
    // Item types share the high bit with BDB's delete flag.
    let entry_type = page[offset + 2] & 0x7f;
    if page[offset + 2] & 0x80 != 0 {
        return Ok(None);
    }
    match entry_type {
        1 => {
            let data_start = offset + 3;
            let data_end = data_start
                .checked_add(data_len)
                .ok_or_else(|| MigrateError::Other("BDB entry length overflow".into()))?;
            if data_end > page.len() {
                return Err(MigrateError::Other("truncated BDB inline entry".into()));
            }
            *decoded_bytes = decoded_bytes.saturating_add(data_len);
            if *decoded_bytes > file_len {
                return Err(MigrateError::Other(
                    "BDB entries exceed the file size".into(),
                ));
            }
            Ok(Some(page[data_start..data_end].to_vec()))
        }
        3 => {
            // BOVERFLOW: unused1 (2), type (1), unused2 (1), pgno (4),
            // total length (4). The value spans a next_pgno chain of pages.
            if offset + 12 > page.len() {
                return Err(MigrateError::Other("truncated BDB overflow header".into()));
            }
            let first_page = u32::from_le_bytes([
                page[offset + 4],
                page[offset + 5],
                page[offset + 6],
                page[offset + 7],
            ]);
            let total_len = u32::from_le_bytes([
                page[offset + 8],
                page[offset + 9],
                page[offset + 10],
                page[offset + 11],
            ]) as usize;
            *decoded_bytes = decoded_bytes.saturating_add(total_len);
            if *decoded_bytes > file_len {
                return Err(MigrateError::Other(
                    "BDB entries exceed the file size".into(),
                ));
            }
            read_overflow(first_page, total_len, overflow_pages).map(Some)
        }
        // B_DUPLICATE: the value lives on an off-page duplicate leaf.
        2 => Ok(None),
        other => Err(MigrateError::Other(format!(
            "unsupported BDB item type {other}"
        ))),
    }
}

fn read_overflow(
    first_page: u32,
    total_len: usize,
    overflow_pages: &std::collections::HashMap<u32, OverflowPage>,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(total_len.min(1 << 20));
    let mut visited = std::collections::HashSet::new();
    let mut current = first_page;
    while out.len() < total_len {
        if !visited.insert(current) {
            return Err(MigrateError::Other("cyclic BDB overflow chain".into()));
        }
        let page = overflow_pages
            .get(&current)
            .ok_or_else(|| MigrateError::Other("missing BDB overflow page".into()))?;
        let take = (total_len - out.len()).min(page.data.len());
        out.extend_from_slice(&page.data[..take]);
        if out.len() < total_len {
            if page.next == 0 {
                return Err(MigrateError::Other("truncated BDB overflow chain".into()));
            }
            current = page.next;
        }
    }
    if out.len() != total_len {
        return Err(MigrateError::Other("wrong BDB overflow length".into()));
    }
    Ok(out)
}

/// Parse wallet using db5.3_dump, preferring the original file path when available.
fn parse_wallet_via_dbdump_path(
    data: &[u8],
    file_path: Option<&std::path::Path>,
) -> Result<Vec<RawRecord>> {
    if let Some(path) = file_path {
        if path.is_file() {
            let abs_path = std::fs::canonicalize(path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.to_str().unwrap_or_default().to_string());

            match run_dbdump(&["-r", &abs_path]) {
                Ok(output) => {
                    let current = std::fs::read(path)
                        .map_err(|e| MigrateError::Other(format!("re-read failed: {e}")))?;
                    if current != data {
                        return Err(MigrateError::Other(
                            "wallet changed while db5.3_dump was reading it".into(),
                        ));
                    }
                    return parse_dbdump_output(&output);
                }
                Err(e) => {
                    eprintln!("db5.3_dump on original file failed: {e}");
                }
            }
        }
    }

    let mut tmp = tempfile::NamedTempFile::new()
        .map_err(|e| MigrateError::Other(format!("failed to create temp file: {e}")))?;
    tmp.write_all(data)
        .map_err(|e| MigrateError::Other(format!("failed to write temp file: {e}")))?;
    tmp.flush()
        .map_err(|e| MigrateError::Other(format!("failed to flush temp file: {e}")))?;
    let tmp_path = tmp.into_temp_path();

    let path_str = tmp_path.to_str().unwrap_or_default();
    let output = run_dbdump(&["-r", path_str])
        .map_err(|e| MigrateError::Other(format!("db5.3_dump failed: {e}")))?;

    parse_dbdump_output(&output)
}

/// Run `db5.3_dump` with the given arguments.
///
/// Writes output to a tempfile and reads it back. std::process::Command and
/// even raw fork+exec both trigger BDB0090 DB_VERIFY_BAD when called from
/// the full cargo-built binary (likely due to secp256k1/openssl runtime state).
/// This workaround avoids all pipe/fd inheritance issues.
fn run_dbdump(args: &[&str]) -> std::result::Result<String, String> {
    // Use `std::process::Command` rather than a raw `fork`/`execvp`.
    //
    // The previous implementation called `CString::new(..).unwrap()` and
    // `libc::open` in the child between `fork` and `execvp`. In a
    // multithreaded process only async-signal-safe functions are legal there:
    // allocation can deadlock on a lock held by another thread at fork time,
    // and a panic in the child would unwind into an undefined state. Command
    // sets up the stdio redirection before forking and execs directly.
    let tmp = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let out_path = tmp.into_temp_path();
    let out_file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&out_path)
        .map_err(|e| e.to_string())?;

    let status = std::process::Command::new("db5.3_dump")
        .args(args)
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("failed to run db5.3_dump: {e}"))?;

    let output = std::fs::read_to_string(&out_path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out_path);

    if status.success() {
        Ok(output)
    } else {
        match status.code() {
            Some(code) => Err(format!("db5.3_dump exited with status {code}")),
            None => Err("db5.3_dump was killed".into()),
        }
    }
}

/// Parse the output of `db5.3_dump -r`.
///
/// Format: a `HEADER=END` marker, alternating key/value hex lines (one leading
/// space each, and an empty value is a line containing only that space), then
/// `DATA=END`. A file with multiple subdatabases repeats the section.
fn parse_dbdump_output(output: &str) -> Result<Vec<RawRecord>> {
    let mut records = Vec::new();
    let mut in_data = false;
    let mut sections = 0usize;
    let mut pending_key: Option<Vec<u8>> = None;
    for line in output.lines() {
        let value = line.trim();
        if !in_data {
            if value == "HEADER=END" {
                in_data = true;
            }
            continue;
        }
        if value == "DATA=END" {
            if pending_key.is_some() {
                return Err(MigrateError::Other(
                    "db5.3_dump ended with an unmatched key".into(),
                ));
            }
            in_data = false;
            sections += 1;
            continue;
        }
        let bytes = if value.is_empty() {
            Vec::new()
        } else {
            hex_decode(value)
                .map_err(|_| MigrateError::Other("db5.3_dump emitted invalid hex".into()))?
        };
        match pending_key.take() {
            None => pending_key = Some(bytes),
            Some(key_data) => {
                if !key_data.is_empty() {
                    records.push(RawRecord {
                        key_data,
                        value_data: bytes,
                    });
                }
            }
        }
    }
    if pending_key.is_some() || in_data || sections == 0 {
        return Err(MigrateError::Other(
            "db5.3_dump ended before the value line".into(),
        ));
    }

    Ok(records)
}

fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    let raw = hex.as_bytes();
    if !raw.len().is_multiple_of(2) {
        return Err(MigrateError::Other(format!(
            "odd-length hex string: {}",
            raw.len()
        )));
    }
    // Iterate over bytes rather than str-slicing: `&hex[i..i+2]` panics if the
    // input contains multi-byte UTF-8 and the index lands mid-character.
    let mut bytes = Vec::with_capacity(raw.len() / 2);
    for i in (0..raw.len()).step_by(2) {
        let pair = std::str::from_utf8(&raw[i..i + 2])
            .map_err(|_| MigrateError::Other(format!("invalid hex at position {i}")))?;
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|_| MigrateError::Other(format!("invalid hex at position {i}: {pair}")))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Decode the record type string from the key_data bytes.
///
/// In BerkeleyDB wallet.dat, the key starts with a compact-size length prefix
/// followed by the ASCII record type string.
pub fn decode_record_type(key_data: &[u8]) -> Option<(String, &[u8])> {
    if key_data.is_empty() {
        return None;
    }

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
        let mut data = vec![3u8];
        data.extend_from_slice(b"key");
        data.extend_from_slice(&[0x04; 65]);
        let result = decode_record_type(&data);
        assert!(result.is_some());
        let (type_str, rest) = result.unwrap();
        assert_eq!(type_str, "key");
        assert_eq!(rest.len(), 65);
    }

    #[test]
    fn test_decode_record_type_mkey() {
        let mut data = vec![4u8];
        data.extend_from_slice(b"mkey");
        data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        let result = decode_record_type(&data);
        assert!(result.is_some());
        let (type_str, _) = result.unwrap();
        assert_eq!(type_str, "mkey");
    }

    #[test]
    fn test_hex_decode() {
        assert_eq!(hex_decode("00ff").unwrap(), vec![0x00, 0xff]);
        assert_eq!(hex_decode("6d61696e").unwrap(), b"main");
        assert!(hex_decode("odd").is_err());
    }

    #[test]
    fn native_parser_reads_a_real_btree_database() {
        // Generated with `db5.3_load -T -t btree`; synthetic keys only.
        let data = include_bytes!("../tests/fixtures/btree-small.db");
        let records = parse_wallet_native(data).expect("parse a real B-tree database");
        assert_eq!(records.len(), 2, "records: {records:?}");
        assert!(records
            .iter()
            .any(|record| record.key_data == b"key1" && record.value_data == b"val1"));
        assert!(records
            .iter()
            .any(|record| record.key_data == b"key2" && record.value_data == b"val2"));
    }

    #[test]
    fn test_parse_dbdump_output() {
        let output = "VERSION=3\nformat=bytevalue\ntype=btree\nHEADER=END\n 046d6b657901000000\n 301234567890abcdef\nDATA=END\n";
        let records = parse_dbdump_output(output).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].key_data,
            hex_decode("046d6b657901000000").unwrap()
        );
        assert_eq!(
            records[0].value_data,
            hex_decode("301234567890abcdef").unwrap()
        );
    }

    #[test]
    fn test_parse_empty_dbdump() {
        let records = parse_dbdump_output("HEADER=END\nDATA=END\n").unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_parse_dbdump_keeps_empty_values_aligned() {
        let output = "HEADER=END\n 6b657931\n \n 6b657932\n 76616c32\nDATA=END\n";
        let records = parse_dbdump_output(output).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].key_data, b"key1");
        assert!(records[0].value_data.is_empty());
        assert_eq!(records[1].key_data, b"key2");
        assert_eq!(records[1].value_data, b"val2");
    }

    #[test]
    fn test_parse_dbdump_rejects_an_unmatched_key() {
        let output = "HEADER=END\n 6b657931\nDATA=END\n";
        assert!(parse_dbdump_output(output).is_err());
    }

    #[test]
    fn dump_requires_complete_section() {
        assert!(parse_dbdump_output("").is_err());
        assert!(parse_dbdump_output("HEADER=END\n 6b\n 76\n").is_err());
    }

    #[test]
    fn native_parser_reconstructs_overflow_values() {
        let data = include_bytes!("../tests/fixtures/btree-overflow.db");
        let records = parse_wallet_native(data).expect("parse overflow values");
        assert_eq!(records.len(), 2);
        let first = records
            .iter()
            .find(|record| record.key_data == b"key1")
            .expect("key1");
        assert_eq!(first.value_data, vec![b'a'; 5000]);
        assert!(records
            .iter()
            .any(|record| record.key_data == b"key2" && record.value_data == b"val2"));
    }

    #[test]
    fn native_parser_reads_every_record_across_leaf_pages() {
        let data = include_bytes!("../tests/fixtures/btree-multipage.db");
        let records = parse_wallet_native(data).expect("parse multipage database");
        assert_eq!(records.len(), 200);
        for index in 0..200 {
            let key = format!("rec{index:04}");
            let value = vec![b'v'; 40];
            assert!(
                records
                    .iter()
                    .any(|record| record.key_data == key.as_bytes() && record.value_data == value),
                "missing {key}"
            );
        }
    }

    #[test]
    fn native_parser_rejects_a_broken_overflow_chain() {
        let mut data = include_bytes!("../tests/fixtures/btree-overflow.db").to_vec();
        let page = 4096;
        let next = 2 * page + 16;
        data[next..next + 4].copy_from_slice(&99u32.to_le_bytes());
        assert!(parse_wallet_native(&data).is_err());
    }

    #[test]
    fn native_parser_skips_entries_marked_deleted() {
        let mut data = include_bytes!("../tests/fixtures/btree-small.db").to_vec();
        let page = 4096;
        let value_off = u16::from_le_bytes([data[page + 28], data[page + 29]]) as usize;
        data[page + value_off + 2] |= 0x80;
        let records = parse_wallet_native(&data).unwrap();
        assert_eq!(records.len(), 1, "records: {records:?}");
        assert_eq!(records[0].key_data, b"key2");
    }

    #[test]
    fn native_parser_survives_mutations_without_panicking() {
        let fixtures: [&[u8]; 3] = [
            include_bytes!("../tests/fixtures/btree-small.db"),
            include_bytes!("../tests/fixtures/btree-overflow.db"),
            include_bytes!("../tests/fixtures/btree-multipage.db"),
        ];
        for fixture in fixtures {
            for seed in 0..100u32 {
                let mut data = fixture.to_vec();
                let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
                for _ in 0..8 {
                    state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    let index = (state as usize) % data.len();
                    data[index] ^= 1 << ((state >> 8) % 8);
                }
                let _ = parse_wallet_native(&data);
            }
        }
    }

    #[test]
    fn native_matches_the_reference_dump_when_available() {
        for fixture in ["btree-small.db", "btree-overflow.db", "btree-multipage.db"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture);
            let Ok(output) = run_dbdump(&["-r", path.to_str().unwrap()]) else {
                return;
            };
            let reference = parse_dbdump_output(&output).unwrap();
            let native = parse_wallet_native(&std::fs::read(&path).unwrap()).unwrap();
            let mut reference: Vec<_> = reference
                .into_iter()
                .map(|r| (r.key_data, r.value_data))
                .collect();
            let mut native: Vec<_> = native
                .into_iter()
                .map(|r| (r.key_data, r.value_data))
                .collect();
            reference.sort();
            native.sort();
            assert_eq!(native, reference, "{fixture}");
        }
    }

    #[test]
    fn test_run_dbdump_captures_stdout_and_reports_missing_binary() {
        // The subprocess wiring must capture stdout into the temp file and
        // surface a non-zero exit. `db5.3_dump` on a non-BDB file exits
        // non-zero with no records, which exercises the error path without
        // needing a real wallet.
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("not-a-db");
        std::fs::write(&bogus, b"not a berkeley db").unwrap();
        let result = run_dbdump(&[bogus.to_str().unwrap()]);
        // Either the tool is absent (Err from Command) or it ran and failed;
        // both must be an Err, never a panic or a false Ok.
        assert!(result.is_err(), "expected an error, got {result:?}");
    }

    #[test]
    fn test_run_dbdump_missing_binary_is_an_error_not_a_panic() {
        // A non-existent program must produce a clean error. This is the
        // path that previously ran `CString::new(..).unwrap()` in a forked
        // child; it must never panic.
        let result = run_dbdump(&["--definitely-not-a-real-flag"]);
        // With a real db5.3_dump this errors on the bad flag; without it,
        // Command reports the spawn failure. Either way: Err, no panic.
        assert!(result.is_err());
    }
}
