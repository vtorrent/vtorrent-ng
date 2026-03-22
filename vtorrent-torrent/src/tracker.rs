use serde::{Deserialize, Serialize};
use crate::error::{Result, TorrentError};

/// Event type for tracker announces (BEP-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnounceEvent {
    /// First announce when starting a download.
    Started,
    /// Announce when download completes.
    Completed,
    /// Announce when stopping the client.
    Stopped,
    /// Regular interval announce (no event parameter sent).
    None,
}

impl AnnounceEvent {
    pub fn as_str(&self) -> Option<&'static str> {
        match self {
            AnnounceEvent::Started => Some("started"),
            AnnounceEvent::Completed => Some("completed"),
            AnnounceEvent::Stopped => Some("stopped"),
            AnnounceEvent::None => None,
        }
    }
}

/// Parameters for a tracker announce request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnounceRequest {
    /// Tracker URL.
    pub tracker_url: String,
    /// 20-byte info hash.
    pub info_hash: [u8; 20],
    /// 20-byte peer ID.
    pub peer_id: [u8; 20],
    /// Port the client is listening on.
    pub port: u16,
    /// Total bytes uploaded this session.
    pub uploaded: u64,
    /// Total bytes downloaded this session.
    pub downloaded: u64,
    /// Bytes remaining to download.
    pub left: u64,
    /// Announce event.
    pub event: AnnounceEvent,
    /// Number of peers to request.
    pub num_want: i32,
}

/// A peer returned by the tracker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackerPeer {
    pub ip: String,
    pub port: u16,
    /// Optional peer ID (not always provided in compact mode).
    pub peer_id: Option<[u8; 20]>,
}

/// Response from a tracker announce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnounceResponse {
    /// Seconds until the next announce.
    pub interval: u32,
    /// Minimum interval (optional).
    pub min_interval: Option<u32>,
    /// Number of seeders.
    pub complete: u32,
    /// Number of leechers.
    pub incomplete: u32,
    /// List of peers.
    pub peers: Vec<TrackerPeer>,
    /// Tracker warning message (optional).
    pub warning: Option<String>,
}

/// HTTP tracker client.
pub struct HttpTracker {
    client: reqwest::Client,
}

impl HttpTracker {
    pub fn new() -> Self {
        HttpTracker {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("vTorrent-NG/2.0")
                .build()
                .expect("Failed to build HTTP client"),
        }
    }

    /// Send an announce request to an HTTP tracker.
    pub async fn announce(&self, req: &AnnounceRequest) -> Result<AnnounceResponse> {
        // Build the URL with query parameters
        let info_hash_encoded = url_encode_bytes(&req.info_hash);
        let peer_id_encoded = url_encode_bytes(&req.peer_id);

        let mut url = format!(
            "{}?info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&compact=1&numwant={}",
            req.tracker_url,
            info_hash_encoded,
            peer_id_encoded,
            req.port,
            req.uploaded,
            req.downloaded,
            req.left,
            req.num_want,
        );

        if let Some(event) = req.event.as_str() {
            url.push_str(&format!("&event={}", event));
        }

        let response = self.client.get(&url)
            .send()
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(TorrentError::TrackerError(
                format!("HTTP {}", response.status())
            ));
        }

        let bytes = response.bytes()
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        parse_tracker_response(&bytes)
    }
}

impl Default for HttpTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a bencoded tracker response.
fn parse_tracker_response(data: &[u8]) -> Result<AnnounceResponse> {
    let value: serde_bencode::value::Value = serde_bencode::from_bytes(data)
        .map_err(|e| TorrentError::BencodeError(e.to_string()))?;

    let dict = match &value {
        serde_bencode::value::Value::Dict(d) => d,
        _ => return Err(TorrentError::TrackerError("Response is not a dict".into())),
    };

    // Check for failure reason
    if let Some(serde_bencode::value::Value::Bytes(reason)) = dict.get(&b"failure reason".to_vec()) {
        return Err(TorrentError::TrackerError(
            String::from_utf8_lossy(reason).into_owned()
        ));
    }

    let interval = match dict.get(&b"interval".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) => *i as u32,
        _ => 1800, // Default 30 minutes
    };

    let min_interval = match dict.get(&b"min interval".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) => Some(*i as u32),
        _ => None,
    };

    let complete = match dict.get(&b"complete".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) => *i as u32,
        _ => 0,
    };

    let incomplete = match dict.get(&b"incomplete".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) => *i as u32,
        _ => 0,
    };

    let warning = match dict.get(&b"warning message".to_vec()) {
        Some(serde_bencode::value::Value::Bytes(b)) => String::from_utf8(b.clone()).ok(),
        _ => None,
    };

    // Parse peers — compact format (6 bytes per peer: 4 IP + 2 port)
    let peers = match dict.get(&b"peers".to_vec()) {
        Some(serde_bencode::value::Value::Bytes(compact)) => {
            parse_compact_peers(compact)
        }
        Some(serde_bencode::value::Value::List(list)) => {
            parse_dict_peers(list)
        }
        _ => Vec::new(),
    };

    Ok(AnnounceResponse { interval, min_interval, complete, incomplete, peers, warning })
}

/// Parse compact peer format (4 bytes IP + 2 bytes port per peer).
fn parse_compact_peers(data: &[u8]) -> Vec<TrackerPeer> {
    let mut peers = Vec::new();
    let mut i = 0;
    while i + 6 <= data.len() {
        let ip = format!("{}.{}.{}.{}", data[i], data[i+1], data[i+2], data[i+3]);
        let port = u16::from_be_bytes([data[i+4], data[i+5]]);
        peers.push(TrackerPeer { ip, port, peer_id: None });
        i += 6;
    }
    peers
}

/// Parse dictionary peer format (BEP-3 non-compact).
fn parse_dict_peers(list: &[serde_bencode::value::Value]) -> Vec<TrackerPeer> {
    let mut peers = Vec::new();
    for item in list {
        if let serde_bencode::value::Value::Dict(d) = item {
            let ip = match d.get(&b"ip".to_vec()) {
                Some(serde_bencode::value::Value::Bytes(b)) => {
                    String::from_utf8(b.clone()).unwrap_or_default()
                }
                _ => continue,
            };
            let port = match d.get(&b"port".to_vec()) {
                Some(serde_bencode::value::Value::Int(p)) => *p as u16,
                _ => continue,
            };
            peers.push(TrackerPeer { ip, port, peer_id: None });
        }
    }
    peers
}

/// URL-encode a byte array (each byte as %XX).
fn url_encode_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{:02X}", b));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_peer_parse() {
        // 192.168.1.1:6881 and 10.0.0.1:6882
        let data = [192u8, 168, 1, 1, 0x1A, 0xE1, 10, 0, 0, 1, 0x1A, 0xE2];
        let peers = parse_compact_peers(&data);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].ip, "192.168.1.1");
        assert_eq!(peers[0].port, 6881);
        assert_eq!(peers[1].ip, "10.0.0.1");
        assert_eq!(peers[1].port, 6882);
    }

    #[test]
    fn test_announce_event_str() {
        assert_eq!(AnnounceEvent::Started.as_str(), Some("started"));
        assert_eq!(AnnounceEvent::Completed.as_str(), Some("completed"));
        assert_eq!(AnnounceEvent::Stopped.as_str(), Some("stopped"));
        assert_eq!(AnnounceEvent::None.as_str(), None);
    }

    #[test]
    fn test_url_encode_bytes() {
        let bytes = [0x00u8, 0xFF, 0x41]; // 0x41 = 'A'
        let encoded = url_encode_bytes(&bytes);
        assert_eq!(encoded, "%00%FFA");
    }
}
