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
        // Parse the bencode
        let value: serde_bencode::value::Value = serde_bencode::from_bytes(data)
            .map_err(|e| TorrentError::BencodeError(e.to_string()))?;

        let dict = match &value {
            serde_bencode::value::Value::Dict(d) => d,
            _ => return Err(TorrentError::InvalidMetainfo("Root is not a dict".into())),
        };

        // Extract the info dict
        let info_key = b"info".to_vec();
        let info_val = dict
            .get(&info_key)
            .ok_or_else(|| TorrentError::InvalidMetainfo("Missing 'info' key".into()))?;

        // Compute info hash by re-encoding the info dict
        let info_bytes = serde_bencode::to_bytes(info_val)
            .map_err(|e| TorrentError::BencodeError(e.to_string()))?;
        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);
        let hash_result = hasher.finalize();
        let mut info_hash = [0u8; 20];
        info_hash.copy_from_slice(&hash_result);

        // Parse the info dict
        let info_dict = match info_val {
            serde_bencode::value::Value::Dict(d) => d,
            _ => return Err(TorrentError::InvalidMetainfo("'info' is not a dict".into())),
        };

        let name = get_string(info_dict, b"name")?;
        let piece_length = get_integer(info_dict, b"piece length")? as u64;

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
                let length = get_integer(fd, b"length")? as u64;
                total += length;
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
            let length = get_integer(info_dict, b"length")? as u64;
            let file = TorrentFile {
                path: vec![name.clone()],
                length,
                md5sum: None,
            };
            (vec![file], length)
        };

        let piece_count = total_size.div_ceil(piece_length) as u32;

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
}
