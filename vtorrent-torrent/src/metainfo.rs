use crate::error::{Result, TorrentError};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

/// A parsed .torrent file (BEP-3 metainfo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metainfo {
    /// The 20-byte SHA1 info hash identifying this torrent.
    pub info_hash: [u8; 20],
    /// Human-readable name of the torrent.
    pub name: String,
    /// Total size in bytes.
    pub total_size: u64,
    /// Piece length in bytes (typically 256KB–1MB).
    pub piece_length: u64,
    /// Number of pieces.
    pub piece_count: u32,
    /// The 20-byte SHA1 hash of each piece (BEP-3 `pieces` string).
    pub pieces: Vec<[u8; 20]>,
    /// Announce URL of the primary tracker.
    pub announce: Option<String>,
    /// List of tracker tiers (BEP-12 announce-list).
    pub announce_list: Vec<Vec<String>>,
    /// Files in this torrent (single-file torrents have one entry).
    pub files: Vec<TorrentFile>,
    /// Creation date (Unix timestamp).
    pub created_at: Option<u64>,
    /// Comment field.
    pub comment: Option<String>,
    /// Whether this is a private torrent (BEP-27).
    pub is_private: bool,
}

/// A file within a torrent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentFile {
    /// Path components (e.g. ["dir", "file.txt"]).
    pub path: Vec<String>,
    /// File size in bytes.
    pub length: u64,
    /// MD5 sum (optional, rarely used).
    pub md5sum: Option<String>,
}

/// A parsed magnet link (BEP-9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagnetLink {
    /// The 20-byte info hash (from xt=urn:btih:...).
    pub info_hash: [u8; 20],
    /// Display name (dn= parameter).
    pub display_name: Option<String>,
    /// Tracker URLs (tr= parameters).
    pub trackers: Vec<String>,
    /// Total size hint (xl= parameter).
    pub size_hint: Option<u64>,
}

impl Metainfo {
    /// Parse a .torrent file from raw bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        // Parse the bencode with a nesting-depth pre-check: serde_bencode
        // recurses per level with no depth counter, so deeply nested input
        // would overflow the stack and abort the process.
        let value: serde_bencode::value::Value = crate::bencode_guard::parse_untrusted(data)
            .ok_or_else(|| TorrentError::BencodeError("bencode nesting too deep".into()))?
            .map_err(TorrentError::BencodeError)?;

        let dict = match &value {
            serde_bencode::value::Value::Dict(d) => d,
            _ => return Err(TorrentError::InvalidMetainfo("Root is not a dict".into())),
        };

        // Extract the info dict
        let info_key = b"info".to_vec();
        let info_val = dict
            .get(&info_key)
            .ok_or_else(|| TorrentError::InvalidMetainfo("Missing 'info' key".into()))?;

        // Compute info hash from the exact bytes of the info dict as they
        // appear in the file (BEP-3). Re-encoding the parsed value would
        // normalize non-canonical encodings and produce a wrong hash.
        let info_hash = {
            let span = find_info_span(data)?;
            let mut hasher = Sha1::new();
            hasher.update(&data[span]);
            let hash_result = hasher.finalize();
            let mut arr = [0u8; 20];
            arr.copy_from_slice(&hash_result);
            arr
        };

        // Parse the info dict
        let info_dict = match info_val {
            serde_bencode::value::Value::Dict(d) => d,
            _ => return Err(TorrentError::InvalidMetainfo("'info' is not a dict".into())),
        };

        let name = get_string(info_dict, b"name")?;
        let piece_length = get_u64(info_dict, b"piece length")?;
        if piece_length == 0 {
            return Err(TorrentError::InvalidMetainfo(
                "'piece length' cannot be zero".into(),
            ));
        }

        // Determine if single-file or multi-file
        let (files, total_size) = if info_dict.contains_key(&b"files".to_vec()) {
            // Multi-file torrent
            let files_val = info_dict
                .get(&b"files".to_vec())
                .ok_or_else(|| TorrentError::InvalidMetainfo("Missing 'files'".into()))?;
            let files_list = match files_val {
                serde_bencode::value::Value::List(l) => l,
                _ => {
                    return Err(TorrentError::InvalidMetainfo(
                        "'files' is not a list".into(),
                    ))
                }
            };
            let mut files = Vec::new();
            let mut total = 0u64;
            for f in files_list {
                let fd = match f {
                    serde_bencode::value::Value::Dict(d) => d,
                    _ => continue,
                };
                let length = get_u64(fd, b"length")?;
                total = total.saturating_add(length);
                let path = get_string_list(fd, b"path")?;
                files.push(TorrentFile {
                    path,
                    length,
                    md5sum: None,
                });
            }
            (files, total)
        } else {
            // Single-file torrent
            let length = get_u64(info_dict, b"length")?;
            let file = TorrentFile {
                path: vec![name.clone()],
                length,
                md5sum: None,
            };
            (vec![file], length)
        };

        let piece_count_u64 = total_size.div_ceil(piece_length);
        if piece_count_u64 > u32::MAX as u64 {
            return Err(TorrentError::InvalidMetainfo(
                "piece count exceeds u32 range".into(),
            ));
        }
        let piece_count = piece_count_u64 as u32;

        // Parse the piece hashes (BEP-3 `pieces` string: 20 bytes per piece).
        // A missing or malformed `pieces` key is an error: without it the
        // engine would request and silently discard every downloaded piece
        // forever (no hash to verify against).
        let pieces = match info_dict.get(&b"pieces".to_vec()) {
            Some(serde_bencode::value::Value::Bytes(b)) => {
                if b.len() % 20 != 0 {
                    return Err(TorrentError::InvalidMetainfo(
                        "pieces string length is not a multiple of 20".into(),
                    ));
                }
                let mut hashes = Vec::with_capacity(b.len() / 20);
                for chunk in b.chunks_exact(20) {
                    let mut h = [0u8; 20];
                    h.copy_from_slice(chunk);
                    hashes.push(h);
                }
                hashes
            }
            _ => return Err(TorrentError::InvalidMetainfo("missing 'pieces' key".into())),
        };

        // The piece-hash count must match the computed piece count, otherwise
        // some pieces can never be verified.
        if pieces.len() != piece_count as usize {
            return Err(TorrentError::InvalidMetainfo(format!(
                "piece count {} does not match pieces hash count {}",
                piece_count,
                pieces.len()
            )));
        }

        // Parse announce
        let announce = dict.get(&b"announce".to_vec()).and_then(|v| match v {
            serde_bencode::value::Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
            _ => None,
        });

        // Parse announce-list
        let announce_list = parse_announce_list(dict);

        // Parse creation date
        let created_at = dict.get(&b"creation date".to_vec()).and_then(|v| match v {
            serde_bencode::value::Value::Int(i) => Some(*i as u64),
            _ => None,
        });

        // Parse comment
        let comment = dict.get(&b"comment".to_vec()).and_then(|v| match v {
            serde_bencode::value::Value::Bytes(b) => String::from_utf8(b.clone()).ok(),
            _ => None,
        });

        // Parse private flag
        let is_private = info_dict
            .get(&b"private".to_vec())
            .and_then(|v| match v {
                serde_bencode::value::Value::Int(i) => Some(*i == 1),
                _ => None,
            })
            .unwrap_or(false);

        Ok(Metainfo {
            info_hash,
            name,
            total_size,
            piece_length,
            piece_count,
            pieces,
            announce,
            announce_list,
            files,
            created_at,
            comment,
            is_private,
        })
    }

    /// Return the info hash as a hex string.
    pub fn info_hash_hex(&self) -> String {
        hex::encode(self.info_hash)
    }

    /// Return the info hash as a URL-encoded string for tracker announces.
    pub fn info_hash_urlencoded(&self) -> String {
        let mut encoded = String::new();
        for byte in &self.info_hash {
            encoded.push('%');
            encoded.push_str(&format!("{:02X}", byte));
        }
        encoded
    }

    /// Create a minimal Metainfo from a MagnetLink.
    /// The torrent metadata (pieces, files) will be fetched via BEP-9 extension protocol.
    pub fn from_magnet_link(magnet: &MagnetLink) -> Self {
        Metainfo {
            info_hash: magnet.info_hash,
            name: magnet
                .display_name
                .clone()
                .unwrap_or_else(|| hex::encode(magnet.info_hash)),
            total_size: magnet.size_hint.unwrap_or(0),
            piece_length: 0,
            piece_count: 0,
            pieces: Vec::new(),
            announce: magnet.trackers.first().cloned(),
            announce_list: vec![magnet.trackers.clone()],
            files: Vec::new(),
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    /// Return all tracker URLs from announce and announce-list.
    pub fn all_trackers(&self) -> Vec<String> {
        let mut trackers = Vec::new();
        if let Some(ref url) = self.announce {
            trackers.push(url.clone());
        }
        for tier in &self.announce_list {
            for url in tier {
                if !trackers.contains(url) {
                    trackers.push(url.clone());
                }
            }
        }
        trackers
    }
}

impl MagnetLink {
    /// Parse a magnet URI (magnet:?xt=urn:btih:...&dn=...&tr=...).
    pub fn parse(uri: &str) -> Result<Self> {
        if !uri.starts_with("magnet:?") {
            return Err(TorrentError::MagnetError("Not a magnet URI".into()));
        }

        let query = &uri["magnet:?".len()..];
        let mut info_hash_bytes = None;
        let mut display_name = None;
        let mut trackers = Vec::new();
        let mut size_hint = None;

        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                let decoded = urlencoding::decode(value)
                    .map_err(|e| TorrentError::MagnetError(e.to_string()))?
                    .into_owned();
                match key {
                    "xt" => {
                        if let Some(hash_str) = decoded.strip_prefix("urn:btih:") {
                            let bytes = if hash_str.len() == 40 {
                                // Hex-encoded
                                hex::decode(hash_str)
                                    .map_err(|e| TorrentError::MagnetError(e.to_string()))?
                            } else if hash_str.len() == 32 {
                                // Base32-encoded
                                base32_decode(hash_str)?
                            } else {
                                return Err(TorrentError::MagnetError(format!(
                                    "Invalid info hash length: {}",
                                    hash_str.len()
                                )));
                            };
                            if bytes.len() != 20 {
                                return Err(TorrentError::MagnetError(
                                    "Info hash must be 20 bytes".into(),
                                ));
                            }
                            let mut arr = [0u8; 20];
                            arr.copy_from_slice(&bytes);
                            info_hash_bytes = Some(arr);
                        }
                    }
                    "dn" => display_name = Some(decoded),
                    "tr" => trackers.push(decoded),
                    "xl" => size_hint = decoded.parse().ok(),
                    _ => {}
                }
            }
        }

        let info_hash = info_hash_bytes
            .ok_or_else(|| TorrentError::MagnetError("Missing xt=urn:btih: parameter".into()))?;

        Ok(MagnetLink {
            info_hash,
            display_name,
            trackers,
            size_hint,
        })
    }

    /// Return the info hash as a hex string.
    pub fn info_hash_hex(&self) -> String {
        hex::encode(self.info_hash)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

type BencodeDict = std::collections::HashMap<Vec<u8>, serde_bencode::value::Value>;

fn get_string(dict: &BencodeDict, key: &[u8]) -> Result<String> {
    match dict.get(key) {
        Some(serde_bencode::value::Value::Bytes(b)) => String::from_utf8(b.clone()).map_err(|_| {
            TorrentError::InvalidMetainfo(format!("Non-UTF8 value for key {:?}", key))
        }),
        _ => Err(TorrentError::InvalidMetainfo(format!(
            "Missing or invalid key {:?}",
            key
        ))),
    }
}

fn get_integer(dict: &BencodeDict, key: &[u8]) -> Result<i64> {
    match dict.get(key) {
        Some(serde_bencode::value::Value::Int(i)) => Ok(*i),
        _ => Err(TorrentError::InvalidMetainfo(format!(
            "Missing or invalid integer key {:?}",
            key
        ))),
    }
}

/// Read a non-negative integer as `u64`, rejecting negative values.
///
/// A negative bencode integer would otherwise wrap to a huge `u64` via `as`,
/// driving unbounded allocations downstream.
fn get_u64(dict: &BencodeDict, key: &[u8]) -> Result<u64> {
    let i = get_integer(dict, key)?;
    if i < 0 {
        return Err(TorrentError::InvalidMetainfo(format!(
            "Integer key {:?} must be non-negative",
            key
        )));
    }
    Ok(i as u64)
}

fn get_string_list(dict: &BencodeDict, key: &[u8]) -> Result<Vec<String>> {
    match dict.get(key) {
        Some(serde_bencode::value::Value::List(list)) => list
            .iter()
            .map(|v| match v {
                serde_bencode::value::Value::Bytes(b) => String::from_utf8(b.clone())
                    .map_err(|_| TorrentError::InvalidMetainfo("Non-UTF8 path component".into())),
                _ => Err(TorrentError::InvalidMetainfo(
                    "Path component is not bytes".into(),
                )),
            })
            .collect(),
        _ => Err(TorrentError::InvalidMetainfo(format!(
            "Missing or invalid list key {:?}",
            key
        ))),
    }
}

fn parse_announce_list(dict: &BencodeDict) -> Vec<Vec<String>> {
    let mut result = Vec::new();
    if let Some(serde_bencode::value::Value::List(tiers)) = dict.get(&b"announce-list".to_vec()) {
        for tier in tiers {
            if let serde_bencode::value::Value::List(urls) = tier {
                let tier_urls: Vec<String> = urls
                    .iter()
                    .filter_map(|u| {
                        if let serde_bencode::value::Value::Bytes(b) = u {
                            String::from_utf8(b.clone()).ok()
                        } else {
                            None
                        }
                    })
                    .collect();
                if !tier_urls.is_empty() {
                    result.push(tier_urls);
                }
            }
        }
    }
    result
}

/// Locate the raw byte span of the top-level `info` dictionary value.
///
/// Walks the top-level dict of a bencoded metainfo file and returns the
/// byte range of the value for the `info` key, so the info hash can be
/// computed over the exact bytes as they appear in the file (BEP-3).
fn find_info_span(data: &[u8]) -> Result<std::ops::Range<usize>> {
    let mut pos = 0usize;
    if data.get(pos) != Some(&b'd') {
        return Err(TorrentError::InvalidMetainfo("Root is not a dict".into()));
    }
    pos += 1;
    let mut info_span: Option<std::ops::Range<usize>> = None;
    loop {
        match data.get(pos) {
            Some(b'e') => break,
            None => {
                return Err(TorrentError::InvalidMetainfo(
                    "Truncated top-level dict".into(),
                ))
            }
            Some(_) => {
                let key_start = pos;
                skip_string(data, &mut pos)?;
                let val_start = pos;
                skip_value(data, &mut pos)?;
                let key = &data[key_start..val_start];
                let colon = key
                    .iter()
                    .position(|&b| b == b':')
                    .ok_or_else(|| TorrentError::InvalidMetainfo("Invalid dict key".into()))?;
                if &key[colon + 1..] == b"info" {
                    // Duplicate top-level "info" keys are forbidden: the
                    // info-hash is computed over the first occurrence while
                    // field parsing (HashMap insert semantics) would use the
                    // last — a UI-deception / content-identity mismatch.
                    if info_span.is_some() {
                        return Err(TorrentError::InvalidMetainfo("duplicate 'info' key".into()));
                    }
                    info_span = Some(val_start..pos);
                }
            }
        }
    }
    info_span.ok_or_else(|| TorrentError::InvalidMetainfo("Missing 'info' key".into()))
}

/// Skip a length-prefixed bencode string, advancing `pos` past it.
fn skip_string(data: &[u8], pos: &mut usize) -> Result<()> {
    let colon = data[*pos..]
        .iter()
        .position(|&b| b == b':')
        .ok_or_else(|| TorrentError::InvalidMetainfo("Unterminated string length".into()))?;
    let len_str = std::str::from_utf8(&data[*pos..*pos + colon])
        .map_err(|_| TorrentError::InvalidMetainfo("Invalid string length".into()))?;
    let len: usize = len_str
        .parse()
        .map_err(|_| TorrentError::InvalidMetainfo("Invalid string length".into()))?;
    *pos += colon + 1;
    let end = (*pos)
        .checked_add(len)
        .ok_or_else(|| TorrentError::InvalidMetainfo("String length overflow".into()))?;
    if end > data.len() {
        return Err(TorrentError::InvalidMetainfo(
            "String exceeds input length".into(),
        ));
    }
    *pos = end;
    Ok(())
}

/// Skip any bencode value (int, string, list, or dict), advancing `pos`.
fn skip_value(data: &[u8], pos: &mut usize) -> Result<()> {
    let Some(&c) = data.get(*pos) else {
        return Err(TorrentError::InvalidMetainfo("Truncated value".into()));
    };
    match c {
        b'i' => {
            let rel = data[*pos..]
                .iter()
                .position(|&b| b == b'e')
                .ok_or_else(|| TorrentError::InvalidMetainfo("Unterminated integer".into()))?;
            *pos += rel + 1;
            Ok(())
        }
        b'l' | b'd' => {
            *pos += 1;
            loop {
                match data.get(*pos) {
                    Some(b'e') => {
                        *pos += 1;
                        return Ok(());
                    }
                    None => {
                        return Err(TorrentError::InvalidMetainfo(
                            "Truncated list or dict".into(),
                        ))
                    }
                    Some(_) => {
                        if c == b'd' {
                            skip_string(data, pos)?;
                        }
                        skip_value(data, pos)?;
                    }
                }
            }
        }
        b'0'..=b'9' => skip_string(data, pos),
        _ => Err(TorrentError::InvalidMetainfo(
            "Invalid bencode value".into(),
        )),
    }
}

/// Decode a Base32-encoded string (RFC 4648, no padding).
fn base32_decode(s: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let s = s.to_uppercase();
    let mut bits: u64 = 0;
    let mut bit_count = 0;
    let mut output = Vec::new();

    for c in s.bytes() {
        let val = ALPHABET.iter().position(|&b| b == c).ok_or_else(|| {
            TorrentError::MagnetError(format!("Invalid base32 char: {}", c as char))
        })?;
        bits = (bits << 5) | val as u64;
        bit_count += 5;
        if bit_count >= 8 {
            bit_count -= 8;
            output.push((bits >> bit_count) as u8);
            bits &= (1 << bit_count) - 1;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_magnet_parse_hex() {
        let uri = "magnet:?xt=urn:btih:aabbccddaabbccddaabbccddaabbccddaabbccdd&dn=Test+Torrent&tr=udp%3A%2F%2Ftracker.example.com%3A6969";
        let m = MagnetLink::parse(uri).unwrap();
        assert_eq!(
            m.info_hash_hex(),
            "aabbccddaabbccddaabbccddaabbccddaabbccdd"
        );
        assert_eq!(m.display_name.as_deref(), Some("Test+Torrent"));
        assert_eq!(m.trackers.len(), 1);
    }

    #[test]
    fn test_magnet_missing_xt_fails() {
        let uri = "magnet:?dn=Test";
        assert!(MagnetLink::parse(uri).is_err());
    }

    #[test]
    fn test_magnet_not_magnet_uri_fails() {
        assert!(MagnetLink::parse("https://example.com").is_err());
    }

    #[test]
    fn test_info_hash_urlencoded() {
        let mut m = MagnetLink {
            info_hash: [0xAA; 20],
            display_name: None,
            trackers: vec![],
            size_hint: None,
        };
        // Manually set a known hash for testing
        m.info_hash[0] = 0x00;
        m.info_hash[1] = 0xFF;
        // The Metainfo urlencoded method
        let meta = Metainfo {
            info_hash: m.info_hash,
            name: "test".into(),
            total_size: 0,
            piece_length: 262144,
            piece_count: 0,
            pieces: Vec::new(),
            announce: None,
            announce_list: vec![],
            files: vec![],
            created_at: None,
            comment: None,
            is_private: false,
        };
        let encoded = meta.info_hash_urlencoded();
        assert!(encoded.starts_with("%00%FF"));
    }

    #[test]
    fn test_info_hash_hashes_raw_bytes() {
        // The info dict uses a non-canonical integer ("i01e"). BEP-3 requires
        // hashing the exact bytes as they appear in the file; re-encoding the
        // parsed value would normalize it to "i1e" and produce a wrong hash.
        let torrent: &[u8] =
            b"d4:infod6:lengthi01e4:name8:test.iso12:piece lengthi32768e6:pieces20:aaaaaaaaaaaaaaaaaaaaee";
        let meta = Metainfo::from_bytes(torrent).unwrap();
        assert_eq!(
            meta.info_hash_hex(),
            "8da789bdef65cc39dd28008f1e2017e5124ee9db"
        );
    }

    #[test]
    fn test_info_hash_multi_file() {
        let torrent: &[u8] = b"d8:announce26:http://tracker.example.com4:infod5:filesld6:lengthi40000e4:pathl3:dir9:file1.txteed6:lengthi60000e4:pathl3:dir9:file2.txteee6:lengthi100000e4:name8:test.dir12:piece lengthi32768e6:pieces80:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaee";
        let meta = Metainfo::from_bytes(torrent).unwrap();
        assert_eq!(meta.name, "test.dir");
        assert_eq!(meta.total_size, 100_000);
        assert_eq!(meta.files.len(), 2);
        assert_eq!(meta.files[0].path, vec!["dir", "file1.txt"]);
        assert_eq!(meta.files[1].path, vec!["dir", "file2.txt"]);
        assert_eq!(
            meta.info_hash_hex(),
            "0a1b23ec28ec4a24f8025733a17a31d2632887a1"
        );
    }

    #[test]
    fn test_info_hash_single_file() {
        let torrent: &[u8] =
            b"d8:announce26:http://tracker.example.com4:infod6:lengthi100000e4:name8:test.iso12:piece lengthi32768e6:pieces80:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaee";
        let meta = Metainfo::from_bytes(torrent).unwrap();
        assert_eq!(meta.name, "test.iso");
        assert_eq!(meta.total_size, 100_000);
        assert_eq!(meta.piece_length, 32_768);
        assert_eq!(meta.piece_count, 4);
        assert_eq!(
            meta.info_hash_hex(),
            "df08b2cf9cc869445ff01af89b070cbba66fecaf"
        );
    }

    #[test]
    fn test_piece_hashes_parsed() {
        // Build a minimal single-file torrent with 2 pieces of 4 bytes each.
        // pieces string = 2 * 20 bytes of SHA1 hashes.
        let mut pieces = Vec::new();
        pieces.extend_from_slice(&[0x11u8; 20]);
        pieces.extend_from_slice(&[0x22u8; 20]);

        // Construct the bencode bytes directly (the pieces value is raw bytes).
        let mut bencode = Vec::new();
        bencode.extend_from_slice(b"d4:infod6:lengthi8e4:name4:test12:piece lengthi4e6:pieces");
        bencode.extend_from_slice(format!("{}:", pieces.len()).as_bytes());
        bencode.extend_from_slice(&pieces);
        bencode.extend_from_slice(b"e8:announce15:http://tracker/e");

        let meta = Metainfo::from_bytes(&bencode).unwrap();
        assert_eq!(meta.piece_count, 2);
        assert_eq!(meta.pieces.len(), 2);
        assert_eq!(meta.pieces[0], [0x11u8; 20]);
        assert_eq!(meta.pieces[1], [0x22u8; 20]);
    }

    #[test]
    fn test_zero_piece_length_rejected() {
        // A malicious torrent with "piece length" = 0 must be rejected instead
        // of dividing by zero when computing the piece count.
        let mut pieces = Vec::new();
        pieces.extend_from_slice(&[0x11u8; 20]);

        let mut bencode = Vec::new();
        bencode.extend_from_slice(b"d4:infod6:lengthi8e4:name4:test12:piece lengthi0e6:pieces");
        bencode.extend_from_slice(format!("{}:", pieces.len()).as_bytes());
        bencode.extend_from_slice(&pieces);
        bencode.extend_from_slice(b"e8:announce15:http://tracker/e");

        let result = Metainfo::from_bytes(&bencode);
        assert!(result.is_err(), "zero piece length must be rejected");
    }

    #[test]
    fn test_negative_length_rejected() {
        // A malicious torrent with a negative "length" must be rejected instead
        // of wrapping to a huge u64 and driving an unbounded allocation.
        let mut pieces = Vec::new();
        pieces.extend_from_slice(&[0x11u8; 20]);

        let mut bencode = Vec::new();
        bencode.extend_from_slice(b"d4:infod6:lengthi-1e4:name4:test12:piece lengthi1e6:pieces");
        bencode.extend_from_slice(format!("{}:", pieces.len()).as_bytes());
        bencode.extend_from_slice(&pieces);
        bencode.extend_from_slice(b"e8:announce15:http://tracker/e");

        let result = Metainfo::from_bytes(&bencode);
        assert!(result.is_err(), "negative length must be rejected");
    }
}
