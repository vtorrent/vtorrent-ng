/// DHT Bootstrap Module for vTorrent 2.0
///
/// Uses the BitTorrent Kademlia DHT network (BEP-5) to discover vTorrent peers
/// without requiring any centralized seed servers. The node announces itself
/// using a vTorrent-specific info-hash derived from the chain genesis hash,
/// then queries the DHT for other nodes announcing the same info-hash.
///
/// Bootstrap nodes are the well-known public BitTorrent DHT routers — these
/// are maintained by BitTorrent Inc., µTorrent, and Transmission and have
/// been reliably online for over a decade.

use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use std::collections::HashMap;

use rand::Rng;
use sha2::{Sha256, Digest};

use crate::error::{P2pError, Result};

// ─── vTorrent DHT Info-Hash ───────────────────────────────────────────────────
// This is the "torrent" that vTorrent nodes announce on the BitTorrent DHT.
// Derived from: SHA256("vtorrent-mainnet-v2-genesis") truncated to 20 bytes.
// Any vTorrent node that queries this info-hash will find other vTorrent nodes.
pub const VTORRENT_DHT_INFOHASH: &str = "vtorrent-mainnet-v2-genesis";

// ─── Public DHT Bootstrap Nodes ──────────────────────────────────────────────
// These are the well-known BitTorrent DHT bootstrap nodes. They are maintained
// by BitTorrent Inc., µTorrent, and Transmission respectively.
pub const DHT_BOOTSTRAP_NODES: &[&str] = &[
    "router.bittorrent.com:6881",
    "router.utorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.aelitis.com:6881",   // Vuze/Azureus
];

/// Timeout for DHT UDP queries.
const DHT_TIMEOUT_MS: u64 = 3000;

/// Maximum peers to collect from DHT before returning.
const MAX_DHT_PEERS: usize = 50;

/// A DHT node ID (20 bytes, Kademlia node identifier).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeId([u8; 20]);

impl NodeId {
    /// Generate a random node ID.
    pub fn random() -> Self {
        let mut rng = rand::thread_rng();
        let mut id = [0u8; 20];
        rng.fill(&mut id);
        Self(id)
    }

    /// Derive the vTorrent DHT info-hash from the chain identifier.
    pub fn vtorrent_infohash() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(VTORRENT_DHT_INFOHASH.as_bytes());
        let hash = hasher.finalize();
        let mut id = [0u8; 20];
        id.copy_from_slice(&hash[..20]);
        Self(id)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

/// A compact peer address from the DHT (6 bytes: 4 IP + 2 port).
#[derive(Debug, Clone)]
pub struct CompactPeer {
    pub addr: SocketAddr,
}

impl CompactPeer {
    /// Parse a list of compact peers from a byte slice (6 bytes each).
    pub fn parse_list(data: &[u8]) -> Vec<Self> {
        data.chunks_exact(6)
            .filter_map(|chunk| {
                let ip = std::net::Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3]);
                let port = u16::from_be_bytes([chunk[4], chunk[5]]);
                if port > 0 {
                    Some(CompactPeer {
                        addr: SocketAddr::from((ip, port)),
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

/// A compact node info from the DHT (26 bytes: 20 ID + 4 IP + 2 port).
#[derive(Debug, Clone)]
pub struct CompactNode {
    pub id: NodeId,
    pub addr: SocketAddr,
}

impl CompactNode {
    /// Parse a list of compact nodes from a byte slice (26 bytes each).
    pub fn parse_list(data: &[u8]) -> Vec<Self> {
        data.chunks_exact(26)
            .filter_map(|chunk| {
                let mut id = [0u8; 20];
                id.copy_from_slice(&chunk[..20]);
                let ip = std::net::Ipv4Addr::new(chunk[20], chunk[21], chunk[22], chunk[23]);
                let port = u16::from_be_bytes([chunk[24], chunk[25]]);
                if port > 0 {
                    Some(CompactNode {
                        id: NodeId(id),
                        addr: SocketAddr::from((ip, port)),
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Encode a byte slice as a bencoded string: "len:data"
fn bencode_bytes(data: &[u8]) -> Vec<u8> {
    let mut result = format!("{}:", data.len()).into_bytes();
    result.extend_from_slice(data);
    result
}

/// Build a BEP-5 `get_peers` query message.
///
/// Format: `d1:ad2:id20:<node_id>9:info_hash20:<info_hash>e1:q9:get_peers1:t2:<tid>1:y1:qe`
fn build_get_peers_query(node_id: &NodeId, info_hash: &NodeId, tid: &[u8; 2]) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"9:info_hash20:");
    msg.extend_from_slice(info_hash.as_bytes());
    msg.extend_from_slice(b"e1:q9:get_peers1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

/// Build a BEP-5 `find_node` query message.
fn build_find_node_query(node_id: &NodeId, target: &NodeId, tid: &[u8; 2]) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"6:target20:");
    msg.extend_from_slice(target.as_bytes());
    msg.extend_from_slice(b"e1:q9:find_node1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

/// Parse a bencoded response to extract `values` (peers) or `nodes` (DHT nodes).
///
/// This is a minimal bencode parser focused on extracting the fields we care about.
/// It handles the specific response format of BEP-5 `get_peers` responses.
fn parse_dht_response(data: &[u8]) -> (Vec<CompactPeer>, Vec<CompactNode>) {
    let mut peers = Vec::new();
    let mut nodes = Vec::new();

    // Look for "values" key (list of compact peer strings)
    if let Some(pos) = find_bytes(data, b"6:values") {
        let after = &data[pos + 8..];
        if let Some(list_end) = parse_bencode_list_of_strings(after) {
            for item in list_end {
                peers.extend(CompactPeer::parse_list(&item));
            }
        }
    }

    // Look for "nodes" key (compact node info string)
    if let Some(pos) = find_bytes(data, b"5:nodes") {
        let after = &data[pos + 7..];
        if let Some(node_data) = parse_bencode_string(after) {
            nodes = CompactNode::parse_list(&node_data);
        }
    }

    (peers, nodes)
}

/// Find a byte pattern in a slice, returning the position if found.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse a bencoded string at the start of the slice.
/// Returns the string bytes if successful.
fn parse_bencode_string(data: &[u8]) -> Option<Vec<u8>> {
    let colon_pos = data.iter().position(|&b| b == b':')?;
    let len_str = std::str::from_utf8(&data[..colon_pos]).ok()?;
    let len: usize = len_str.parse().ok()?;
    let start = colon_pos + 1;
    if start + len <= data.len() {
        Some(data[start..start + len].to_vec())
    } else {
        None
    }
}

/// Parse a bencoded list of strings: `l<str1><str2>...e`
fn parse_bencode_list_of_strings(data: &[u8]) -> Option<Vec<Vec<u8>>> {
    if data.first() != Some(&b'l') {
        return None;
    }
    let mut result = Vec::new();
    let mut pos = 1;
    while pos < data.len() && data[pos] != b'e' {
        if let Some(s) = parse_bencode_string(&data[pos..]) {
            let colon = data[pos..].iter().position(|&b| b == b':')?;
            let len_str = std::str::from_utf8(&data[pos..pos + colon]).ok()?;
            let len: usize = len_str.parse().ok()?;
            pos += colon + 1 + len;
            result.push(s);
        } else {
            break;
        }
    }
    Some(result)
}

/// The DHT bootstrap client.
pub struct DhtBootstrap {
    node_id: NodeId,
    infohash: NodeId,
}

impl DhtBootstrap {
    pub fn new() -> Self {
        Self {
            node_id: NodeId::random(),
            infohash: NodeId::vtorrent_infohash(),
        }
    }

    /// Discover vTorrent peers via the BitTorrent DHT network.
    ///
    /// This function:
    /// 1. Sends `find_node` queries to the well-known bootstrap nodes
    /// 2. Sends `get_peers` queries for the vTorrent info-hash
    /// 3. Collects peer addresses from responses
    /// 4. Returns a list of potential vTorrent peer socket addresses
    ///
    /// The returned addresses are on the vTorrent P2P port (22524), not the DHT port.
    pub fn discover_peers(&self) -> Vec<SocketAddr> {
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("DHT: Failed to bind UDP socket: {}", e);
                return Vec::new();
            }
        };

        socket.set_read_timeout(Some(Duration::from_millis(DHT_TIMEOUT_MS))).ok();

        let mut discovered_peers: Vec<SocketAddr> = Vec::new();
        let mut queried_nodes: HashMap<SocketAddr, bool> = HashMap::new();
        let mut pending_nodes: Vec<SocketAddr> = Vec::new();

        // Seed the queue with the well-known bootstrap nodes
        for seed in DHT_BOOTSTRAP_NODES {
            if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&seed) {
                for addr in addrs {
                    pending_nodes.push(addr);
                }
            }
        }

        let start = Instant::now();
        let timeout = Duration::from_secs(10);
        let mut tid: u16 = 0;

        while !pending_nodes.is_empty() && discovered_peers.len() < MAX_DHT_PEERS {
            if start.elapsed() > timeout {
                tracing::debug!("DHT: Bootstrap timeout after {}ms", start.elapsed().as_millis());
                break;
            }

            let node_addr = pending_nodes.remove(0);
            if queried_nodes.contains_key(&node_addr) {
                continue;
            }
            queried_nodes.insert(node_addr, true);

            tid = tid.wrapping_add(1);
            let tid_bytes = tid.to_be_bytes();

            // Send get_peers query for the vTorrent info-hash
            let query = build_get_peers_query(&self.node_id, &self.infohash, &tid_bytes);
            if socket.send_to(&query, node_addr).is_err() {
                continue;
            }

            // Read response(s)
            let mut buf = [0u8; 65536];
            while let Ok((len, _from)) = socket.recv_from(&mut buf) {
                let (peers, nodes) = parse_dht_response(&buf[..len]);

                // Peers found — these are on DHT port; we need to check if they
                // are running vTorrent by attempting a connection on port 22524
                for peer in peers {
                    let vtorrent_addr = SocketAddr::new(peer.addr.ip(), crate::peer_manager::DEFAULT_PORT);
                    if !discovered_peers.contains(&vtorrent_addr) {
                        discovered_peers.push(vtorrent_addr);
                        tracing::debug!("DHT: Found peer candidate: {}", vtorrent_addr);
                    }
                }

                // Nodes found — add to pending queue for further querying
                for node in nodes {
                    if !queried_nodes.contains_key(&node.addr) {
                        pending_nodes.push(node.addr);
                    }
                }

                // Stop reading if we have enough peers
                if discovered_peers.len() >= MAX_DHT_PEERS {
                    break;
                }
            }
        }

        tracing::info!(
            "DHT bootstrap complete: {} peer candidates found, {} nodes queried",
            discovered_peers.len(),
            queried_nodes.len()
        );

        discovered_peers
    }

    /// Announce ourselves on the BitTorrent DHT so other vTorrent nodes can find us.
    ///
    /// This sends `announce_peer` messages to the closest DHT nodes we know about.
    /// Should be called periodically (every 30 minutes) to stay visible in the DHT.
    pub fn announce(&self, our_port: u16) {
        let socket = match UdpSocket::bind("0.0.0.0:0") {
            Ok(s) => s,
            Err(_) => return,
        };
        socket.set_read_timeout(Some(Duration::from_millis(2000))).ok();

        // First do a find_node to get close nodes, then announce to them
        for seed in DHT_BOOTSTRAP_NODES.iter().take(2) {
            if let Ok(mut addrs) = std::net::ToSocketAddrs::to_socket_addrs(seed) {
                if let Some(addr) = addrs.next() {
                    let tid = [0x61u8, 0x61u8]; // "aa"
                    let query = build_find_node_query(&self.node_id, &self.infohash, &tid);
                    let _ = socket.send_to(&query, addr);

                    let mut buf = [0u8; 65536];
                    if let Ok((len, _)) = socket.recv_from(&mut buf) {
                        let (_, nodes) = parse_dht_response(&buf[..len]);
                        for node in nodes.iter().take(8) {
                            // Build announce_peer message
                            let announce = build_announce_peer(
                                &self.node_id,
                                &self.infohash,
                                our_port,
                                &[0x62u8, 0x62u8],
                            );
                            let _ = socket.send_to(&announce, node.addr);
                        }
                    }
                }
            }
        }

        tracing::debug!("DHT: Announced on port {}", our_port);
    }
}

/// Build a BEP-5 `announce_peer` message.
fn build_announce_peer(
    node_id: &NodeId,
    info_hash: &NodeId,
    port: u16,
    tid: &[u8; 2],
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"12:implied_porti1e9:info_hash20:");
    msg.extend_from_slice(info_hash.as_bytes());
    msg.extend_from_slice(b"4:porti");
    msg.extend_from_slice(port.to_string().as_bytes());
    msg.extend_from_slice(b"e5:token0:e1:q13:announce_peer1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

impl Default for DhtBootstrap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vtorrent_infohash_deterministic() {
        let h1 = NodeId::vtorrent_infohash();
        let h2 = NodeId::vtorrent_infohash();
        assert_eq!(h1, h2, "Info-hash must be deterministic");
    }

    #[test]
    fn test_vtorrent_infohash_not_zero() {
        let h = NodeId::vtorrent_infohash();
        assert_ne!(h.0, [0u8; 20], "Info-hash must not be all zeros");
    }

    #[test]
    fn test_node_id_random_unique() {
        let id1 = NodeId::random();
        let id2 = NodeId::random();
        // Astronomically unlikely to be equal
        assert_ne!(id1, id2, "Random node IDs should be unique");
    }

    #[test]
    fn test_compact_peer_parse() {
        // 127.0.0.1:8080 in compact format
        let data = [127u8, 0, 0, 1, 0x1F, 0x90];
        let peers = CompactPeer::parse_list(&data);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addr.port(), 8080);
    }

    #[test]
    fn test_compact_peer_parse_multiple() {
        // Two peers: 1.2.3.4:1000 and 5.6.7.8:2000
        let data = [
            1u8, 2, 3, 4, 0x03, 0xE8,  // 1.2.3.4:1000
            5u8, 6, 7, 8, 0x07, 0xD0,  // 5.6.7.8:2000
        ];
        let peers = CompactPeer::parse_list(&data);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].addr.port(), 1000);
        assert_eq!(peers[1].addr.port(), 2000);
    }

    #[test]
    fn test_compact_node_parse() {
        // 20-byte ID + 4-byte IP + 2-byte port
        let mut data = vec![0u8; 26];
        data[0..20].fill(0xAB);
        data[20..24].copy_from_slice(&[192, 168, 1, 1]);
        data[24..26].copy_from_slice(&[0x57, 0xE4]); // port 22500
        let nodes = CompactNode::parse_list(&data);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.0, [0xAB; 20]);
        assert_eq!(nodes[0].addr.port(), 22500);
    }

    #[test]
    fn test_get_peers_query_format() {
        let node_id = NodeId([0u8; 20]);
        let infohash = NodeId([1u8; 20]);
        let tid = [0x61u8, 0x61u8];
        let query = build_get_peers_query(&node_id, &infohash, &tid);
        // Must be valid bencode starting with 'd' and ending with 'e'
        assert_eq!(query[0], b'd');
        assert_eq!(*query.last().unwrap(), b'e');
        // Must contain the query type
        assert!(query.windows(9).any(|w| w == b"get_peers"));
    }

    #[test]
    fn test_bencode_bytes() {
        let result = bencode_bytes(b"hello");
        assert_eq!(result, b"5:hello");
    }
}
