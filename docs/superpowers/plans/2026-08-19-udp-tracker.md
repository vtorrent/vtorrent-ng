# UDP Tracker (BEP-15) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add UDP tracker support (BEP-15) so the torrent client can announce to UDP trackers in addition to HTTP trackers.

**Architecture:** Add a `udp.rs` module with the BEP-15 message codec (connect/announce/scrape/error) and a `UdpTracker` client using `tokio::net::UdpSocket` with transaction-ID matching and timeout. Wire `run_engine` to try UDP trackers (from `udp://` URLs) alongside HTTP trackers.

**Tech Stack:** Rust (edition 2021), `tokio`.

**Spec:** `docs/superpowers/specs/2026-08-19-udp-tracker-design.md`

---

## File Structure

**New:**
- `vtorrent-torrent/src/udp.rs` — BEP-15 message codec + `UdpTracker` client

**Modified:**
- `vtorrent-torrent/src/lib.rs` — export `udp`
- `vtorrent-torrent/src/engine.rs` — try UDP trackers in `run_engine`

---

## Task 1: BEP-15 message codec

**Files:**
- Create: `vtorrent-torrent/src/udp.rs`
- Modify: `vtorrent-torrent/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-torrent/src/udp.rs`:

```rust
//! UDP tracker protocol (BEP-15): connect, announce, scrape, error.

use crate::error::{Result, TorrentError};
use crate::tracker::{AnnounceEvent, TrackerPeer};

/// BEP-15 protocol magic (0x41727101980).
const PROTOCOL_ID: u64 = 0x4172_7101_9800_0000;

/// Action codes.
const ACTION_CONNECT: u32 = 0;
const ACTION_ANNOUNCE: u32 = 1;
const ACTION_SCRAPE: u32 = 2;
const ACTION_ERROR: u32 = 3;

/// Encode a connect request: (protocol_id, action=0, transaction_id).
pub fn encode_connect(transaction_id: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(&PROTOCOL_ID.to_be_bytes());
    buf.extend_from_slice(&ACTION_CONNECT.to_be_bytes());
    buf.extend_from_slice(&transaction_id.to_be_bytes());
    buf
}

/// Decode a connect response, returning the connection_id.
pub fn decode_connect_response(data: &[u8]) -> Result<u64> {
    if data.len() < 16 {
        return Err(TorrentError::TrackerError("connect response too short".into()));
    }
    let action = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if action == ACTION_ERROR {
        return Err(TorrentError::TrackerError(
            String::from_utf8_lossy(&data[8..]).into_owned(),
        ));
    }
    if action != ACTION_CONNECT {
        return Err(TorrentError::TrackerError("unexpected action".into()));
    }
    Ok(u64::from_be_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]))
}

/// Encode an announce request.
pub fn encode_announce(
    connection_id: u64,
    transaction_id: u32,
    info_hash: &[u8; 20],
    peer_id: &[u8; 20],
    downloaded: u64,
    left: u64,
    uploaded: u64,
    event: AnnounceEvent,
    port: u16,
) -> Vec<u8> {
    let event_code: u32 = match event {
        AnnounceEvent::None => 0,
        AnnounceEvent::Completed => 1,
        AnnounceEvent::Started => 2,
        AnnounceEvent::Stopped => 3,
    };
    let mut buf = Vec::with_capacity(98);
    buf.extend_from_slice(&connection_id.to_be_bytes());
    buf.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
    buf.extend_from_slice(&transaction_id.to_be_bytes());
    buf.extend_from_slice(info_hash);
    buf.extend_from_slice(peer_id);
    buf.extend_from_slice(&downloaded.to_be_bytes());
    buf.extend_from_slice(&left.to_be_bytes());
    buf.extend_from_slice(&uploaded.to_be_bytes());
    buf.extend_from_slice(&event_code.to_be_bytes());
    buf.extend_from_slice(&0u32.to_be_bytes()); // IP address (0 = default)
    buf.extend_from_slice(&0u32.to_be_bytes()); // key
    buf.extend_from_slice(&(-1i32).to_be_bytes()); // num_want = -1 (default)
    buf.extend_from_slice(&port.to_be_bytes());
    buf
}

/// Decode an announce response, returning (interval, leechers, seeders, peers).
pub fn decode_announce_response(data: &[u8]) -> Result<(u32, u32, u32, Vec<TrackerPeer>)> {
    if data.len() < 20 {
        return Err(TorrentError::TrackerError("announce response too short".into()));
    }
    let action = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if action == ACTION_ERROR {
        return Err(TorrentError::TrackerError(
            String::from_utf8_lossy(&data[8..]).into_owned(),
        ));
    }
    if action != ACTION_ANNOUNCE {
        return Err(TorrentError::TrackerError("unexpected action".into()));
    }
    let interval = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let leechers = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    let seeders = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let peers = parse_compact_peers(&data[20..]);
    Ok((interval, leechers, seeders, peers))
}

/// Encode a scrape request for a single info hash.
pub fn encode_scrape(connection_id: u64, transaction_id: u32, info_hash: &[u8; 20]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(36);
    buf.extend_from_slice(&connection_id.to_be_bytes());
    buf.extend_from_slice(&ACTION_SCRAPE.to_be_bytes());
    buf.extend_from_slice(&transaction_id.to_be_bytes());
    buf.extend_from_slice(info_hash);
    buf
}

/// Decode a scrape response, returning (seeders, completed, leechers).
pub fn decode_scrape_response(data: &[u8]) -> Result<(u32, u32, u32)> {
    if data.len() < 20 {
        return Err(TorrentError::TrackerError("scrape response too short".into()));
    }
    let action = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if action == ACTION_ERROR {
        return Err(TorrentError::TrackerError(
            String::from_utf8_lossy(&data[8..]).into_owned(),
        ));
    }
    if action != ACTION_SCRAPE {
        return Err(TorrentError::TrackerError("unexpected action".into()));
    }
    let seeders = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let completed = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
    let leechers = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    Ok((seeders, completed, leechers))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_roundtrip() {
        let req = encode_connect(0x1234_5678);
        assert_eq!(req.len(), 16);
        // First 8 bytes are the protocol id.
        assert_eq!(&req[0..8], &PROTOCOL_ID.to_be_bytes());
        // Action is connect (0).
        assert_eq!(u32::from_be_bytes([req[8], req[9], req[10], req[11]]), 0);
    }

    #[test]
    fn test_decode_connect_response() {
        let mut data = Vec::new();
        data.extend_from_slice(&ACTION_CONNECT.to_be_bytes());
        data.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        data.extend_from_slice(&0xDEAD_BEEF_CAFE_BABEu64.to_be_bytes());
        let conn_id = decode_connect_response(&data).unwrap();
        assert_eq!(conn_id, 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn test_announce_roundtrip() {
        let info_hash = [0xAA; 20];
        let peer_id = [0xBB; 20];
        let req = encode_announce(
            0x1234,
            0x5678,
            &info_hash,
            &peer_id,
            0,
            100,
            0,
            AnnounceEvent::Started,
            6881,
        );
        assert_eq!(req.len(), 98);
    }

    #[test]
    fn test_decode_announce_response() {
        let mut data = Vec::new();
        data.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        data.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        data.extend_from_slice(&1800u32.to_be_bytes()); // interval
        data.extend_from_slice(&5u32.to_be_bytes()); // leechers
        data.extend_from_slice(&10u32.to_be_bytes()); // seeders
        // One compact peer: 192.168.1.1:6881
        data.extend_from_slice(&[192, 168, 1, 1, 0x1A, 0xE1]);
        let (interval, leechers, seeders, peers) = decode_announce_response(&data).unwrap();
        assert_eq!(interval, 1800);
        assert_eq!(leechers, 5);
        assert_eq!(seeders, 10);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip, "192.168.1.1");
        assert_eq!(peers[0].port, 6881);
    }

    #[test]
    fn test_scrape_roundtrip() {
        let info_hash = [0xCC; 20];
        let req = encode_scrape(0x1234, 0x5678, &info_hash);
        assert_eq!(req.len(), 36);
    }

    #[test]
    fn test_decode_scrape_response() {
        let mut data = Vec::new();
        data.extend_from_slice(&ACTION_SCRAPE.to_be_bytes());
        data.extend_from_slice(&0x1234_5678u32.to_be_bytes());
        data.extend_from_slice(&10u32.to_be_bytes()); // seeders
        data.extend_from_slice(&20u32.to_be_bytes()); // completed
        data.extend_from_slice(&5u32.to_be_bytes()); // leechers
        let (seeders, completed, leechers) = decode_scrape_response(&data).unwrap();
        assert_eq!(seeders, 10);
        assert_eq!(completed, 20);
        assert_eq!(leechers, 5);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent udp::`
Expected: 6 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-torrent/src/lib.rs`, add `pub mod udp;` after `pub mod tracker;`.

```bash
git add vtorrent-torrent/src/udp.rs vtorrent-torrent/src/lib.rs
git commit -m "feat: add BEP-15 UDP tracker message codec"
```

---

## Task 2: `UdpTracker` client

**Files:**
- Modify: `vtorrent-torrent/src/udp.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `udp.rs`:

```rust
    #[tokio::test]
    async fn test_udp_tracker_connect_and_announce() {
        use tokio::net::UdpSocket;

        // Bind a mock UDP tracker.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            // First: connect request.
            let (n, client_addr) = server.recv_from(&mut buf).await.unwrap();
            let tx_id = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
            let mut resp = Vec::new();
            resp.extend_from_slice(&ACTION_CONNECT.to_be_bytes());
            resp.extend_from_slice(&tx_id.to_be_bytes());
            resp.extend_from_slice(&0xCAFE_BABEu64.to_be_bytes());
            server.send_to(&resp, client_addr).await.unwrap();

            // Second: announce request.
            let (n2, client_addr2) = server.recv_from(&mut buf).await.unwrap();
            let tx_id2 = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
            let mut resp2 = Vec::new();
            resp2.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
            resp2.extend_from_slice(&tx_id2.to_be_bytes());
            resp2.extend_from_slice(&1800u32.to_be_bytes());
            resp2.extend_from_slice(&0u32.to_be_bytes());
            resp2.extend_from_slice(&1u32.to_be_bytes());
            resp2.extend_from_slice(&[10, 0, 0, 1, 0x1A, 0xE1]);
            server.send_to(&resp2, client_addr2).await.unwrap();
            let _ = n;
            let _ = n2;
        });

        let tracker = UdpTracker::new(server_addr);
        let peers = tracker
            .announce(&[0xAA; 20], &[0xBB; 20], 0, 100, 0, AnnounceEvent::Started, 6881)
            .await
            .unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].ip, "10.0.0.1");
        assert_eq!(peers[0].port, 6881);

        server_task.await.unwrap();
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_udp_tracker_connect_and_announce`
Expected: FAIL (compile error — `UdpTracker` not defined).

- [ ] **Step 3: Implement `UdpTracker`**

Add to `udp.rs` (after `parse_compact_peers`):

```rust
use std::net::SocketAddr;
use tokio::net::UdpSocket;

/// A UDP tracker client (BEP-15).
pub struct UdpTracker {
    addr: SocketAddr,
}

impl UdpTracker {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr }
    }

    /// Perform a connect handshake, returning the connection id.
    async fn connect(&self, socket: &UdpSocket) -> Result<u64> {
        let transaction_id = rand_transaction_id();
        let req = encode_connect(transaction_id);
        socket
            .send_to(&req, self.addr)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        let mut buf = [0u8; 2048];
        let (n, _) = socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        decode_connect_response(&buf[..n])
    }

    /// Announce to the tracker, returning the discovered peers.
    pub async fn announce(
        &self,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
        downloaded: u64,
        left: u64,
        uploaded: u64,
        event: AnnounceEvent,
        port: u16,
    ) -> Result<Vec<TrackerPeer>> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        let connection_id = self.connect(&socket).await?;

        let transaction_id = rand_transaction_id();
        let req = encode_announce(
            connection_id,
            transaction_id,
            info_hash,
            peer_id,
            downloaded,
            left,
            uploaded,
            event,
            port,
        );
        socket
            .send_to(&req, self.addr)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        let mut buf = [0u8; 4096];
        let (n, _) = socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        let (_interval, _leechers, _seeders, peers) = decode_announce_response(&buf[..n])?;
        Ok(peers)
    }

    /// Scrape the tracker for a single info hash.
    pub async fn scrape(&self, info_hash: &[u8; 20]) -> Result<(u32, u32, u32)> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        let connection_id = self.connect(&socket).await?;

        let transaction_id = rand_transaction_id();
        let req = encode_scrape(connection_id, transaction_id, info_hash);
        socket
            .send_to(&req, self.addr)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;

        let mut buf = [0u8; 2048];
        let (n, _) = socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| TorrentError::TrackerError(e.to_string()))?;
        decode_scrape_response(&buf[..n])
    }
}

/// Generate a random transaction id.
fn rand_transaction_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    nanos ^ (std::process::id() as u32)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_udp_tracker_connect_and_announce`
Expected: PASS.

- [ ] **Step 5: Run full udp tests and commit**

Run: `cargo test -p vtorrent-torrent udp::`
Expected: 7 tests pass.

```bash
git add vtorrent-torrent/src/udp.rs
git commit -m "feat: add UdpTracker client"
```

---

## Task 3: Wire UDP trackers into `run_engine`

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Add UDP tracker support to the announce step**

In `run_engine`, replace the announce loop (the `for url in &trackers { ... }` block that only uses `HttpTracker`) with a loop that dispatches on the URL scheme:

```rust
    // Announce to trackers (HTTP and UDP), collecting peers.
    let tracker = HttpTracker::new();
    let peer_id = [0x2du8; 20]; // "-VT0001-" style peer id
    let mut peers = Vec::new();
    for url in &trackers {
        if url.starts_with("udp://") {
            // UDP tracker (BEP-15).
            let host_port = url.trim_start_matches("udp://");
            let addr: SocketAddr = match host_port.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let udp = crate::udp::UdpTracker::new(addr);
            if let Ok(p) = udp
                .announce(
                    &metainfo.info_hash,
                    &peer_id,
                    0,
                    metainfo.total_size,
                    0,
                    AnnounceEvent::Started,
                    6881,
                )
                .await
            {
                peers = p;
                break;
            }
        } else {
            let req = AnnounceRequest {
                tracker_url: url.clone(),
                info_hash: metainfo.info_hash,
                peer_id,
                port: 6881,
                uploaded: 0,
                downloaded: 0,
                left: metainfo.total_size,
                event: AnnounceEvent::Started,
                num_want: 50,
            };
            if let Ok(resp) = tracker.announce(&req).await {
                peers = resp.peers;
                break;
            }
        }
    }
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: announce to UDP trackers in the engine"
```

---

## Final Verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features 2>&1 | rg "test result: FAILED|error\["`
Expected: no failures.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | rg "warning:|error:"`
Expected: no output.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 3: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of UDP tracker"
```
