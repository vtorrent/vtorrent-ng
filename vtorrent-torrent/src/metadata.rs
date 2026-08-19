//! BEP-9 `ut_metadata` extension: fetch the info dict from peers.

use crate::error::{Result, TorrentError};
use serde_bencode::value::Value;

/// Build the extension handshake dict: `{ "m": { "ut_metadata": id }, "metadata_size": n }`.
pub fn build_extension_handshake(ut_metadata_id: u8, metadata_size: u64) -> Vec<u8> {
    let mut m = std::collections::HashMap::new();
    m.insert(b"ut_metadata".to_vec(), Value::Int(ut_metadata_id as i64));
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"m".to_vec(), Value::Dict(m));
    dict.insert(b"metadata_size".to_vec(), Value::Int(metadata_size as i64));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Build an extension handshake advertising `ut_vtr` at the given id.
pub fn build_ut_vtr_handshake(ut_vtr_id: u8) -> Vec<u8> {
    let mut m = std::collections::HashMap::new();
    m.insert(b"ut_vtr".to_vec(), Value::Int(ut_vtr_id as i64));
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"m".to_vec(), Value::Dict(m));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Build a `ut_vtr` address message payload: `<bencoded string>`.
///
/// The extension id is carried by the `PeerMessage::Extended` envelope, not
/// embedded in the payload.
pub fn build_ut_vtr_address(address: &str) -> Vec<u8> {
    serde_bencode::to_bytes(&Value::Bytes(address.as_bytes().to_vec())).unwrap_or_default()
}

/// Parse a `ut_vtr` address message payload (the bencoded string).
pub fn parse_ut_vtr_address(payload: &[u8]) -> Result<String> {
    let value: Value = serde_bencode::from_bytes(payload)
        .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
    match value {
        Value::Bytes(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
        _ => Err(TorrentError::PeerWireError("ut_vtr not a string".into())),
    }
}

/// Build a `ut_metadata` request payload: `{ "msg_type": 0, "piece": i }`.
///
/// The extension id is carried by the `PeerMessage::Extended` envelope, not
/// embedded in the payload.
pub fn build_request(piece: u32) -> Vec<u8> {
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"msg_type".to_vec(), Value::Int(0));
    dict.insert(b"piece".to_vec(), Value::Int(piece as i64));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Parse a `ut_metadata` data message payload, returning (piece_index, total_size, data).
///
/// The payload is: `<bencoded dict><piece bytes>` (the extension id is stripped
/// by the `PeerMessage::Extended` envelope).
pub fn parse_data(payload: &[u8]) -> Result<(u32, u64, Vec<u8>)> {
    let dict_end = find_dict_end(payload)
        .ok_or_else(|| TorrentError::PeerWireError("malformed ut_metadata".into()))?;
    let dict_bytes = &payload[..dict_end];
    let value: Value = serde_bencode::from_bytes(dict_bytes)
        .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
    let dict = match value {
        Value::Dict(d) => d,
        _ => return Err(TorrentError::PeerWireError("ut_metadata not a dict".into())),
    };
    let piece = match dict.get(b"piece".as_slice()) {
        Some(Value::Int(i)) if *i >= 0 => *i as u32,
        _ => return Err(TorrentError::PeerWireError("missing piece".into())),
    };
    let total_size = match dict.get(b"total_size".as_slice()) {
        Some(Value::Int(i)) if *i >= 0 => *i as u64,
        _ => return Err(TorrentError::PeerWireError("missing total_size".into())),
    };
    let data = payload[dict_end..].to_vec();
    Ok((piece, total_size, data))
}

/// Find the byte offset (exclusive) of the end of the top-level bencoded dict.
fn find_dict_end(bytes: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < bytes.len() {
        match bytes[i] {
            b'd' => {
                depth += 1;
                i += 1;
            }
            b'l' => {
                depth += 1;
                i += 1;
            }
            b'e' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'i' => {
                // Integer: skip to the terminating 'e'.
                i += 1;
                while i < bytes.len() && bytes[i] != b'e' {
                    i += 1;
                }
                i += 1; // skip the 'e'
            }
            b'0'..=b'9' => {
                // Byte string: <len>:<bytes>.
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j >= bytes.len() || bytes[j] != b':' {
                    return None;
                }
                let len: usize = std::str::from_utf8(&bytes[i..j]).ok()?.parse().ok()?;
                let next = j.checked_add(1)?.checked_add(len)?;
                if next > bytes.len() {
                    return None;
                }
                i = next;
            }
            _ => return None,
        }
    }
    None
}

/// Reassemble the info dict from pieces, in piece order.
pub fn reassemble_metadata(
    pieces: &std::collections::HashMap<u32, Vec<u8>>,
    total_size: u64,
) -> Result<Vec<u8>> {
    // Cap the allocation: total_size comes from a remote peer and must not be
    // trusted to allocate unbounded memory. A legitimate info dict is small
    // (a few MB at most).
    const MAX_METADATA_SIZE: u64 = 64 * 1024 * 1024;
    if total_size > MAX_METADATA_SIZE {
        return Err(TorrentError::PeerWireError(format!(
            "metadata size too large: {}",
            total_size
        )));
    }
    let mut indices: Vec<u32> = pieces.keys().copied().collect();
    indices.sort_unstable();
    let mut out = Vec::with_capacity(total_size as usize);
    for i in indices {
        out.extend_from_slice(&pieces[&i]);
    }
    if out.len() as u64 != total_size {
        return Err(TorrentError::PeerWireError(format!(
            "metadata size mismatch: expected {} got {}",
            total_size,
            out.len()
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_handshake_roundtrip() {
        let bytes = build_extension_handshake(3, 1024);
        let value: Value = serde_bencode::from_bytes(&bytes).unwrap();
        if let Value::Dict(d) = value {
            assert!(d.contains_key(b"m".as_slice()));
            assert!(d.contains_key(b"metadata_size".as_slice()));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_request_payload() {
        let bytes = build_request(0);
        let value: Value = serde_bencode::from_bytes(&bytes).unwrap();
        if let Value::Dict(d) = value {
            assert_eq!(d.get(b"msg_type".as_slice()), Some(&Value::Int(0)));
            assert_eq!(d.get(b"piece".as_slice()), Some(&Value::Int(0)));
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_parse_data() {
        let mut dict = std::collections::HashMap::new();
        dict.insert(b"msg_type".to_vec(), Value::Int(1));
        dict.insert(b"piece".to_vec(), Value::Int(0));
        dict.insert(b"total_size".to_vec(), Value::Int(4));
        let dict_bytes = serde_bencode::to_bytes(&Value::Dict(dict)).unwrap();
        let mut payload = dict_bytes;
        payload.extend_from_slice(b"test");

        let (piece, total, data) = parse_data(&payload).unwrap();
        assert_eq!(piece, 0);
        assert_eq!(total, 4);
        assert_eq!(data, b"test");
    }

    #[test]
    fn test_parse_data_rejects_overflowing_length() {
        // A malicious length prefix must not overflow the offset arithmetic.
        let payload = b"d3:piecei0e11:total_sizei4ee99999999999999999999:";
        assert!(parse_data(payload).is_err());
    }

    #[test]
    fn test_reassemble_metadata() {
        let info_dict = b"d4:name4:test12:piece lengthi4e6:pieces40:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa4:lengthi8ee";
        let mut pieces = std::collections::HashMap::new();
        pieces.insert(0u32, info_dict[..20].to_vec());
        pieces.insert(1u32, info_dict[20..].to_vec());
        let reassembled = reassemble_metadata(&pieces, info_dict.len() as u64).unwrap();
        assert_eq!(reassembled, info_dict);
    }

    #[test]
    fn test_ut_vtr_handshake_advertises() {
        let bytes = build_ut_vtr_handshake(2);
        let value: Value = serde_bencode::from_bytes(&bytes).unwrap();
        if let Value::Dict(d) = value {
            if let Some(Value::Dict(m)) = d.get(b"m".as_slice()) {
                assert_eq!(m.get(b"ut_vtr".as_slice()), Some(&Value::Int(2)));
            } else {
                panic!("missing m dict");
            }
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_ut_vtr_address_message_roundtrip() {
        let addr = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        let msg = build_ut_vtr_address(addr);
        let parsed = parse_ut_vtr_address(&msg).unwrap();
        assert_eq!(parsed, addr);
    }
}
