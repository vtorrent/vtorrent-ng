//! Bencode safety guards.
//!
//! `serde_bencode`'s deserializer recurses once per nesting level with no
//! depth counter, so a peer-supplied payload of deeply nested `d`/`l` bytes
//! (well under any message-size cap) overflows the thread stack and aborts
//! the whole process. Every untrusted bencode blob must be passed through
//! [`bencode_depth_ok`] before handing it to `serde_bencode`.

/// Maximum allowed bencode nesting depth for untrusted input.
///
/// Legitimate torrents and DHT/tracker/extension messages nest at most a
/// handful of levels; 32 is far above anything real while capping the
/// deserializer's recursion to a safe stack depth.
pub const MAX_BENCODE_DEPTH: usize = 32;

/// Walk raw bencode bytes iteratively and return whether the nesting depth
/// stays within `max_depth`. Malformed input returns `false` (serde_bencode
/// will produce the proper error when invoked, so we only need to veto on
/// excessive depth here — but early rejection of obvious garbage is cheap).
pub fn bencode_depth_ok(data: &[u8], max_depth: usize) -> bool {
    let mut depth: usize = 0;
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'd' | b'l' => {
                depth += 1;
                if depth > max_depth {
                    return false;
                }
                i += 1;
            }
            b'e' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                i += 1;
            }
            b'i' => {
                i += 1;
                // Skip to 'e'.
                match data[i..].iter().position(|&b| b == b'e') {
                    Some(p) => i += p + 1,
                    None => return false,
                }
            }
            b'0'..=b'9' => {
                // String length prefix: digits then ':' then that many bytes.
                let start = i;
                while i < data.len() && data[i].is_ascii_digit() {
                    i += 1;
                }
                if i >= data.len() || data[i] != b':' {
                    return false;
                }
                let len: usize = match std::str::from_utf8(&data[start..i])
                    .ok()
                    .and_then(|s| s.parse().ok())
                {
                    Some(l) => l,
                    None => return false,
                };
                i += 1; // ':'
                i = i.saturating_add(len);
                if i > data.len() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    // A complete bencode document must balance; truncated input fails.
    depth == 0
}

/// Parse untrusted bencode with a nesting-depth pre-check.
///
/// Returns `None` when the input exceeds [`MAX_BENCODE_DEPTH`] (or is
/// structurally malformed); otherwise the serde result.
pub fn parse_untrusted<T: serde::de::DeserializeOwned>(data: &[u8]) -> Option<Result<T, String>> {
    if !bencode_depth_ok(data, MAX_BENCODE_DEPTH) {
        return None;
    }
    Some(serde_bencode::from_bytes(data).map_err(|e| e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shallow_input_ok() {
        assert!(bencode_depth_ok(b"d4:infod6:lengthi5eee", 32));
    }

    #[test]
    fn deep_input_rejected() {
        let bomb = vec![b'd'; 10_000];
        assert!(!bencode_depth_ok(&bomb, MAX_BENCODE_DEPTH));
    }

    #[test]
    fn malformed_rejected() {
        assert!(!bencode_depth_ok(b"zzzz", 32));
        assert!(!bencode_depth_ok(b"d4:info", 32));
        assert!(!bencode_depth_ok(b"iee", 32));
    }

    #[test]
    fn string_lengths_walked() {
        assert!(bencode_depth_ok(b"5:hello", 32));
        assert!(!bencode_depth_ok(b"99:hi", 32));
    }

    #[test]
    fn parse_untrusted_rejects_depth_bomb() {
        let bomb = vec![b'd'; 100_000];
        assert!(parse_untrusted::<serde_bencode::value::Value>(&bomb).is_none());
    }

    #[test]
    fn parse_untrusted_accepts_normal() {
        let v: serde_bencode::value::Value = parse_untrusted(b"d1:a1:b1:cli7eee")
            .expect("normal dict parses")
            .expect("ok");
        match v {
            serde_bencode::value::Value::Dict(d) => {
                assert_eq!(d.len(), 2);
            }
            other => panic!("expected dict, got {:?}", other),
        }
    }
}
