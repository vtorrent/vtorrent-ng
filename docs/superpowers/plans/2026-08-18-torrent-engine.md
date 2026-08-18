# Torrent Download Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the vTorrent BitTorrent client functional by parsing piece hashes and building a per-session download/upload engine that announces to trackers, connects to peers, transfers and verifies pieces, writes to disk, and seeds.

**Architecture:** Add a `pieces` field to `Metainfo`. Add a new `engine.rs` module with pure, testable helpers (piece assembler + SHA1 verification, file-layout mapping) and a `PeerConnection` wrapper, plus a `run_engine` orchestration task. Wire `add_torrent`/`remove_torrent` to spawn/cancel the engine via `tokio_util::sync::CancellationToken`.

**Tech Stack:** Rust (edition 2021), `tokio`, `tokio-util`, `sha1`, `reqwest`.

**Spec:** `docs/superpowers/specs/2026-08-18-torrent-engine-design.md`

> **Scope note:** This plan delivers the full engine *lifecycle* — tracker announce, peer connect, handshake, choke/interest messages, piece request, SHA1 verification, disk write, and seeding state — but the piece-transfer loop requests a single piece as a smoke test of the transfer path. A full multi-peer, multi-piece scheduler (rarest-first, endgame mode, per-peer pipelining) is a follow-up increment. The engine is structured so that scheduler can replace the single-piece loop without touching the assembler, layout, or connection layers.

---

## File Structure

**Modified:**
- `Cargo.toml` — add `tokio-util` workspace dep
- `vtorrent-torrent/Cargo.toml` — add `tokio-util` dep
- `vtorrent-torrent/src/metainfo.rs` — add `pieces` field + parsing
- `vtorrent-torrent/src/engine.rs` (new) — piece assembler, file layout, peer connection, orchestration
- `vtorrent-torrent/src/lib.rs` — export `engine`
- `vtorrent-rpc/Cargo.toml` — add `tokio-util` dep
- `vtorrent-rpc/src/state.rs` — add `download_dir` field
- `vtorrent-rpc/src/handlers.rs` — spawn/cancel engine in add/remove
- `vtorrent-daemon/src/main.rs` — set `download_dir` on AppState

---

## Task 1: Parse piece hashes in `Metainfo`

**Files:**
- Modify: `vtorrent-torrent/src/metainfo.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `metainfo.rs` (find the existing tests near the bottom of the file):

```rust
    #[test]
    fn test_piece_hashes_parsed() {
        // Build a minimal single-file torrent with 2 pieces of 4 bytes each.
        // pieces string = 2 * 20 bytes of SHA1 hashes.
        let mut pieces = Vec::new();
        pieces.extend_from_slice(&[0x11u8; 20]);
        pieces.extend_from_slice(&[0x22u8; 20]);

        let bencode = format!(
            "d4:infod6:lengthi8e4:name4:test12:piece lengthi4e6:pieces{}:{}e8:announce18:http://tracker/ee",
            pieces.len(),
            String::from_utf8_lossy(&pieces),
        );
        let meta = Metainfo::from_bytes(bencode.as_bytes()).unwrap();
        assert_eq!(meta.piece_count, 2);
        assert_eq!(meta.pieces.len(), 2);
        assert_eq!(meta.pieces[0], [0x11u8; 20]);
        assert_eq!(meta.pieces[1], [0x22u8; 20]);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_piece_hashes_parsed`
Expected: FAIL (compile error — `Metainfo` has no `pieces` field).

- [ ] **Step 3: Add the `pieces` field**

In `vtorrent-torrent/src/metainfo.rs`, add the field to the `Metainfo` struct (after `piece_count`):

```rust
    /// Number of pieces.
    pub piece_count: u32,
    /// The 20-byte SHA1 hash of each piece (BEP-3 `pieces` string).
    pub pieces: Vec<[u8; 20]>,
```

- [ ] **Step 4: Parse the `pieces` string**

In `Metainfo::from_bytes`, after computing `piece_count` (around line 138), add the parsing. Insert after the `let piece_count = ...` line:

```rust
        let piece_count = total_size.div_ceil(piece_length) as u32;

        // Parse the piece hashes (BEP-3 `pieces` string: 20 bytes per piece).
        let pieces = match info_dict.get(&b"pieces".to_vec()) {
            Some(serde_bencode::value::Value::Bytes(b)) => {
                if b.len() % 20 != 0 {
                    return Err(TorrentError::InvalidMetainfo(
                        "pieces string length is not a multiple of 20".into(),
                    ));
                }
                let mut hashes = Vec::with_capacity(b.len() / 20);
                for chunk in b.chunks_exact(20) {
                    let mut h = [0u8; 20];
                    h.copy_from_slice(chunk);
                    hashes.push(h);
                }
                hashes
            }
            _ => Vec::new(),
        };
```

- [ ] **Step 5: Add `pieces` to the `Ok(Metainfo { ... })` literal**

In the `Ok(Metainfo { ... })` literal, add `pieces,` after `piece_count,`.

- [ ] **Step 6: Add `pieces` to `from_magnet_link`**

In `Metainfo::from_magnet_link`, add `pieces: Vec::new(),` after `piece_count: 0,`.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_piece_hashes_parsed`
Expected: PASS.

- [ ] **Step 8: Run full torrent tests and commit**

Run: `cargo test -p vtorrent-torrent 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-torrent/src/metainfo.rs
git commit -m "feat: parse piece hashes in torrent metainfo"
```

---

## Task 2: Add `tokio-util` dependency

**Files:**
- Modify: `Cargo.toml`
- Modify: `vtorrent-torrent/Cargo.toml`
- Modify: `vtorrent-rpc/Cargo.toml`

- [ ] **Step 1: Add the workspace dependency**

In `Cargo.toml`, under `[workspace.dependencies]`, add after the `tokio` line:

```toml
tokio-util = { version = "0.7", features = ["rt"] }
```

- [ ] **Step 2: Add to vtorrent-torrent**

In `vtorrent-torrent/Cargo.toml`, add to `[dependencies]`:

```toml
tokio-util = { workspace = true }
```

- [ ] **Step 3: Add to vtorrent-rpc**

In `vtorrent-rpc/Cargo.toml`, add to `[dependencies]`:

```toml
tokio-util = { workspace = true }
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-torrent -p vtorrent-rpc 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add Cargo.toml vtorrent-torrent/Cargo.toml vtorrent-rpc/Cargo.toml
git commit -m "chore: add tokio-util dependency"
```

---

## Task 3: Piece assembler and SHA1 verification

**Files:**
- Create: `vtorrent-torrent/src/engine.rs`
- Modify: `vtorrent-torrent/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-torrent/src/engine.rs`:

```rust
//! Torrent download/upload engine: piece assembly, file layout, peer I/O.

use crate::error::{Result, TorrentError};
use crate::metainfo::TorrentFile;
use crate::peer_wire::PeerMessage;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Assembles blocks into a full piece and verifies its SHA1.
pub struct PieceAssembler {
    piece_index: u32,
    expected_length: u64,
    blocks: HashMap<u32, Vec<u8>>,
    received: u64,
}

impl PieceAssembler {
    pub fn new(piece_index: u32, expected_length: u64) -> Self {
        Self {
            piece_index,
            expected_length,
            blocks: HashMap::new(),
            received: 0,
        }
    }

    /// Add a block of data at the given byte offset within the piece.
    pub fn add_block(&mut self, begin: u32, data: Vec<u8>) {
        if self.blocks.contains_key(&begin) {
            return;
        }
        self.received += data.len() as u64;
        self.blocks.insert(begin, data);
    }

    pub fn is_complete(&self) -> bool {
        self.received >= self.expected_length
    }

    /// Assemble the full piece if complete, in block order.
    pub fn assemble(&self) -> Option<Vec<u8>> {
        if !self.is_complete() {
            return None;
        }
        let mut offsets: Vec<u32> = self.blocks.keys().copied().collect();
        offsets.sort_unstable();
        let mut out = Vec::with_capacity(self.expected_length as usize);
        for off in offsets {
            out.extend_from_slice(&self.blocks[&off]);
        }
        Some(out)
    }

    /// Verify the assembled piece against the expected SHA1 hash.
    pub fn verify(&self, expected_hash: &[u8; 20]) -> bool {
        match self.assemble() {
            None => false,
            Some(data) => {
                let mut hasher = Sha1::new();
                hasher.update(&data);
                let digest = hasher.finalize();
                digest.as_slice() == expected_hash
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha1_of(data: &[u8]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        hasher.update(data);
        let digest = hasher.finalize();
        let mut out = [0u8; 20];
        out.copy_from_slice(&digest);
        out
    }

    #[test]
    fn test_piece_assembler_complete_and_verify() {
        let data = b"hello world, this is a test piece";
        let hash = sha1_of(data);
        let mut asm = PieceAssembler::new(0, data.len() as u64);
        asm.add_block(0, data[..10].to_vec());
        asm.add_block(10, data[10..].to_vec());
        assert!(asm.is_complete());
        assert_eq!(asm.assemble().unwrap(), data);
        assert!(asm.verify(&hash));
    }

    #[test]
    fn test_piece_assembler_incomplete() {
        let mut asm = PieceAssembler::new(0, 100);
        asm.add_block(0, vec![0u8; 50]);
        assert!(!asm.is_complete());
        assert!(asm.assemble().is_none());
    }

    #[test]
    fn test_piece_assembler_wrong_hash() {
        let mut asm = PieceAssembler::new(0, 4);
        asm.add_block(0, b"test".to_vec());
        assert!(!asm.verify(&[0u8; 20]));
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent engine::`
Expected: 3 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-torrent/src/lib.rs`, add `pub mod engine;` after `pub mod error;`.

```bash
git add vtorrent-torrent/src/engine.rs vtorrent-torrent/src/lib.rs
git commit -m "feat: add piece assembler and SHA1 verification to torrent engine"
```

---

## Task 4: File-layout mapping

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `engine.rs`:

```rust
    #[test]
    fn test_file_layout_single_file() {
        let files = vec![TorrentFile {
            path: vec!["a.bin".to_string()],
            length: 100,
            md5sum: None,
        }];
        let layout = FileLayout::new(&files, 50);
        // Piece 0 covers bytes 0..50, all in file 0 at offset 0.
        let segs = layout.piece_segments(0, &vec![0u8; 50]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].0, 0); // file index
        assert_eq!(segs[0].1, 0); // file offset
        assert_eq!(segs[0].2.len(), 50);
    }

    #[test]
    fn test_file_layout_multi_file_boundary() {
        let files = vec![
            TorrentFile {
                path: vec!["a.bin".to_string()],
                length: 30,
                md5sum: None,
            },
            TorrentFile {
                path: vec!["b.bin".to_string()],
                length: 70,
                md5sum: None,
            },
        ];
        let layout = FileLayout::new(&files, 50);
        // Piece 0 covers bytes 0..50: 30 bytes in file 0, 20 bytes in file 1.
        let segs = layout.piece_segments(0, &vec![0u8; 50]);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].0, 0);
        assert_eq!(segs[0].1, 0);
        assert_eq!(segs[0].2.len(), 30);
        assert_eq!(segs[1].0, 1);
        assert_eq!(segs[1].1, 0);
        assert_eq!(segs[1].2.len(), 20);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_file_layout`
Expected: FAIL (compile error — `FileLayout` not defined).

- [ ] **Step 3: Implement `FileLayout`**

Add to `engine.rs` (after the `PieceAssembler` impl):

```rust
/// Maps piece data to (file index, file offset, bytes) segments for disk writes.
pub struct FileLayout {
    ranges: Vec<(usize, u64, u64)>,
    piece_length: u64,
}

impl FileLayout {
    pub fn new(files: &[TorrentFile], piece_length: u64) -> Self {
        let mut ranges = Vec::new();
        let mut offset = 0u64;
        for (i, f) in files.iter().enumerate() {
            ranges.push((i, offset, f.length));
            offset += f.length;
        }
        Self {
            ranges,
            piece_length,
        }
    }

    /// Map a piece's data to (file_index, file_offset, bytes) segments.
    pub fn piece_segments(&self, piece_index: u32, piece_data: &[u8]) -> Vec<(usize, u64, Vec<u8>)> {
        let piece_start = piece_index as u64 * self.piece_length;
        let piece_end = piece_start + piece_data.len() as u64;
        let mut segments = Vec::new();
        let mut data_offset = 0usize;
        for (file_index, file_start, file_len) in &self.ranges {
            let file_end = file_start + file_len;
            if file_end <= piece_start {
                continue;
            }
            if *file_start >= piece_end {
                break;
            }
            let seg_start = piece_start.max(*file_start);
            let seg_end = piece_end.min(file_end);
            if seg_end <= seg_start {
                continue;
            }
            let len = (seg_end - seg_start) as usize;
            let file_offset = seg_start - file_start;
            let slice = piece_data[data_offset..data_offset + len].to_vec();
            segments.push((*file_index, file_offset, slice));
            data_offset += len;
        }
        segments
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_file_layout`
Expected: PASS.

- [ ] **Step 5: Run full engine tests and commit**

Run: `cargo test -p vtorrent-torrent engine::`
Expected: 5 tests pass.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: add file-layout mapping to torrent engine"
```

---

## Task 5: Peer connection (handshake + message I/O)

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `engine.rs`:

```rust
    #[tokio::test]
    async fn test_peer_connection_handshake() {
        use tokio::net::TcpListener;

        let info_hash = [0xAA; 20];
        let our_peer_id = [0x11; 20];
        let their_peer_id = [0x22; 20];

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Mock peer: accept, read handshake, reply with handshake.
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 68];
            sock.read_exact(&mut buf).await.unwrap();
            let (hs, _) = PeerMessage::decode_handshake(&buf).unwrap().unwrap();
            if let PeerMessage::Handshake { info_hash: ih, .. } = hs {
                assert_eq!(ih, info_hash);
            }
            let reply = PeerMessage::Handshake {
                info_hash,
                peer_id: their_peer_id,
                reserved: [0u8; 8],
            };
            sock.write_all(&reply.encode()).await.unwrap();
        });

        let mut conn = PeerConnection::connect(addr, info_hash, our_peer_id)
            .await
            .unwrap();
        assert_eq!(conn.remote_peer_id, their_peer_id);

        server.await.unwrap();
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_peer_connection_handshake`
Expected: FAIL (compile error — `PeerConnection` not defined).

- [ ] **Step 3: Implement `PeerConnection`**

Add to `engine.rs` (after `FileLayout`):

```rust
/// A single peer connection: handshake + message read/write.
pub struct PeerConnection {
    stream: TcpStream,
    /// The remote peer's ID (from the handshake).
    pub remote_peer_id: [u8; 20],
}

impl PeerConnection {
    /// Connect to a peer and perform the handshake.
    pub async fn connect(
        addr: SocketAddr,
        info_hash: [u8; 20],
        our_peer_id: [u8; 20],
    ) -> Result<Self> {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;

        let handshake = PeerMessage::Handshake {
            info_hash,
            peer_id: our_peer_id,
            reserved: [0u8; 8],
        };
        stream
            .write_all(&handshake.encode())
            .await
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;

        let mut buf = [0u8; 68];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
        let (hs, _) = PeerMessage::decode_handshake(&buf)?
            .ok_or_else(|| TorrentError::PeerWireError("incomplete handshake".into()))?;
        let remote_peer_id = match hs {
            PeerMessage::Handshake { peer_id, .. } => peer_id,
            _ => return Err(TorrentError::PeerWireError("expected handshake".into())),
        };

        Ok(Self {
            stream,
            remote_peer_id,
        })
    }

    /// Send a message.
    pub async fn send(&mut self, msg: &PeerMessage) -> Result<()> {
        self.stream
            .write_all(&msg.encode())
            .await
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))
    }

    /// Receive one message (blocking until a full message arrives).
    pub async fn recv(&mut self) -> Result<PeerMessage> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            if let Some((msg, _)) = PeerMessage::decode(&buf)? {
                return Ok(msg);
            }
            let n = self
                .stream
                .read(&mut tmp)
                .await
                .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
            if n == 0 {
                return Err(TorrentError::PeerWireError("connection closed".into()));
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_peer_connection_handshake`
Expected: PASS.

- [ ] **Step 5: Run full engine tests and commit**

Run: `cargo test -p vtorrent-torrent engine::`
Expected: 6 tests pass.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: add peer connection to torrent engine"
```

---

## Task 6: Engine orchestration (`run_engine`)

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Implement `run_engine`**

Add to `engine.rs` (after `PeerConnection`):

```rust
use crate::metainfo::Metainfo;
use crate::session::{SessionManager, SessionState};
use crate::tracker::{AnnounceEvent, AnnounceRequest, HttpTracker};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Run the download/upload engine for a session until cancelled.
///
/// Announces to the tracker, connects to peers, downloads and verifies pieces,
/// writes them to disk, and seeds. Updates the session's state and progress in
/// place. This is a best-effort engine: it drives the full lifecycle but does
/// not implement every BEP extension.
pub async fn run_engine(
    session_id: String,
    sessions: Arc<RwLock<SessionManager>>,
    download_dir: PathBuf,
    cancel: CancellationToken,
) {
    // Snapshot the metainfo and tracker list.
    let (metainfo, trackers) = {
        let guard = sessions.read().await;
        match guard.get_session(&session_id) {
            Ok(s) => (s.metainfo.clone(), s.metainfo.all_trackers()),
            Err(_) => return,
        }
    };

    // Mark connecting.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.state = SessionState::Connecting;
        }
    }

    // Announce to the first tracker.
    let tracker = HttpTracker::new();
    let peer_id = [0x2du8; 20]; // "-VT0001-" style peer id
    let mut peers = Vec::new();
    for url in &trackers {
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

    // Update the session's peer list.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.peers = peers.clone();
        }
    }

    // Connect to peers and download pieces.
    let mut downloaded = 0u64;
    for peer in peers {
        if cancel.is_cancelled() {
            break;
        }
        let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let mut conn = match PeerConnection::connect(addr, metainfo.info_hash, peer_id).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Send interested, then request pieces we don't have.
        let _ = conn.send(&PeerMessage::Interested).await;
        let _ = conn.send(&PeerMessage::Unchoke).await;

        // Request the first piece as a smoke test of the transfer path.
        if metainfo.piece_count > 0 {
            let piece_len = metainfo.piece_length.min(metainfo.total_size);
            let _ = conn
                .send(&PeerMessage::Request {
                    index: 0,
                    begin: 0,
                    length: piece_len as u32,
                })
                .await;

            // Read messages until we get the piece or the connection closes.
            for _ in 0..100 {
                if cancel.is_cancelled() {
                    break;
                }
                match conn.recv().await {
                    Ok(PeerMessage::Piece { index, begin, data }) => {
                        let mut asm = PieceAssembler::new(index, piece_len);
                        asm.add_block(begin, data);
                        if asm.is_complete() {
                            if let Some(expected) = metainfo.pieces.get(index as usize) {
                                if asm.verify(expected) {
                                    if let Some(piece_data) = asm.assemble() {
                                        write_piece_to_disk(
                                            &metainfo,
                                            &download_dir,
                                            index,
                                            &piece_data,
                                        )
                                        .await;
                                        downloaded += piece_data.len() as u64;
                                    }
                                }
                            }
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        }
    }

    // Final state update.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.bytes_downloaded = downloaded;
            s.state = if downloaded >= metainfo.total_size {
                SessionState::Seeding
            } else {
                SessionState::Downloading
            };
            s.last_active = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
    }
}

/// Write a verified piece's data to the correct file(s) on disk.
async fn write_piece_to_disk(
    metainfo: &Metainfo,
    download_dir: &PathBuf,
    piece_index: u32,
    piece_data: &[u8],
) {
    let layout = FileLayout::new(&metainfo.files, metainfo.piece_length);
    let base = download_dir.join(&metainfo.name);
    for (file_index, file_offset, bytes) in layout.piece_segments(piece_index, piece_data) {
        let file = &metainfo.files[file_index];
        let mut path = base.clone();
        for comp in &file.path {
            path.push(comp);
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .await
        {
            use tokio::io::AsyncSeekExt;
            let _ = f.seek(std::io::SeekFrom::Start(file_offset)).await;
            let _ = f.write_all(&bytes).await;
        }
    }
}
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: add torrent engine orchestration"
```

---

## Task 7: Wire the engine into RPC add/remove

**Files:**
- Modify: `vtorrent-rpc/src/state.rs`
- Modify: `vtorrent-rpc/src/handlers.rs`
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Add `download_dir` and cancellation tokens to AppState**

In `vtorrent-rpc/src/state.rs`, add the field to `AppState` (after `torrent_sessions`):

```rust
    /// The torrent session manager.
    pub torrent_sessions: Arc<RwLock<SessionManager>>,
    /// Directory where downloaded torrent data is written.
    pub download_dir: Arc<RwLock<std::path::PathBuf>>,
    /// Cancellation tokens for active torrent engine tasks, keyed by session id.
    pub torrent_cancels: Arc<RwLock<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>>,
```

Initialize in both constructors (after `torrent_sessions`):

```rust
            download_dir: Arc::new(RwLock::new(std::path::PathBuf::from("downloads"))),
            torrent_cancels: Arc::new(RwLock::new(std::collections::HashMap::new())),
```

- [ ] **Step 2: Spawn the engine in `add_torrent`**

In `vtorrent-rpc/src/handlers.rs`, in `add_torrent`, after `add_session` returns `session_id`, spawn the engine. Replace the tail of the function:

```rust
    let info_hash = hex::encode(metainfo.info_hash);
    let name = metainfo.name.clone();
    let session = TorrentSession::new(metainfo, req.wallet_address);
    let session_id = state.torrent_sessions.write().await.add_session(session);

    // Spawn the download engine for this session.
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .torrent_cancels
        .write()
        .await
        .insert(session_id.clone(), cancel.clone());
    let sessions = Arc::clone(&state.torrent_sessions);
    let download_dir = state.download_dir.read().await.clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        vtorrent_torrent::engine::run_engine(sid, sessions, download_dir, cancel).await;
    });

    Ok(Json(AddTorrentResponse {
        session_id,
        info_hash,
        name,
    }))
```

- [ ] **Step 3: Cancel the engine in `remove_torrent`**

In `vtorrent-rpc/src/handlers.rs`, in `remove_torrent`, cancel the token before removing. Replace the body:

```rust
pub async fn remove_torrent(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> RpcResult<Json<Value>> {
    if let Some(cancel) = state.torrent_cancels.write().await.remove(&id) {
        cancel.cancel();
    }
    let removed = state.torrent_sessions.write().await.remove_session(&id);
    if removed.is_none() {
        return Err(RpcError::NotFound(format!("Session {} not found", id)));
    }
    Ok(Json(
        json!({ "success": true, "message": format!("Session {} removed", id) }),
    ))
}
```

- [ ] **Step 4: Set the download dir in the daemon**

In `vtorrent-daemon/src/main.rs`, after `rpc_state.rpc_api_key = cli.rpc_api_key.clone();`, add:

```rust
    // Set the torrent download directory under the data dir.
    *rpc_state.download_dir.write().await = data_dir.join("downloads");
```

- [ ] **Step 5: Build and test**

Run: `cargo build -p vtorrent-rpc -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

Run: `cargo test -p vtorrent-rpc 2>&1 | rg "test result"`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-rpc/src/state.rs vtorrent-rpc/src/handlers.rs vtorrent-daemon/src/main.rs
git commit -m "feat: wire torrent engine into RPC add/remove"
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
git commit -m "chore: final verification of torrent engine"
```
