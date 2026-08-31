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

    let native_records = parse_wallet_native(data)?;

    if native_records.len() < MIN_RECORDS_FOR_NATIVE {
        eprintln!(
            "Native parser found only {} records — trying db5.3_dump fallback...",
            native_records.len()
        );
        match parse_wallet_via_dbdump_path(data, file_path) {
            Ok(dump_records) => {
                eprintln!(
                    "db5.3_dump found {} records — using fallback parser.",
                    dump_records.len()
                );
                return Ok(dump_records);
            }
            Err(e) => {
                eprintln!("db5.3_dump fallback failed: {e}");
                return Ok(native_records);
            }
        }
    }

    Ok(native_records)
}

/// Native BDB parser (works for BDB 4.x page format).
fn parse_wallet_native(data: &[u8]) -> Result<Vec<RawRecord>> {
    let page_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
    if !(512..=65536).contains(&page_size) || (page_size & (page_size - 1)) != 0 {
        return Err(MigrateError::UnsupportedPageSize(page_size));
    }

    let page_size = page_size as usize;
    let num_pages = data.len() / page_size;
    let mut records = Vec::new();

    let mut overflow_pages: std::collections::HashMap<u32, Vec<u8>> =
        std::collections::HashMap::new();

    for page_idx in 0..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];
        if page.len() < 26 {
            continue;
        }
        let page_type = page[17];
        if page_type == 7 {
            let page_num = u32::from_le_bytes([page[0], page[1], page[2], page[3]]);
            let data_len = u16::from_le_bytes([page[14], page[15]]) as usize;
            let available = page_size.saturating_sub(26);
            let copy_len = data_len.min(available);
            if copy_len > 0 && 26 + copy_len <= page.len() {
                overflow_pages.insert(page_num, page[26..26 + copy_len].to_vec());
            }
        }
    }

    for page_idx in 0..num_pages {
        let page_start = page_idx * page_size;
        let page = &data[page_start..page_start + page_size];
        if page.len() < 26 {
            continue;
        }
        let page_type = page[17];
        if page_type != 5 {
            continue;
        }
        let num_entries = u16::from_le_bytes([page[12], page[13]]) as usize;
        if num_entries == 0 {
            continue;
        }
        let offsets_start = 26usize;
        let offsets_end = offsets_start + num_entries * 2;
        if offsets_end > page.len() {
            continue;
        }
        let mut entry_offsets = Vec::with_capacity(num_entries);
        for i in 0..num_entries {
            let off =
                u16::from_le_bytes([page[offsets_start + i * 2], page[offsets_start + i * 2 + 1]])
                    as usize;
            entry_offsets.push(off);
        }
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

fn read_entry(
    page: &[u8],
    offset: usize,
    overflow_pages: &std::collections::HashMap<u32, Vec<u8>>,
) -> Option<Vec<u8>> {
    if offset + 3 > page.len() {
        return None;
    }
    let data_len = u16::from_le_bytes([page[offset], page[offset + 1]]) as usize;
    let entry_type = page[offset + 2];
    match entry_type {
        1 => {
            let data_start = offset + 3;
            let data_end = data_start + data_len;
            if data_end > page.len() {
                return None;
            }
            Some(page[data_start..data_end].to_vec())
        }
        3 => {
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
                Ok(output) => return parse_dbdump_output(&output),
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
    use std::ffi::CString;

    let tmp = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let out_path = tmp.into_temp_path();
    let out_str = out_path.to_str().unwrap_or_default().to_string();

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err("fork failed".into());
        }

        if pid == 0 {
            let c_out = CString::new(out_str.as_str()).unwrap();
            let fd = libc::open(c_out.as_ptr(), libc::O_WRONLY | libc::O_TRUNC, 0o644);
            if fd >= 0 {
                libc::dup2(fd, 1);
                libc::close(fd);
            }
            let devnull = libc::open(c"/dev/null".as_ptr().cast(), libc::O_WRONLY);
            if devnull >= 0 {
                libc::dup2(devnull, 2);
                libc::close(devnull);
            }

            let prog = CString::new("db5.3_dump").unwrap();
            let mut c_args: Vec<CString> = vec![prog];
            for a in args {
                c_args.push(CString::new(*a).unwrap());
            }
            let c_ptrs: Vec<*const i8> = c_args
                .iter()
                .map(|c| c.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect();
            libc::execvp(c_ptrs[0], c_ptrs.as_ptr());
            libc::_exit(127);
        }

        let mut status = 0i32;
        libc::waitpid(pid, &mut status, 0);

        if libc::WIFEXITED(status) {
            let code = libc::WEXITSTATUS(status);
            let output = std::fs::read_to_string(&out_str).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&out_str);
            if code == 0 || output.contains("6d61696e") || output.contains("636b6579") {
                Ok(output)
            } else {
                Err(format!("db5.3_dump exited with status {code}"))
            }
        } else {
            let _ = std::fs::remove_file(&out_str);
            Err("db5.3_dump was killed".into())
        }
    }
}

/// Parse the output of `db5.3_dump -r`.
///
/// Format:
/// - Line 1: subdatabase name (hex-encoded)
/// - Line 2: record count header (hex-encoded)
/// - Then alternating key/value lines (hex-encoded)
fn parse_dbdump_output(output: &str) -> Result<Vec<RawRecord>> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 4 {
        return Ok(Vec::new());
    }

    let data_lines: Vec<&str> = lines[2..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    let mut records = Vec::new();
    let mut i = 0;
    while i + 1 < data_lines.len() {
        let key_hex = data_lines[i].trim();
        let val_hex = data_lines[i + 1].trim();

        if let (Ok(key_data), Ok(value_data)) = (hex_decode(key_hex), hex_decode(val_hex)) {
            if !key_data.is_empty() {
                records.push(RawRecord {
                    key_data,
                    value_data,
                });
            }
        }

        i += 2;
    }

    Ok(records)
}

fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim();
    if !hex.len().is_multiple_of(2) {
        return Err(MigrateError::Other(format!(
            "odd-length hex string: {}",
            hex.len()
        )));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| {
            MigrateError::Other(format!("invalid hex at position {i}: {}", &hex[i..i + 2]))
        })?;
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
    fn test_parse_dbdump_output() {
        let output = " 6d61696e\n 00000002\n 046d6b657901000000\n 301234567890abcdef\n";
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
        let records = parse_dbdump_output("").unwrap();
        assert!(records.is_empty());
    }
}
