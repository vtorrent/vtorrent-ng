//! BitTorrent Kademlia DHT (BEP-5): get_peers, find_node, announce_peer.

use crate::error::{Result, TorrentError};
use crate::tracker::TrackerPeer;
use std::net::{Ipv4Addr, SocketAddr};

/// A 20-byte DHT node id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub [u8; 20]);

impl NodeId {
    pub fn random() -> Self {
        use rand::RngCore;
        let mut id = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut id);
        NodeId(id)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

/// A compact DHT node (26 bytes: 20 id + 4 ip + 2 port).
#[derive(Debug, Clone)]
pub struct CompactNode {
    pub id: NodeId,
    pub addr: SocketAddr,
}

impl CompactNode {
    pub fn parse_list(data: &[u8]) -> Vec<Self> {
        data.chunks_exact(26)
            .filter_map(|chunk| {
                let mut id = [0u8; 20];
                id.copy_from_slice(&chunk[..20]);
                let ip = Ipv4Addr::new(chunk[20], chunk[21], chunk[22], chunk[23]);
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

/// Build a BEP-5 `get_peers` query.
pub fn build_get_peers_query(node_id: &NodeId, info_hash: &[u8; 20], tid: &[u8; 2]) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"9:info_hash20:");
    msg.extend_from_slice(info_hash);
    msg.extend_from_slice(b"e1:q9:get_peers1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

/// Build a BEP-5 `find_node` query.
pub fn build_find_node_query(node_id: &NodeId, target: &[u8; 20], tid: &[u8; 2]) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"6:target20:");
    msg.extend_from_slice(target);
    msg.extend_from_slice(b"e1:q9:find_node1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

/// Build a BEP-5 `announce_peer` query.
pub fn build_announce_peer_query(
    node_id: &NodeId,
    info_hash: &[u8; 20],
    port: u16,
    token: &[u8],
    tid: &[u8; 2],
) -> Vec<u8> {
    let mut msg = Vec::new();
    msg.extend_from_slice(b"d1:ad2:id20:");
    msg.extend_from_slice(node_id.as_bytes());
    msg.extend_from_slice(b"12:implied_porti1e9:info_hash20:");
    msg.extend_from_slice(info_hash);
    msg.extend_from_slice(b"4:porti");
    msg.extend_from_slice(port.to_string().as_bytes());
    msg.extend_from_slice(b"e5:token");
    msg.extend_from_slice(format!("{}:", token.len()).as_bytes());
    msg.extend_from_slice(token);
    msg.extend_from_slice(b"1:q13:announce_peer1:t2:");
    msg.extend_from_slice(tid);
    msg.extend_from_slice(b"1:y1:qe");
    msg
}

/// Parse a DHT response, extracting peers (from `values`) and nodes (from `nodes`).
pub fn parse_dht_response(data: &[u8]) -> (Vec<TrackerPeer>, Vec<CompactNode>) {
    let mut peers = Vec::new();
    let mut nodes = Vec::new();

    if let Some(pos) = find_bytes(data, b"6:values") {
        let after = &data[pos + 8..];
        if let Some(list) = parse_bencode_list_of_strings(after) {
            for item in list {
                peers.extend(parse_compact_peers(&item));
            }
        }
    }

    if let Some(pos) = find_bytes(data, b"5:nodes") {
        let after = &data[pos + 7..];
        if let Some(node_data) = parse_bencode_string(after) {
            nodes = CompactNode::parse_list(&node_data);
        }
    }

    (peers, nodes)
}

/// Parse compact peers (6 bytes each: 4 IP + 2 port).
fn parse_compact_peers(data: &[u8]) -> Vec<TrackerPeer> {
    let mut peers = Vec::new();
    let mut i = 0;
    while i + 6 <= data.len() {
        let ip = format!("{}.{}.{}.{}", data[i], data[i + 1], data[i + 2], data[i + 3]);
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

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_peers_query_contains_info_hash() {
        let node_id = NodeId([0x11; 20]);
        let info_hash = [0xAA; 20];
        let q = build_get_peers_query(&node_id, &info_hash, &[0x01, 0x02]);
        let q_hex = hex::encode(&q);
        assert!(q_hex.contains(&hex::encode(info_hash)));
    }

    #[test]
    fn test_find_node_query_contains_target() {
        let node_id = NodeId([0x11; 20]);
        let target = [0xBB; 20];
        let q = build_find_node_query(&node_id, &target, &[0x01, 0x02]);
        let q_hex = hex::encode(&q);
        assert!(q_hex.contains(&hex::encode(target)));
    }

    #[test]
    fn test_announce_peer_query_contains_port() {
        let node_id = NodeId([0x11; 20]);
        let info_hash = [0xAA; 20];
        let q = build_announce_peer_query(&node_id, &info_hash, 6881, b"token", &[0x01, 0x02]);
        let q_hex = hex::encode(&q);
        // The port is bencoded as an integer: "i6881e" -> hex "693638383165".
        assert!(q_hex.contains("693638383165"));
    }

    #[test]
    fn test_parse_dht_response_peers() {
        let mut resp = Vec::new();
        resp.extend_from_slice(b"d6:valuesl6:");
        resp.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
        resp.extend_from_slice(b"ee");
        let (peers, nodes) = parse_dht_response(&resp);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip, "10.0.0.1");
        assert_eq!(peers[0].port, 6881);
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_parse_dht_response_nodes() {
        let mut node = Vec::new();
        node.extend_from_slice(&[0x22; 20]);
        node.extend_from_slice(&[10, 0, 0, 2]);
        node.extend_from_slice(&6882u16.to_be_bytes());
        let mut resp = Vec::new();
        resp.extend_from_slice(b"d5:nodes");
        resp.extend_from_slice(format!("{}:", node.len()).as_bytes());
        resp.extend_from_slice(&node);
        resp.extend_from_slice(b"e");
        let (peers, nodes) = parse_dht_response(&resp);
        assert!(peers.is_empty());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].addr.port(), 6882);
    }
}
