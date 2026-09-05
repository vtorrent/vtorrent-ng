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
        data.as_chunks::<26>()
            .0
            .iter()
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

/// Parse a DHT reply, extracting peers (from `r.values`) and nodes
/// (`r.nodes`). Only accepts well-formed top-level response dicts; returns
/// the transaction id (`r.t`) so callers can match replies to queries.
pub fn parse_dht_response(data: &[u8]) -> (Vec<TrackerPeer>, Vec<CompactNode>, Option<Vec<u8>>) {
    let mut peers = Vec::new();
    let mut nodes = Vec::new();
    let mut txn_id = None;

    let mut pos = 0usize;
    if let Some(BVal::Dict(root)) = parse_bencode_value(data, &mut pos, 0) {
        // KRPC reply: {"t": <txn id>, "y": "r", "r": {...}}
        if let Some((_, BVal::Bytes(t))) = root.iter().find(|(k, _)| k == b"t") {
            txn_id = Some(t.clone());
        }
        if let Some(BVal::Dict(r)) = root.iter().find(|(k, _)| k == b"r").map(|(_, v)| v) {
            if let Some((_, BVal::Bytes(node_data))) = r.iter().find(|(k, _)| k == b"nodes") {
                nodes = CompactNode::parse_list(node_data);
            }
            if let Some((_, BVal::List(values))) = r.iter().find(|(k, _)| k == b"values") {
                for v in values {
                    if let BVal::Bytes(item) = v {
                        peers.extend(parse_compact_peers(item));
                    }
                }
            }
        }
    }

    (peers, nodes, txn_id)
}

/// Minimal depth-limited bencode value used for KRPC response parsing.
enum BVal {
    Int(#[allow(dead_code)] i64),
    Bytes(Vec<u8>),
    List(Vec<BVal>),
    Dict(Vec<(Vec<u8>, BVal)>),
}

const MAX_KRPC_DEPTH: usize = 32;

/// Iterative-safe recursive bencode parser with a hard depth cap. Returns
/// `None` on malformed input or when `depth` exceeds [`MAX_KRPC_DEPTH`].
fn parse_bencode_value(data: &[u8], pos: &mut usize, depth: usize) -> Option<BVal> {
    if depth > MAX_KRPC_DEPTH || *pos >= data.len() {
        return None;
    }
    match data[*pos] {
        b'i' => {
            *pos += 1;
            let end = data[*pos..].iter().position(|&b| b == b'e')? + *pos;
            let s = std::str::from_utf8(&data[*pos..end]).ok()?;
            let n: i64 = s.parse().ok()?;
            *pos = end + 1;
            Some(BVal::Int(n))
        }
        b'0'..=b'9' => {
            let colon = data[*pos..].iter().position(|&b| b == b':')? + *pos;
            let s = std::str::from_utf8(&data[*pos..colon]).ok()?;
            let len: usize = s.parse().ok()?;
            let start = colon + 1;
            let end = start.checked_add(len)?;
            if end > data.len() {
                return None;
            }
            let bytes = data[start..end].to_vec();
            *pos = end;
            Some(BVal::Bytes(bytes))
        }
        b'l' => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                if *pos >= data.len() {
                    return None;
                }
                if data[*pos] == b'e' {
                    *pos += 1;
                    break;
                }
                items.push(parse_bencode_value(data, pos, depth + 1)?);
            }
            Some(BVal::List(items))
        }
        b'd' => {
            *pos += 1;
            let mut entries = Vec::new();
            loop {
                if *pos >= data.len() {
                    return None;
                }
                if data[*pos] == b'e' {
                    *pos += 1;
                    break;
                }
                match parse_bencode_value(data, pos, depth + 1)? {
                    BVal::Bytes(key) => {
                        let val = parse_bencode_value(data, pos, depth + 1)?;
                        entries.push((key, val));
                    }
                    _ => return None,
                }
            }
            Some(BVal::Dict(entries))
        }
        _ => None,
    }
}

/// Parse compact peers (6 bytes each: 4 IP + 2 port).
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

use tokio::net::UdpSocket;
use tokio::time::{timeout, Duration};

/// Public BitTorrent DHT bootstrap routers.
pub const DHT_BOOTSTRAP_NODES: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "router.utorrent.com:6881",
];

/// A BitTorrent DHT client (BEP-5).
pub struct DhtClient {
    node_id: NodeId,
    bootstrap: Vec<SocketAddr>,
}

impl DhtClient {
    pub fn new(bootstrap: Vec<SocketAddr>) -> Self {
        Self {
            node_id: NodeId::random(),
            bootstrap,
        }
    }

    /// Create a client seeded with the public bootstrap routers.
    pub fn with_default_bootstrap() -> Self {
        let mut addrs = Vec::new();
        for seed in DHT_BOOTSTRAP_NODES {
            if let Ok(iter) = std::net::ToSocketAddrs::to_socket_addrs(seed) {
                addrs.extend(iter);
            }
        }
        Self::new(addrs)
    }

    /// Perform an iterative `get_peers` lookup for an info hash.
    pub async fn get_peers(&self, info_hash: &[u8; 20]) -> Result<Vec<TrackerPeer>> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        let mut peers = Vec::new();
        let mut queried: std::collections::HashSet<SocketAddr> = std::collections::HashSet::new();
        let mut pending: Vec<SocketAddr> = self.bootstrap.clone();

        let mut tid: u16 = 0;
        while !pending.is_empty() && peers.len() < 50 {
            let node_addr = pending.remove(0);
            if !queried.insert(node_addr) {
                continue;
            }
            tid = tid.wrapping_add(1);
            let tid_bytes = tid.to_be_bytes();
            let query = build_get_peers_query(&self.node_id, info_hash, &tid_bytes);
            if socket.send_to(&query, node_addr).await.is_err() {
                continue;
            }

            let mut buf = [0u8; 65536];
            match timeout(Duration::from_secs(3), socket.recv_from(&mut buf)).await {
                Ok(Ok((len, src))) => {
                    // Only accept replies from the node we actually queried,
                    // and only when the transaction id matches our query —
                    // otherwise any internet host could inject fake peers.
                    if src != node_addr {
                        continue;
                    }
                    let (found_peers, nodes, reply_tid) = parse_dht_response(&buf[..len]);
                    if reply_tid.as_deref() != Some(&tid_bytes[..]) {
                        continue;
                    }
                    for p in found_peers {
                        if !peers.contains(&p) {
                            peers.push(p);
                        }
                    }
                    for node in nodes {
                        if !queried.contains(&node.addr) {
                            pending.push(node.addr);
                        }
                    }
                }
                _ => continue,
            }
        }

        Ok(peers)
    }

    /// Announce ourselves as a peer for an info hash.
    pub async fn announce_peer(&self, info_hash: &[u8; 20], port: u16) -> Result<()> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        for node_addr in &self.bootstrap {
            let tid = [0x61u8, 0x61u8];
            let query = build_get_peers_query(&self.node_id, info_hash, &tid);
            if socket.send_to(&query, node_addr).await.is_err() {
                continue;
            }
            let mut buf = [0u8; 65536];
            if let Ok(Ok((len, src))) =
                timeout(Duration::from_secs(3), socket.recv_from(&mut buf)).await
            {
                if src != *node_addr {
                    continue;
                }
                // Extract the token from the response (if present) and announce.
                if let Some(token) = extract_token(&buf[..len], &tid) {
                    let tid2 = [0x62u8, 0x62u8];
                    let announce =
                        build_announce_peer_query(&self.node_id, info_hash, port, &token, &tid2);
                    let _ = socket.send_to(&announce, node_addr).await;
                }
            }
        }
        Ok(())
    }
}

/// Extract the `r.token` field from a KRPC reply whose transaction id matches.
fn extract_token(data: &[u8], expected_tid: &[u8]) -> Option<Vec<u8>> {
    let (_, _, reply_tid) = parse_dht_response(data);
    if reply_tid.as_deref() != Some(expected_tid) {
        return None;
    }
    let mut pos = 0usize;
    if let Some(BVal::Dict(root)) = parse_bencode_value(data, &mut pos, 0) {
        if let Some(BVal::Dict(r)) = root.iter().find(|(k, _)| k == b"r").map(|(_, v)| v) {
            if let Some((_, BVal::Bytes(token))) = r.iter().find(|(k, _)| k == b"token") {
                return Some(token.clone());
            }
        }
    }
    None
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
        // Proper KRPC reply:
        // {"t": "aa", "y": "r", "r": {"id": <20>, "values": [<peer>]}}
        let mut resp = Vec::new();
        resp.extend_from_slice(b"d1:t2:aa1:y1:e1:rd2:id20:");
        resp.extend_from_slice(&[0u8; 20]);
        resp.extend_from_slice(b"6:valuesl6:");
        resp.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
        resp.extend_from_slice(b"eee");
        let (peers, nodes, tid) = parse_dht_response(&resp);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip, "10.0.0.1");
        assert_eq!(peers[0].port, 6881);
        assert!(nodes.is_empty());
        assert_eq!(tid.as_deref(), Some(&b"aa"[..]));
    }

    #[test]
    fn test_parse_dht_response_rejects_substring_smuggling() {
        // The old substring parser accepted "values"/"nodes" appearing inside
        // unrelated strings; the dict walker must not.
        let mut resp = Vec::new();
        resp.extend_from_slice(b"d1:t2:aa1:y1:e1:rd2:id20:");
        resp.extend_from_slice(&[0u8; 20]);
        // A token string that CONTAINS "6:values..." as raw bytes.
        // Content: x 6:values l 6: AAAAAA e = 19 bytes.
        resp.extend_from_slice(b"5:token19:x6:valuesl6:AAAAAAe");
        resp.extend_from_slice(b"ee");
        let (peers, _, _) = parse_dht_response(&resp);
        assert!(peers.is_empty());
    }

    #[test]
    fn test_parse_dht_response_nodes() {
        let mut node = Vec::new();
        node.extend_from_slice(&[0x22; 20]);
        node.extend_from_slice(&[10, 0, 0, 2]);
        node.extend_from_slice(&6882u16.to_be_bytes());
        let mut resp = Vec::new();
        resp.extend_from_slice(b"d1:t2:aa1:y1:e1:rd2:id20:");
        resp.extend_from_slice(&[0u8; 20]);
        resp.extend_from_slice(b"5:nodes");
        resp.extend_from_slice(format!("{}:", node.len()).as_bytes());
        resp.extend_from_slice(&node);
        resp.extend_from_slice(b"ee");
        let (peers, nodes, _) = parse_dht_response(&resp);
        assert!(peers.is_empty());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].addr.port(), 6882);
    }

    #[test]
    fn test_parse_dht_response_depth_bomb_safe() {
        let bomb = vec![b'd'; 50_000];
        let (peers, nodes, _) = parse_dht_response(&bomb);
        assert!(peers.is_empty());
        assert!(nodes.is_empty());
    }

    #[tokio::test]
    async fn test_dht_client_get_peers() {
        use tokio::net::UdpSocket;

        // Mock DHT node that responds to get_peers with one peer.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (n, client_addr) = server.recv_from(&mut buf).await.unwrap();
            // Reply with the SAME transaction id the query carried
            // ("1:t2:" followed by the two tid bytes at the end).
            let query = &buf[..n];
            let tid_pos = query
                .windows(5)
                .rposition(|w| w == b"1:t2:")
                .expect("query carries t2:");
            let tid = [query[tid_pos + 5], query[tid_pos + 6]];
            let mut resp = Vec::new();
            resp.extend_from_slice(b"d1:t2:");
            resp.extend_from_slice(&tid);
            resp.extend_from_slice(b"1:y1:e1:rd2:id20:");
            resp.extend_from_slice(&[0u8; 20]);
            resp.extend_from_slice(b"6:valuesl6:");
            resp.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
            resp.extend_from_slice(b"eee");
            server.send_to(&resp, client_addr).await.unwrap();
            let _ = n;
        });

        let client = DhtClient::new(vec![server_addr]);
        let peers = client.get_peers(&[0xAA; 20]).await.unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip, "10.0.0.1");

        server_task.await.unwrap();
    }
}
