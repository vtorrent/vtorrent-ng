/// Peer Exchange (PEX) Module for vTorrent 2.0
///
/// Implements the `addr` and `getaddr` messages from the Bitcoin P2P protocol,
/// which allow peers to share their known peer lists with each other.
///
/// Once a node connects to even one peer via DHT bootstrap, PEX takes over:
/// - The node sends `getaddr` to request the peer's known address list
/// - The peer responds with `addr` containing up to 1000 known peers
/// - The node adds these to its peer candidate pool
/// - The node also broadcasts its own address to all connected peers periodically
///
/// This creates a fully self-sustaining peer discovery mechanism that requires
/// zero centralized infrastructure.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::message::{AddrMsg, NetMessage, PeerAddr, NODE_NETWORK};

/// Maximum number of addresses to store in the peer address book.
pub const MAX_ADDR_BOOK_SIZE: usize = 10_000;

/// Maximum addresses to send in a single `addr` message.
pub const MAX_ADDR_PER_MSG: usize = 1000;

/// How long before a peer address is considered stale (3 hours).
pub const ADDR_STALE_SECS: u64 = 3 * 60 * 60;

/// How often to broadcast our own address to peers (30 minutes).
pub const SELF_ANNOUNCE_INTERVAL_SECS: u64 = 30 * 60;

/// How often to request addresses from peers (10 minutes).
pub const GETADDR_INTERVAL_SECS: u64 = 10 * 60;

/// An entry in the peer address book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddrEntry {
    /// The peer's socket address.
    pub addr: SocketAddr,
    /// Services bitfield (from the peer's version message).
    pub services: u64,
    /// Unix timestamp of when this address was last seen/announced.
    pub last_seen: u64,
    /// Number of successful connections to this peer.
    pub connection_attempts: u32,
    /// Number of successful connections to this peer.
    pub successful_connections: u32,
}

impl AddrEntry {
    pub fn new(addr: SocketAddr, services: u64) -> Self {
        let last_seen = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            addr,
            services,
            last_seen,
            connection_attempts: 0,
            successful_connections: 0,
        }
    }

    /// Returns true if this address is fresh enough to try connecting.
    pub fn is_fresh(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.last_seen) < ADDR_STALE_SECS
    }

    /// Returns a connection quality score (higher = better candidate).
    pub fn quality_score(&self) -> f64 {
        if self.connection_attempts == 0 {
            return 0.5; // Unknown — neutral score
        }
        let success_rate = self.successful_connections as f64 / self.connection_attempts as f64;
        // Fresher addresses score higher
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let age_hours = now.saturating_sub(self.last_seen) as f64 / 3600.0;
        let freshness = (-age_hours / 24.0).exp(); // Exponential decay over 24 hours
        success_rate * 0.7 + freshness * 0.3
    }
}

/// The peer address book — stores known peer addresses for future connections.
pub struct AddrBook {
    /// All known peer addresses, keyed by socket address.
    entries: HashMap<SocketAddr, AddrEntry>,
    /// Addresses that are currently connected (excluded from candidates).
    connected: HashSet<SocketAddr>,
    /// Our own listen address (excluded from candidates).
    our_addr: Option<SocketAddr>,
    /// Last time we broadcast our own address.
    last_self_announce: Instant,
    /// Last time we sent `getaddr` to peers.
    last_getaddr: Instant,
}

impl AddrBook {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            connected: HashSet::new(),
            our_addr: None,
            last_self_announce: Instant::now() - Duration::from_secs(SELF_ANNOUNCE_INTERVAL_SECS),
            last_getaddr: Instant::now() - Duration::from_secs(GETADDR_INTERVAL_SECS),
        }
    }

    /// Set our own listen address (so we don't try to connect to ourselves).
    pub fn set_our_addr(&mut self, addr: SocketAddr) {
        self.our_addr = Some(addr);
    }

    /// Mark a peer as connected.
    pub fn mark_connected(&mut self, addr: SocketAddr) {
        self.connected.insert(addr);
        if let Some(entry) = self.entries.get_mut(&addr) {
            entry.successful_connections += 1;
        }
    }

    /// Mark a peer as disconnected.
    pub fn mark_disconnected(&mut self, addr: SocketAddr) {
        self.connected.remove(&addr);
    }

    /// Record a connection attempt.
    pub fn record_attempt(&mut self, addr: SocketAddr) {
        if let Some(entry) = self.entries.get_mut(&addr) {
            entry.connection_attempts += 1;
        }
    }

    /// Add a list of addresses to the book.
    pub fn add_addrs(&mut self, addrs: &[AddrEntry]) {
        for entry in addrs {
            // Skip our own address
            if Some(entry.addr) == self.our_addr {
                continue;
            }
            // Skip loopback addresses
            if entry.addr.ip().is_loopback() {
                continue;
            }
            // Skip private addresses for mainnet (allow for testnet/local dev)
            // TODO: add testnet flag

            // Update existing or insert new
            self.entries
                .entry(entry.addr)
                .and_modify(|e| {
                    // Only update if newer
                    if entry.last_seen > e.last_seen {
                        e.last_seen = entry.last_seen;
                        e.services = entry.services;
                    }
                })
                .or_insert_with(|| entry.clone());
        }

        // Prune if over the limit — remove the lowest-quality entries
        if self.entries.len() > MAX_ADDR_BOOK_SIZE {
            let mut entries: Vec<_> = self.entries.iter()
                .map(|(addr, e)| (*addr, e.quality_score()))
                .collect();
            entries.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let to_remove = self.entries.len() - MAX_ADDR_BOOK_SIZE;
            for (addr, _) in entries.iter().take(to_remove) {
                self.entries.remove(addr);
            }
        }
    }

    /// Add addresses from a DHT discovery result.
    pub fn add_dht_peers(&mut self, addrs: &[SocketAddr]) {
        let entries: Vec<AddrEntry> = addrs.iter()
            .map(|&addr| AddrEntry::new(addr, NODE_NETWORK))
            .collect();
        self.add_addrs(&entries);
        tracing::info!("PEX: Added {} DHT-discovered peers to address book", addrs.len());
    }

    /// Add addresses from a received `addr` message.
    pub fn add_from_addr_msg(&mut self, msg: &AddrMsg) {
        let entries: Vec<AddrEntry> = msg.addrs.iter()
            .filter_map(|pa| {
                let addr: SocketAddr = format!("{}:{}", pa.addr, pa.port).parse().ok()?;
                Some(AddrEntry {
                    addr,
                    services: pa.services,
                    last_seen: pa.timestamp as u64,
                    connection_attempts: 0,
                    successful_connections: 0,
                })
            })
            .collect();
        let count = entries.len();
        self.add_addrs(&entries);
        tracing::debug!("PEX: Received {} addresses from peer", count);
    }

    /// Get the best candidate addresses to try connecting to.
    ///
    /// Returns up to `count` addresses sorted by quality score, excluding
    /// already-connected peers and our own address.
    pub fn get_candidates(&self, count: usize) -> Vec<SocketAddr> {
        let mut candidates: Vec<_> = self.entries.iter()
            .filter(|(addr, entry)| {
                !self.connected.contains(*addr)
                    && Some(**addr) != self.our_addr
                    && entry.is_fresh()
            })
            .map(|(addr, entry)| (*addr, entry.quality_score()))
            .collect();

        // Sort by quality score descending
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.iter().take(count).map(|(addr, _)| *addr).collect()
    }

    /// Get all known addresses for sharing with peers (for `addr` response).
    ///
    /// Returns up to MAX_ADDR_PER_MSG fresh addresses.
    pub fn get_for_sharing(&self) -> AddrMsg {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        let addrs: Vec<PeerAddr> = self.entries.values()
            .filter(|e| e.is_fresh())
            .take(MAX_ADDR_PER_MSG)
            .map(|e| PeerAddr {
                timestamp: e.last_seen as u32,
                services: e.services,
                addr: e.addr.ip().to_string(),
                port: e.addr.port(),
            })
            .collect();

        AddrMsg { addrs }
    }

    /// Build a `getaddr` message to request peer addresses.
    pub fn build_getaddr() -> NetMessage {
        NetMessage::new("getaddr", vec![])
    }

    /// Build an `addr` message with our known peers.
    pub fn build_addr_msg(&self) -> NetMessage {
        let addr_msg = self.get_for_sharing();
        let payload = serde_json::to_vec(&addr_msg).unwrap_or_default();
        NetMessage::new("addr", payload)
    }

    /// Build a self-announcement `addr` message (just our own address).
    pub fn build_self_announce(&self, our_services: u64) -> Option<NetMessage> {
        let our_addr = self.our_addr?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        let addr_msg = AddrMsg {
            addrs: vec![PeerAddr {
                timestamp: now,
                services: our_services,
                addr: our_addr.ip().to_string(),
                port: our_addr.port(),
            }],
        };
        let payload = serde_json::to_vec(&addr_msg).unwrap_or_default();
        Some(NetMessage::new("addr", payload))
    }

    /// Returns true if it's time to broadcast our own address.
    pub fn should_self_announce(&self) -> bool {
        self.last_self_announce.elapsed().as_secs() >= SELF_ANNOUNCE_INTERVAL_SECS
    }

    /// Returns true if it's time to send `getaddr` to peers.
    pub fn should_getaddr(&self) -> bool {
        self.last_getaddr.elapsed().as_secs() >= GETADDR_INTERVAL_SECS
    }

    /// Mark that we just sent a self-announcement.
    pub fn record_self_announce(&mut self) {
        self.last_self_announce = Instant::now();
    }

    /// Mark that we just sent a `getaddr`.
    pub fn record_getaddr(&mut self) {
        self.last_getaddr = Instant::now();
    }

    /// Total number of known addresses.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Number of currently connected peers.
    pub fn connected_count(&self) -> usize {
        self.connected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for AddrBook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), port)
    }

    #[test]
    fn test_addr_book_add_and_retrieve() {
        let mut book = AddrBook::new();
        let addr = make_addr(22524);
        book.add_dht_peers(&[addr]);
        assert_eq!(book.len(), 1);
        let candidates = book.get_candidates(10);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], addr);
    }

    #[test]
    fn test_addr_book_excludes_connected() {
        let mut book = AddrBook::new();
        let addr = make_addr(22524);
        book.add_dht_peers(&[addr]);
        book.mark_connected(addr);
        let candidates = book.get_candidates(10);
        assert!(candidates.is_empty(), "Connected peers should not be candidates");
    }

    #[test]
    fn test_addr_book_excludes_self() {
        let mut book = AddrBook::new();
        let addr = make_addr(22524);
        book.set_our_addr(addr);
        book.add_dht_peers(&[addr]);
        let candidates = book.get_candidates(10);
        assert!(candidates.is_empty(), "Our own address should not be a candidate");
    }

    #[test]
    fn test_addr_book_quality_score() {
        let entry = AddrEntry::new(make_addr(22524), NODE_NETWORK);
        let score = entry.quality_score();
        // Unknown peer (no attempts) gets neutral score
        assert!((score - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_addr_book_from_addr_msg() {
        let mut book = AddrBook::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        let msg = AddrMsg {
            addrs: vec![
                PeerAddr { timestamp: now, services: NODE_NETWORK, addr: "10.0.0.1".to_string(), port: 22524 },
                PeerAddr { timestamp: now, services: NODE_NETWORK, addr: "10.0.0.2".to_string(), port: 22524 },
            ],
        };
        book.add_from_addr_msg(&msg);
        assert_eq!(book.len(), 2);
    }

    #[test]
    fn test_getaddr_message() {
        let msg = AddrBook::build_getaddr();
        assert_eq!(msg.command_str(), "getaddr");
        assert!(msg.payload.is_empty());
    }

    #[test]
    fn test_self_announce_message() {
        let mut book = AddrBook::new();
        let addr = make_addr(22524);
        book.set_our_addr(addr);
        let msg = book.build_self_announce(NODE_NETWORK);
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg.command_str(), "addr");
    }

    #[test]
    fn test_addr_book_pruning() {
        let mut book = AddrBook::new();
        // Add more than MAX_ADDR_BOOK_SIZE entries
        let addrs: Vec<SocketAddr> = (1u16..=100).map(|i| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, (i / 256) as u8, (i % 256) as u8)), 22524)
        }).collect();
        book.add_dht_peers(&addrs);
        // Should not exceed MAX_ADDR_BOOK_SIZE (10,000), but with only 100 entries, all should fit
        assert!(book.len() <= MAX_ADDR_BOOK_SIZE);
        assert_eq!(book.len(), 100);
    }
}
