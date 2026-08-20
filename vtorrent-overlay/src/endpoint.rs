/// An overlay endpoint — the combination of a node's public key identity
/// and its reachable network address (UDP socket address).
///
/// This is what gets published to the DHT and exchanged via PEX addr messages.
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An overlay network endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    /// Curve25519 public key (32 bytes, hex-encoded) — the node's identity.
    pub node_id: String,
    /// The external UDP socket address (public IP:port after STUN discovery).
    pub addr: SocketAddr,
    /// Optional internal LAN address for same-subnet direct connections.
    pub lan_addr: Option<SocketAddr>,
    /// Protocol version supported by this node.
    pub version: u8,
}

impl Endpoint {
    pub fn new(node_id: String, addr: SocketAddr) -> Self {
        Self {
            node_id,
            addr,
            lan_addr: None,
            version: 1,
        }
    }

    pub fn with_lan(mut self, lan_addr: SocketAddr) -> Self {
        self.lan_addr = Some(lan_addr);
        self
    }

    /// Serialize to a compact wire format for DHT/PEX exchange.
    /// Format: `<node_id_hex>@<addr>[|<lan_addr>]`
    pub fn to_wire(&self) -> String {
        match &self.lan_addr {
            Some(lan) => format!("{}@{}|{}", self.node_id, self.addr, lan),
            None => format!("{}@{}", self.node_id, self.addr),
        }
    }

    /// Parse from the compact wire format.
    pub fn from_wire(s: &str) -> Option<Self> {
        let (node_id, rest) = s.split_once('@')?;
        if node_id.len() != 64 {
            return None; // Must be 32-byte hex
        }
        let (addr_str, lan_str) = match rest.split_once('|') {
            Some((a, l)) => (a, Some(l)),
            None => (rest, None),
        };
        let addr = SocketAddr::from_str(addr_str).ok()?;
        let lan_addr = lan_str.and_then(|l| SocketAddr::from_str(l).ok());
        Some(Self {
            node_id: node_id.to_string(),
            addr,
            lan_addr,
            version: 1,
        })
    }

    /// Returns the best address to try first: LAN if available, else external.
    pub fn best_addr(&self) -> SocketAddr {
        self.lan_addr.unwrap_or(self.addr)
    }

    /// Returns all candidate addresses to try (LAN first, then external).
    pub fn candidates(&self) -> Vec<SocketAddr> {
        let mut addrs = Vec::new();
        if let Some(lan) = self.lan_addr {
            addrs.push(lan);
        }
        addrs.push(self.addr);
        addrs
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short = &self.node_id[..self.node_id.len().min(8)];
        write!(f, "{}@{}", short, self.addr)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_endpoint() -> Endpoint {
        Endpoint::new("a".repeat(64), "1.2.3.4:22526".parse().unwrap())
    }

    #[test]
    fn test_wire_roundtrip_no_lan() {
        let ep = make_endpoint();
        let wire = ep.to_wire();
        let parsed = Endpoint::from_wire(&wire).unwrap();
        assert_eq!(parsed.node_id, ep.node_id);
        assert_eq!(parsed.addr, ep.addr);
        assert!(parsed.lan_addr.is_none());
    }

    #[test]
    fn test_wire_roundtrip_with_lan() {
        let ep = make_endpoint().with_lan("192.168.1.10:22526".parse().unwrap());
        let wire = ep.to_wire();
        let parsed = Endpoint::from_wire(&wire).unwrap();
        assert_eq!(parsed.lan_addr, Some("192.168.1.10:22526".parse().unwrap()));
    }

    #[test]
    fn test_from_wire_invalid_node_id() {
        assert!(Endpoint::from_wire("short@1.2.3.4:22526").is_none());
    }

    #[test]
    fn test_best_addr_prefers_lan() {
        let ep = make_endpoint().with_lan("192.168.1.10:22526".parse().unwrap());
        assert_eq!(ep.best_addr(), "192.168.1.10:22526".parse().unwrap());
    }

    #[test]
    fn test_candidates_order() {
        let ep = make_endpoint().with_lan("192.168.1.10:22526".parse().unwrap());
        let cands = ep.candidates();
        assert_eq!(cands[0], "192.168.1.10:22526".parse().unwrap());
        assert_eq!(cands[1], "1.2.3.4:22526".parse().unwrap());
    }
}
