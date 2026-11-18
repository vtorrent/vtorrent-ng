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
    if ut_vtr_id < u8::MAX {
        m.insert(
            b"ut_vtr_receipt".to_vec(),
            Value::Int(i64::from(ut_vtr_id) + 1),
        );
    }
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"m".to_vec(), Value::Dict(m));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Encode a signed bandwidth receipt for an `ut_vtr_receipt` extension message.
pub fn encode_receipt(receipt: &vtorrent_core::receipt::SignedBandwidthReceipt) -> Vec<u8> {
    bincode::serialize(receipt).unwrap_or_default()
}

/// Decode a signed bandwidth receipt from an `ut_vtr_receipt` payload.
pub fn decode_receipt(payload: &[u8]) -> Result<vtorrent_core::receipt::SignedBandwidthReceipt> {
    const MAX_RECEIPT_PAYLOAD: usize = 4096;
    if payload.len() > MAX_RECEIPT_PAYLOAD {
        return Err(TorrentError::PeerWireError(
            "ut_vtr_receipt payload is too large".into(),
        ));
    }
    bincode::deserialize(payload)
        .map_err(|_| TorrentError::PeerWireError("invalid ut_vtr_receipt payload".into()))
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
    // This value becomes a per-peer map key and a payment recipient.
    if payload.len() > 128 {
        return Err(TorrentError::PeerWireError(
            "ut_vtr address payload is too large".into(),
        ));
    }
    let value: Value = crate::bencode_guard::parse_untrusted(payload)
        .ok_or_else(|| TorrentError::PeerWireError("bencode nesting too deep".into()))?
        .map_err(TorrentError::PeerWireError)?;
    match value {
        Value::Bytes(b) => {
            let address = String::from_utf8(b)
                .map_err(|_| TorrentError::PeerWireError("ut_vtr is not UTF-8".into()))?;
            vtorrent_core::address::validate_p2pkh(&address).map_err(|error| {
                TorrentError::PeerWireError(format!("invalid ut_vtr address: {error}"))
            })?;
            Ok(address)
        }
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
    let value: Value = crate::bencode_guard::parse_untrusted(dict_bytes)
        .ok_or_else(|| TorrentError::PeerWireError("bencode nesting too deep".into()))?
        .map_err(TorrentError::PeerWireError)?;
    let dict = match value {
        Value::Dict(d) => d,
        _ => return Err(TorrentError::PeerWireError("ut_metadata not a dict".into())),
    };
    let piece = match dict.get(b"piece".as_slice()) {
        Some(Value::Int(i)) if *i >= 0 && *i <= u32::MAX as i64 => *i as u32,
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
                assert_eq!(m.get(b"ut_vtr_receipt".as_slice()), Some(&Value::Int(3)));
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

    #[test]
    fn test_ut_vtr_rejects_invalid_or_oversized_addresses() {
        let invalid = build_ut_vtr_address("not-an-address");
        assert!(parse_ut_vtr_address(&invalid).is_err());
        let oversized = build_ut_vtr_address(&"V".repeat(129));
        assert!(parse_ut_vtr_address(&oversized).is_err());
    }

    #[test]
    fn receipt_payload_roundtrip() {
        let receipt = vtorrent_core::receipt::SignedBandwidthReceipt {
            receipt: vtorrent_core::receipt::BandwidthReceipt {
                info_hash: [1u8; 20],
                window_start: 0,
                window_end: 300,
                uploaded_by_me: 1024,
                downloaded_by_me: 2048,
                peer: "VPeer".into(),
                signer: "VSigner".into(),
                settlement_id: 9,
                epoch: 7,
            },
            pubkey: vec![2u8; 33],
            signature: vec![3u8; 70],
        };
        let encoded = encode_receipt(&receipt);
        assert_eq!(decode_receipt(&encoded).unwrap(), receipt);
    }
}
