use crate::error::{Result, TorrentError};
use serde::{Deserialize, Serialize};

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

/// Whether outbound tracker requests may target private, loopback, link-local,
/// or otherwise non-public addresses.
///
/// Disabled by default: tracker URLs come from untrusted `.torrent` files and
/// magnet links, so an unguarded fetch is an SSRF primitive that can reach
/// cloud metadata endpoints and internal services. Tests and local-only
/// deployments can opt in explicitly.
static ALLOW_PRIVATE_TRACKER_TARGETS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Allow tracker announces to private/loopback/link-local addresses.
///
/// Intended for tests and explicitly local deployments. Never enable this on a
/// node that accepts untrusted torrents.
pub fn set_allow_private_tracker_targets(allow: bool) {
    ALLOW_PRIVATE_TRACKER_TARGETS.store(allow, std::sync::atomic::Ordering::SeqCst);
}

fn private_targets_allowed() -> bool {
    ALLOW_PRIVATE_TRACKER_TARGETS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Whether a tracker target address is permitted, honouring the
/// `set_allow_private_tracker_targets` override. Used by the UDP path, which
/// resolves its own address rather than going through `validate_tracker_url`.
pub fn tracker_target_allowed(ip: std::net::IpAddr) -> bool {
    private_targets_allowed() || !is_non_public(ip)
}

/// Validate a tracker URL before fetching it.
///
/// Rejects non-HTTP(S) schemes and, unless explicitly allowed, any literal host
/// that is a non-public address. Hostname resolution is **not** done here: it
/// must be async to avoid blocking the executor, and its result must be pinned
/// into the request client to close the DNS-rebinding TOCTOU. Use
/// [`resolve_tracker_url`] for the full check.
pub fn validate_tracker_url(raw: &str) -> Result<()> {
    validate_tracker_url_with(raw, private_targets_allowed())
}

/// Policy-explicit form of [`validate_tracker_url`], for callers (and tests)
/// that must not depend on the process-wide override.
pub fn validate_tracker_url_with(raw: &str, allow_private: bool) -> Result<()> {
    let parsed = reqwest::Url::parse(raw)
        .map_err(|e| TorrentError::TrackerError(format!("Invalid tracker URL: {}", e)))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(TorrentError::TrackerError(format!(
                "Unsupported tracker scheme: {}",
                other
            )))
        }
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| TorrentError::TrackerError("Tracker URL has no host".into()))?;
    if host.is_empty() {
        return Err(TorrentError::TrackerError("Tracker URL has no host".into()));
    }
    if allow_private {
        return Ok(());
    }
    // A literal IP can be checked directly; a hostname is resolved (and pinned)
    // by `resolve_tracker_url`.
    // `host_str()` keeps the brackets around IPv6 literals, so strip them
    // before parsing.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<std::net::IpAddr>() {
        if is_non_public(ip) {
            return Err(TorrentError::TrackerError(format!(
                "Tracker host {} is not a public address",
                host
            )));
        }
    }
    Ok(())
}

/// Resolve a tracker URL and return its host plus the validated public socket
/// addresses to pin the connection to.
///
/// Uses `tokio::net::lookup_host` so resolution does not block the executor
/// (T13). The returned addresses must be pinned via
/// `ClientBuilder::resolve_to_addrs`; otherwise `reqwest` re-resolves on
/// connect and a low-TTL hostname can pass the check as public, then resolve
/// private (DNS rebinding).
///
/// Returns an empty address list for a literal-IP host (nothing to pin).
pub async fn resolve_tracker_url(raw: &str) -> Result<(String, Vec<std::net::SocketAddr>)> {
    validate_tracker_url(raw)?;
    let parsed = reqwest::Url::parse(raw)
        .map_err(|e| TorrentError::TrackerError(format!("Invalid tracker URL: {}", e)))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| TorrentError::TrackerError("Tracker URL has no host".into()))?
        .to_string();
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if literal.parse::<std::net::IpAddr>().is_ok() {
        // Literal IP: already validated, nothing to resolve or pin.
        return Ok((host, Vec::new()));
    }
    if private_targets_allowed() {
        // Private targets are explicitly permitted; still resolve so the
        // caller can pin, but skip the public-address check.
        let port = parsed.port_or_known_default().unwrap_or(80);
        let addrs: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| {
                TorrentError::TrackerError(format!("Cannot resolve tracker host {}: {}", host, e))
            })?
            .collect();
        return Ok((host, addrs));
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| {
            TorrentError::TrackerError(format!("Cannot resolve tracker host {}: {}", host, e))
        })?
        .collect();
    if addrs.is_empty() {
        return Err(TorrentError::TrackerError(format!(
            "Tracker host {} did not resolve",
            host
        )));
    }
    for addr in &addrs {
        if is_non_public(addr.ip()) {
            return Err(TorrentError::TrackerError(format!(
                "Tracker host {} resolves to non-public address {}",
                host,
                addr.ip()
            )));
        }
    }
    Ok((host, addrs))
}

/// Whether an address is loopback, private, link-local, unspecified,
/// multicast, or otherwise not a routable public unicast address.
fn is_non_public(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                // 100.64.0.0/10 carrier-grade NAT.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 192.0.0.0/24 IETF protocol assignments.
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
                // 198.18.0.0/15 benchmarking.
                || (v4.octets()[0] == 198 && (v4.octets()[1] == 18 || v4.octets()[1] == 19))
                // 240.0.0.0/4 reserved.
                || v4.octets()[0] >= 240
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local addresses fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: re-check the embedded v4 address.
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_non_public(std::net::IpAddr::V4(v4)))
        }
    }
}

impl HttpTracker {
    pub fn new() -> crate::error::Result<Self> {
        Ok(HttpTracker {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("vTorrent-NG/2.0")
                // Do not follow redirects: a public tracker could redirect to
                // an internal address, bypassing the pre-flight check.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| crate::error::TorrentError::Io(e.to_string()))?,
        })
    }

    /// Send an announce request to an HTTP tracker.
    pub async fn announce(&self, req: &AnnounceRequest) -> Result<AnnounceResponse> {
        // Resolve the tracker host asynchronously and pin the validated public
        // addresses into the client. Without pinning, `reqwest` re-resolves on
        // connect and a low-TTL hostname can pass the check as public, then
        // resolve private (DNS rebinding, T13).
        let (host, pinned) = resolve_tracker_url(&req.tracker_url).await?;
        let client = if pinned.is_empty() {
            // Literal IP: already validated, nothing to pin.
            self.client.clone()
        } else {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("vTorrent-NG/2.0")
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&host, &pinned)
                .build()
                .map_err(|e| TorrentError::TrackerError(e.to_string()))?
        };

        // Build the URL with query parameters. The tracker URL may already
        // carry a query string (e.g. passkey trackers: ".../a?passkey=X"),
        // so join with '?' only when absent, otherwise '&'.
        let info_hash_encoded = url_encode_bytes(&req.info_hash);
        let peer_id_encoded = url_encode_bytes(&req.peer_id);

        let base = if req.tracker_url.contains('?') {
            format!("{}&", req.tracker_url)
        } else {
            format!("{}?", req.tracker_url)
        };

        let mut url = format!(
            "{}info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&compact=1&numwant={}",
            base,
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

        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        if !response.status().is_success() {
            return Err(TorrentError::TrackerError(format!(
                "HTTP {}",
                response.status()
            )));
        }

        // Bound the response: a malicious tracker could return gigabytes.
        // Legitimate announce responses are a few KB (peer lists).
        const MAX_TRACKER_RESPONSE: u64 = 4 * 1024 * 1024;
        if let Some(len) = response.content_length() {
            if len > MAX_TRACKER_RESPONSE {
                return Err(TorrentError::TrackerError(format!(
                    "Tracker response too large: {} bytes",
                    len
                )));
            }
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        if bytes.len() as u64 > MAX_TRACKER_RESPONSE {
            return Err(TorrentError::TrackerError(format!(
                "Tracker response too large: {} bytes",
                bytes.len()
            )));
        }

        parse_tracker_response(&bytes)
    }
}

impl Default for HttpTracker {
    fn default() -> Self {
        Self::new().expect("Failed to build HTTP client")
    }
}

/// Parse a bencoded tracker response.
fn parse_tracker_response(data: &[u8]) -> Result<AnnounceResponse> {
    let value: serde_bencode::value::Value = crate::bencode_guard::parse_untrusted(data)
        .ok_or_else(|| TorrentError::BencodeError("bencode nesting too deep".into()))?
        .map_err(TorrentError::BencodeError)?;

    let dict = match &value {
        serde_bencode::value::Value::Dict(d) => d,
        _ => return Err(TorrentError::TrackerError("Response is not a dict".into())),
    };

    // Check for failure reason
    if let Some(serde_bencode::value::Value::Bytes(reason)) = dict.get(&b"failure reason".to_vec())
    {
        return Err(TorrentError::TrackerError(
            String::from_utf8_lossy(reason).into_owned(),
        ));
    }

    let interval = match dict.get(&b"interval".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) if *i > 0 => *i as u32,
        _ => 1800, // Default 30 minutes
    };

    let min_interval = match dict.get(&b"min interval".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) if *i > 0 => Some(*i as u32),
        _ => None,
    };

    let complete = match dict.get(&b"complete".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) if *i >= 0 => *i as u32,
        _ => 0,
    };

    let incomplete = match dict.get(&b"incomplete".to_vec()) {
        Some(serde_bencode::value::Value::Int(i)) if *i >= 0 => *i as u32,
        _ => 0,
    };

    let warning = match dict.get(&b"warning message".to_vec()) {
        Some(serde_bencode::value::Value::Bytes(b)) => String::from_utf8(b.clone()).ok(),
        _ => None,
    };

    // Parse peers — compact format (6 bytes per peer: 4 IP + 2 port)
    let peers = match dict.get(&b"peers".to_vec()) {
        Some(serde_bencode::value::Value::Bytes(compact)) => parse_compact_peers(compact),
        Some(serde_bencode::value::Value::List(list)) => parse_dict_peers(list),
        _ => Vec::new(),
    };

    Ok(AnnounceResponse {
        interval,
        min_interval,
        complete,
        incomplete,
        peers,
        warning,
    })
}

/// Parse compact peer format (4 bytes IP + 2 bytes port per peer).
fn parse_compact_peers(data: &[u8]) -> Vec<TrackerPeer> {
    let mut peers = Vec::new();
    let mut i = 0;
    while i + 6 <= data.len() {
        let ip = format!(
            "{}.{}.{}.{}",
            data[i],
            data[i + 1],
            data[i + 2],
            data[i + 3]
        );
        let port = u16::from_be_bytes([data[i + 4], data[i + 5]]);
        peers.push(TrackerPeer {
            ip,
            port,
            peer_id: None,
        });
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
                Some(serde_bencode::value::Value::Int(p)) if *p > 0 && *p <= u16::MAX as i64 => {
                    *p as u16
                }
                _ => continue,
            };
            peers.push(TrackerPeer {
                ip,
                port,
                peer_id: None,
            });
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

    #[test]
    fn rejects_non_http_schemes() {
        assert!(validate_tracker_url("ftp://tracker.example.com/announce").is_err());
        assert!(validate_tracker_url("file:///etc/passwd").is_err());
        assert!(validate_tracker_url("gopher://tracker.example.com/").is_err());
    }

    #[test]
    fn rejects_internal_literal_addresses() {
        // The SSRF cases that matter: loopback, RFC1918, link-local (cloud
        // metadata), CGNAT, and IPv6 equivalents.
        for url in [
            "http://127.0.0.1/announce",
            "http://10.0.0.1/announce",
            "http://192.168.1.1/announce",
            "http://172.16.0.1/announce",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/announce",
            "http://0.0.0.0/announce",
            "http://[::1]/announce",
            "http://[fe80::1]/announce",
            "http://[fc00::1]/announce",
            "http://[::ffff:127.0.0.1]/announce",
        ] {
            assert!(
                validate_tracker_url(url).is_err(),
                "{url} must be rejected as a non-public tracker target"
            );
        }
    }

    #[test]
    fn allows_public_literal_addresses() {
        assert!(validate_tracker_url("http://93.184.216.34/announce").is_ok());
        assert!(validate_tracker_url("https://93.184.216.34/announce").is_ok());
        assert!(
            validate_tracker_url("http://[2606:2800:220:1:248:1893:25c8:1946]/announce").is_ok()
        );
    }

    #[test]
    fn explicit_override_permits_private_targets() {
        // Uses the policy-explicit form so this test does not depend on (or
        // mutate) the process-wide flag, which other tests assert is off.
        assert!(validate_tracker_url_with("http://127.0.0.1/announce", true).is_ok());
        assert!(validate_tracker_url_with("http://127.0.0.1/announce", false).is_err());
    }

    #[test]
    fn rejects_url_without_host() {
        // `http://` has no host at all and fails URL parsing.
        assert!(validate_tracker_url("http://").is_err());
    }

    #[tokio::test]
    async fn resolve_rejects_unresolvable_host() {
        // `http:///announce` parses with host "announce"; it is rejected when
        // resolution fails, which now happens in the async resolver.
        assert!(resolve_tracker_url("http:///announce").await.is_err());
    }

    #[tokio::test]
    async fn resolve_tracker_url_returns_pinned_public_addrs() {
        // A literal public IP needs no pinning but must resolve cleanly.
        let (host, pinned) = resolve_tracker_url("http://93.184.216.34/announce")
            .await
            .expect("public literal must resolve");
        assert_eq!(host, "93.184.216.34");
        assert!(pinned.is_empty(), "literal IP has nothing to pin");

        // A literal private IP is rejected before any resolution.
        assert!(resolve_tracker_url("http://127.0.0.1/announce")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn resolve_tracker_url_rejects_non_http_schemes() {
        assert!(resolve_tracker_url("ftp://tracker.example.com/announce")
            .await
            .is_err());
        assert!(resolve_tracker_url("file:///etc/passwd").await.is_err());
    }
}
