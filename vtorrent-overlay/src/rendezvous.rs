/// Rendezvous layer — how two nodes exchange their overlay endpoints
/// so they can initiate hole punching.
///
/// Three mechanisms are used in order of preference:
///
/// 1. **DHT** (BitTorrent DHT BEP-5): each node stores its overlay endpoint
///    in the DHT under a well-known key derived from its node ID. Other nodes
///    look up this key to find the endpoint before punching.
///
/// 2. **PEX addr messages**: the existing P2P `addr` message is extended with
///    an optional overlay endpoint field. When a node connects to a peer via
///    the regular P2P protocol, it also learns the peer's overlay endpoint.
///
/// 3. **GitHub peer list**: the `bootstrap/peers.txt` file can include overlay
///    endpoints in the format `<ip>:<port>/<node_id_hex>`.
///
/// This module provides the in-memory endpoint registry that aggregates
/// endpoints from all three sources, and the serialization format used
/// to embed overlay endpoints in existing P2P messages.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::endpoint::Endpoint;

/// An entry in the endpoint registry.
#[derive(Debug, Clone)]
pub struct EndpointEntry {
    pub endpoint: Endpoint,
    /// Unix timestamp when this entry was last seen/updated.
    pub last_seen: u64,
    /// Source of this endpoint (for debugging/metrics).
    pub source: EndpointSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSource {
    Dht,
    Pex,
    GithubPeerList,
    Manual,
    HolePunch, // Learned from a successful hole punch
}

/// The endpoint registry — a shared in-memory store of known overlay endpoints.
#[derive(Clone)]
pub struct EndpointRegistry {
    inner: Arc<RwLock<HashMap<String, EndpointEntry>>>,
}

impl EndpointRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register or update an endpoint.
    pub async fn upsert(&self, endpoint: Endpoint, source: EndpointSource) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = self.inner.write().await;
        map.insert(
            endpoint.node_id.clone(),
            EndpointEntry {
                endpoint,
                last_seen: now,
                source,
            },
        );
    }

    /// Look up an endpoint by node ID.
    pub async fn get(&self, node_id: &str) -> Option<Endpoint> {
        self.inner
            .read()
            .await
            .get(node_id)
            .map(|e| e.endpoint.clone())
    }

    /// Return all known endpoints (for broadcasting to new peers).
    pub async fn all(&self) -> Vec<Endpoint> {
        self.inner
            .read()
            .await
            .values()
            .map(|e| e.endpoint.clone())
            .collect()
    }

    /// Return endpoints seen within the last `max_age` seconds.
    pub async fn fresh(&self, max_age: u64) -> Vec<Endpoint> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.inner
            .read()
            .await
            .values()
            .filter(|e| now.saturating_sub(e.last_seen) <= max_age)
            .map(|e| e.endpoint.clone())
            .collect()
    }

    /// Remove stale entries older than `max_age` seconds.
    pub async fn evict_stale(&self, max_age: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut map = self.inner.write().await;
        map.retain(|_, e| now.saturating_sub(e.last_seen) <= max_age);
    }

    /// Number of known endpoints.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }
}

impl Default for EndpointRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Wire format for embedding overlay endpoint info in a P2P addr message.
///
/// This is appended as an optional TLV (type=0xFE, length, value) to the
/// existing addr message payload. Nodes that don't understand it ignore it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayAddrExtension {
    /// The sender's overlay endpoint in wire format (see Endpoint::to_wire).
    pub endpoint_wire: String,
}

impl OverlayAddrExtension {
    pub const TLV_TYPE: u8 = 0xFE;

    pub fn new(endpoint: &Endpoint) -> Self {
        Self {
            endpoint_wire: endpoint.to_wire(),
        }
    }

    /// Serialize to bytes for embedding in addr message.
    pub fn to_bytes(&self) -> Vec<u8> {
        let payload = self.endpoint_wire.as_bytes();
        let mut out = Vec::with_capacity(2 + payload.len());
        out.push(Self::TLV_TYPE);
        out.push(payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    /// Parse from the tail of an addr message payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 2 || data[0] != Self::TLV_TYPE {
            return None;
        }
        let len = data[1] as usize;
        if data.len() < 2 + len {
            return None;
        }
        let wire = std::str::from_utf8(&data[2..2 + len]).ok()?;
        Some(Self {
            endpoint_wire: wire.to_string(),
        })
    }

    /// Extract the Endpoint from this extension.
    pub fn endpoint(&self) -> Option<Endpoint> {
        Endpoint::from_wire(&self.endpoint_wire)
    }
}

/// Parse overlay endpoints from the GitHub peer list format.
///
/// Extended format: `<ip>:<port>/<node_id_hex>`
/// Standard format: `<ip>:<port>` (no overlay endpoint)
pub fn parse_github_peer_line(line: &str) -> (Option<SocketAddr>, Option<Endpoint>) {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return (None, None);
    }

    if let Some((addr_str, node_id)) = line.split_once('/') {
        let addr = addr_str.parse::<SocketAddr>().ok();
        let endpoint = addr.map(|a| Endpoint::new(node_id.to_string(), a));
        (addr, endpoint)
    } else {
        let addr = line.parse::<SocketAddr>().ok();
        (addr, None)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_upsert_and_get() {
        let reg = EndpointRegistry::new();
        let ep = Endpoint::new("a".repeat(64), "1.2.3.4:22526".parse().unwrap());
        reg.upsert(ep.clone(), EndpointSource::Manual).await;
        let got = reg.get(&"a".repeat(64)).await.unwrap();
        assert_eq!(got.addr, ep.addr);
    }

    #[tokio::test]
    async fn test_registry_len() {
        let reg = EndpointRegistry::new();
        assert_eq!(reg.len().await, 0);
        let ep = Endpoint::new("b".repeat(64), "5.6.7.8:22526".parse().unwrap());
        reg.upsert(ep, EndpointSource::Dht).await;
        assert_eq!(reg.len().await, 1);
    }

    #[tokio::test]
    async fn test_registry_all() {
        let reg = EndpointRegistry::new();
        for i in 0..3u8 {
            let ep = Endpoint::new(
                format!("{:064x}", i),
                format!("1.2.3.{}:22526", i).parse().unwrap(),
            );
            reg.upsert(ep, EndpointSource::Pex).await;
        }
        assert_eq!(reg.all().await.len(), 3);
    }

    #[test]
    fn test_overlay_addr_extension_roundtrip() {
        let ep = Endpoint::new("c".repeat(64), "9.10.11.12:22526".parse().unwrap());
        let ext = OverlayAddrExtension::new(&ep);
        let bytes = ext.to_bytes();
        let parsed = OverlayAddrExtension::from_bytes(&bytes).unwrap();
        let ep2 = parsed.endpoint().unwrap();
        assert_eq!(ep2.node_id, ep.node_id);
        assert_eq!(ep2.addr, ep.addr);
    }

    #[test]
    fn test_parse_github_peer_line_with_node_id() {
        let (addr, ep) = parse_github_peer_line("1.2.3.4:22526/aabbcc");
        assert!(addr.is_some());
        assert!(ep.is_some());
    }

    #[test]
    fn test_parse_github_peer_line_plain() {
        let (addr, ep) = parse_github_peer_line("1.2.3.4:22526");
        assert!(addr.is_some());
        assert!(ep.is_none());
    }

    #[test]
    fn test_parse_github_peer_line_comment() {
        let (addr, ep) = parse_github_peer_line("# this is a comment");
        assert!(addr.is_none());
        assert!(ep.is_none());
    }
}
