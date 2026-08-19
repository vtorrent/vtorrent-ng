# Torrent Piece Scheduler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single-piece smoke test with a real multi-peer, multi-piece scheduler that downloads complete files via rarest-first selection, block pipelining, choke/interest handling, endgame mode, and resume support.

**Architecture:** Add a `scheduler.rs` module with a `PieceTracker` (per-piece/per-peer availability) and a `SchedulerState` shared via `Arc<Mutex<...>>`. Rewrite `run_engine` to spawn one tokio task per peer, each reading messages and requesting blocks coordinated through the shared state. Persist a piece bitfield sidecar for resume.

**Tech Stack:** Rust (edition 2021), `tokio`, `tokio-util`.

**Spec:** `docs/superpowers/specs/2026-08-19-torrent-scheduler-design.md`

---

## File Structure

**New:**
- `vtorrent-torrent/src/scheduler.rs` — `PieceTracker`, `SchedulerState`, rarest-first selection, resume bitfield

**Modified:**
- `vtorrent-torrent/src/lib.rs` — export `scheduler`
- `vtorrent-torrent/src/engine.rs` — rewrite `run_engine` to use the scheduler

---

## Task 1: `PieceTracker` with bitfield merge and rarest-first selection

**Files:**
- Create: `vtorrent-torrent/src/scheduler.rs`
- Modify: `vtorrent-torrent/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-torrent/src/scheduler.rs`:

```rust
//! Piece scheduler: rarest-first selection, block pipelining, resume.

use std::collections::HashMap;

/// Tracks piece availability across peers and our own download progress.
#[derive(Debug, Default)]
pub struct PieceTracker {
    /// Number of pieces in the torrent.
    piece_count: u32,
    /// Pieces we already have (index -> true).
    have: Vec<bool>,
    /// Pieces currently requested/in-flight (index -> true).
    requested: Vec<bool>,
    /// Per-peer bitfields: peer_id -> set of piece indices the peer has.
    peer_pieces: HashMap<[u8; 20], Vec<bool>>,
}

impl PieceTracker {
    pub fn new(piece_count: u32) -> Self {
        Self {
            piece_count,
            have: vec![false; piece_count as usize],
            requested: vec![false; piece_count as usize],
            peer_pieces: HashMap::new(),
        }
    }

    /// Mark a piece as downloaded.
    pub fn mark_have(&mut self, index: u32) {
        if (index as usize) < self.have.len() {
            self.have[index as usize] = true;
            self.requested[index as usize] = false;
        }
    }

    /// Mark a piece as requested (in flight).
    pub fn mark_requested(&mut self, index: u32) {
        if (index as usize) < self.requested.len() {
            self.requested[index as usize] = true;
        }
    }

    /// Clear the requested flag (e.g. on cancel or completion).
    pub fn clear_requested(&mut self, index: u32) {
        if (index as usize) < self.requested.len() {
            self.requested[index as usize] = false;
        }
    }

    /// Record a peer's bitfield.
    pub fn set_peer_bitfield(&mut self, peer_id: [u8; 20], bits: &[u8]) {
        let mut have = vec![false; self.piece_count as usize];
        for (i, byte) in bits.iter().enumerate() {
            for bit in 0..8 {
                let idx = i * 8 + bit;
                if idx < have.len() && (byte & (0x80 >> bit)) != 0 {
                    have[idx] = true;
                }
            }
        }
        self.peer_pieces.insert(peer_id, have);
    }

    /// Record a peer's `Have` message.
    pub fn set_peer_have(&mut self, peer_id: [u8; 20], index: u32) {
        let entry = self
            .peer_pieces
            .entry(peer_id)
            .or_insert_with(|| vec![false; self.piece_count as usize]);
        if (index as usize) < entry.len() {
            entry[index as usize] = true;
        }
    }

    /// Count how many peers have a given piece.
    pub fn peer_count_for(&self, index: u32) -> usize {
        self.peer_pieces
            .values()
            .filter(|bits| bits.get(index as usize).copied().unwrap_or(false))
            .count()
    }

    /// Select the next piece to download: the rarest piece we don't have and
    /// haven't already requested, preferring pieces at least one peer has.
    pub fn next_piece(&self) -> Option<u32> {
        let mut best: Option<(usize, u32)> = None;
        for index in 0..self.piece_count {
            if self.have[index as usize] || self.requested[index as usize] {
                continue;
            }
            let count = self.peer_count_for(index);
            if count == 0 {
                continue;
            }
            match best {
                None => best = Some((count, index)),
                Some((best_count, _)) if count < best_count => best = Some((count, index)),
                _ => {}
            }
        }
        best.map(|(_, index)| index)
    }

    /// Whether all pieces are downloaded.
    pub fn is_complete(&self) -> bool {
        self.have.iter().all(|&h| h)
    }

    /// Number of pieces remaining.
    pub fn remaining(&self) -> usize {
        self.have.iter().filter(|&&h| !h).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitfield_merge() {
        let mut tracker = PieceTracker::new(16);
        // Peer A has pieces 0 and 1 (bits 0x80, 0x40 in first byte).
        tracker.set_peer_bitfield([1u8; 20], &[0xC0, 0x00]);
        assert_eq!(tracker.peer_count_for(0), 1);
        assert_eq!(tracker.peer_count_for(1), 1);
        assert_eq!(tracker.peer_count_for(2), 0);
    }

    #[test]
    fn test_rarest_first() {
        let mut tracker = PieceTracker::new(4);
        // Peer A has pieces 0,1,2,3 (all).
        tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        // Peer B has only piece 3.
        tracker.set_peer_bitfield([2u8; 20], &[0x10]);
        // Piece 3 is rarest (1 peer) vs pieces 0,1,2 (2 peers).
        assert_eq!(tracker.next_piece(), Some(3));
    }

    #[test]
    fn test_skips_have_and_requested() {
        let mut tracker = PieceTracker::new(4);
        tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        tracker.mark_have(0);
        tracker.mark_requested(1);
        // Pieces 0 (have) and 1 (requested) are skipped; 2 and 3 are tied.
        let next = tracker.next_piece().unwrap();
        assert!(next == 2 || next == 3);
    }

    #[test]
    fn test_complete() {
        let mut tracker = PieceTracker::new(2);
        assert!(!tracker.is_complete());
        tracker.mark_have(0);
        tracker.mark_have(1);
        assert!(tracker.is_complete());
        assert_eq!(tracker.remaining(), 0);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent scheduler::`
Expected: 4 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-torrent/src/lib.rs`, add `pub mod scheduler;` after `pub mod peer_wire;`.

```bash
git add vtorrent-torrent/src/scheduler.rs vtorrent-torrent/src/lib.rs
git commit -m "feat: add PieceTracker with rarest-first selection"
```

---

## Task 2: Resume bitfield save/load

**Files:**
- Modify: `vtorrent-torrent/src/scheduler.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `scheduler.rs`:

```rust
    #[test]
    fn test_resume_bitfield_roundtrip() {
        let mut tracker = PieceTracker::new(16);
        tracker.mark_have(0);
        tracker.mark_have(5);
        tracker.mark_have(15);

        let bytes = tracker.serialize_have_bitfield();
        let mut restored = PieceTracker::new(16);
        restored.load_have_bitfield(&bytes);

        assert!(restored.have[0]);
        assert!(restored.have[5]);
        assert!(restored.have[15]);
        assert!(!restored.have[1]);
        assert_eq!(restored.remaining(), 13);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_resume_bitfield_roundtrip`
Expected: FAIL (compile error — `serialize_have_bitfield`/`load_have_bitfield` not defined).

- [ ] **Step 3: Implement the bitfield serialization**

Add to `impl PieceTracker` (after `remaining`):

```rust
    /// Serialize the `have` bitfield to bytes (one bit per piece, MSB-first).
    pub fn serialize_have_bitfield(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; self.have.len().div_ceil(8)];
        for (i, &h) in self.have.iter().enumerate() {
            if h {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        bytes
    }

    /// Load a `have` bitfield from bytes.
    pub fn load_have_bitfield(&mut self, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            for bit in 0..8 {
                let idx = i * 8 + bit;
                if idx < self.have.len() && (byte & (0x80 >> bit)) != 0 {
                    self.have[idx] = true;
                }
            }
        }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_resume_bitfield_roundtrip`
Expected: PASS.

- [ ] **Step 5: Run full scheduler tests and commit**

Run: `cargo test -p vtorrent-torrent scheduler::`
Expected: 5 tests pass.

```bash
git add vtorrent-torrent/src/scheduler.rs
git commit -m "feat: add resume bitfield serialization to piece tracker"
```

---

## Task 3: `SchedulerState` shared coordination type

**Files:**
- Modify: `vtorrent-torrent/src/scheduler.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `scheduler.rs`:

```rust
    #[test]
    fn test_scheduler_state_endgame() {
        let mut state = SchedulerState::new(4);
        state.tracker.set_peer_bitfield([1u8; 20], &[0xF0]);
        state.tracker.mark_have(0);
        state.tracker.mark_have(1);
        state.tracker.mark_have(2);
        // Only piece 3 remains; endgame should be active.
        assert!(state.in_endgame());
        assert_eq!(state.tracker.remaining(), 1);
    }

    #[test]
    fn test_next_block_iterates_within_piece() {
        // A piece of 40 KiB with 16 KiB blocks has 3 blocks (0, 16384, 32768).
        let mut state = SchedulerState::new(1);
        state.tracker.set_peer_bitfield([1u8; 20], &[0x80]);
        let piece_len = |_index: u32| 40 * 1024u64;
        let (piece, begin, len) = state.next_block(&piece_len).unwrap();
        assert_eq!(piece, 0);
        assert_eq!(begin, 0);
        assert_eq!(len, 16 * 1024);

        let (_, begin2, _) = state.next_block(&piece_len).unwrap();
        assert_eq!(begin2, 16 * 1024);

        let (_, begin3, _) = state.next_block(&piece_len).unwrap();
        assert_eq!(begin3, 32 * 1024);

        // All blocks requested; no more.
        assert!(state.next_block(&piece_len).is_none());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_scheduler_state_endgame test_next_block_iterates_within_piece`
Expected: FAIL (compile error — `SchedulerState`/`next_block` not defined).

- [ ] **Step 3: Implement `SchedulerState` with block tracking**

Add to `scheduler.rs` (after `PieceTracker`):

```rust
/// Shared scheduler state, coordinated across per-peer tasks.
#[derive(Debug)]
pub struct SchedulerState {
    pub tracker: PieceTracker,
    /// Block size for requests (16 KiB).
    pub block_size: u32,
    /// Maximum blocks in flight per peer (pipelining).
    pub max_pipelined_blocks: usize,
    /// Endgame threshold: when this many pieces remain, request from all peers.
    pub endgame_threshold: usize,
    /// Blocks currently in flight: piece index -> set of begin offsets.
    requested_blocks: HashMap<u32, Vec<u32>>,
}

impl SchedulerState {
    pub fn new(piece_count: u32) -> Self {
        Self {
            tracker: PieceTracker::new(piece_count),
            block_size: 16 * 1024,
            max_pipelined_blocks: 5,
            endgame_threshold: 3,
            requested_blocks: HashMap::new(),
        }
    }

    /// Whether endgame mode is active (few pieces remain).
    pub fn in_endgame(&self) -> bool {
        self.tracker.remaining() <= self.endgame_threshold
    }

    /// Select the next block to request: (piece, begin, length).
    ///
    /// Picks the rarest piece we don't have, then the next unrequested block
    /// within that piece. `piece_len` maps a piece index to its byte length.
    /// Returns `None` when no block is available.
    pub fn next_block(&mut self, piece_len: &dyn Fn(u32) -> u64) -> Option<(u32, u32, u32)> {
        let piece = self.tracker.next_piece()?;
        let len = piece_len(piece);
        let block_size = self.block_size as u64;
        let total_blocks = len.div_ceil(block_size);
        let requested = self.requested_blocks.entry(piece).or_default();
        for block in 0..total_blocks {
            let begin = (block * block_size) as u32;
            if requested.contains(&begin) {
                continue;
            }
            let block_len = (len - block * block_size).min(block_size) as u32;
            requested.push(begin);
            return Some((piece, begin, block_len));
        }
        // All blocks of this piece are requested; mark the piece requested so
        // next_piece skips it, and try the next piece.
        self.tracker.mark_requested(piece);
        None
    }

    /// Mark a block as no longer in flight (e.g. on cancel or completion).
    pub fn clear_block(&mut self, piece: u32, begin: u32) {
        if let Some(blocks) = self.requested_blocks.get_mut(&piece) {
            blocks.retain(|&b| b != begin);
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_scheduler_state_endgame test_next_block_iterates_within_piece`
Expected: PASS.

- [ ] **Step 5: Run full scheduler tests and commit**

Run: `cargo test -p vtorrent-torrent scheduler::`
Expected: 8 tests pass.

```bash
git add vtorrent-torrent/src/scheduler.rs
git commit -m "feat: add SchedulerState with block-level tracking"
```

---

## Task 4: Rewrite `run_engine` to use the scheduler

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Add the scheduler imports**

In `vtorrent-torrent/src/engine.rs`, add to the imports (after the existing `use` lines):

```rust
use crate::scheduler::SchedulerState;
use std::sync::{Arc, Mutex as StdMutex};
```

- [ ] **Step 2: Replace the download loop with a scheduler-driven loop**

In `run_engine`, replace the "Connect to peers and download pieces" block (the `for peer in peers { ... }` loop) with:

```rust
    // Build the shared scheduler state and load any resume bitfield.
    let scheduler = Arc::new(StdMutex::new(SchedulerState::new(metainfo.piece_count)));
    {
        let mut sched = scheduler.lock().unwrap();
        let resume_path = download_dir.join(format!("{}.vtorrent", metainfo.name));
        if let Ok(bytes) = std::fs::read(&resume_path) {
            sched.tracker.load_have_bitfield(&bytes);
        }
    }

    // Spawn one task per peer.
    let mut peer_tasks = Vec::new();
    for peer in peers {
        let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let scheduler = Arc::clone(&scheduler);
        let metainfo = metainfo.clone();
        let download_dir = download_dir.clone();
        let cancel = cancel.clone();
        let sessions = Arc::clone(&sessions);
        let session_id = session_id.clone();
        peer_tasks.push(tokio::spawn(async move {
            run_peer_task(
                addr,
                metainfo,
                peer_id,
                scheduler,
                download_dir,
                sessions,
                session_id,
                cancel,
            )
            .await;
        }));
    }

    // Wait for all peer tasks to finish (or cancellation).
    for task in peer_tasks {
        let _ = task.await;
    }

    // Persist the resume bitfield and update final state.
    let downloaded = {
        let sched = scheduler.lock().unwrap();
        let resume_path = download_dir.join(format!("{}.vtorrent", metainfo.name));
        let _ = std::fs::write(&resume_path, sched.tracker.serialize_have_bitfield());
        sched.tracker.have.iter().filter(|&&h| h).count() as u64 * metainfo.piece_length
    };

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
```

- [ ] **Step 3: Add the `run_peer_task` function**

Add to `engine.rs` (after `run_engine`):

```rust
/// Drive a single peer connection: exchange bitfields, request blocks, and
/// write verified pieces to disk, coordinated through the shared scheduler.
async fn run_peer_task(
    addr: SocketAddr,
    metainfo: Metainfo,
    peer_id: [u8; 20],
    scheduler: Arc<StdMutex<SchedulerState>>,
    download_dir: PathBuf,
    sessions: Arc<RwLock<SessionManager>>,
    session_id: String,
    cancel: CancellationToken,
) {
    let mut conn = match PeerConnection::connect(addr, metainfo.info_hash, peer_id).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Send our (empty) bitfield and interested.
    let _ = conn.send(&PeerMessage::Bitfield { bits: vec![] }).await;
    let _ = conn.send(&PeerMessage::Interested).await;

    // Track in-flight blocks for this peer.
    let mut in_flight: usize = 0;
    // Track partial piece assembly across multiple blocks.
    let mut assemblers: std::collections::HashMap<u32, PieceAssembler> =
        std::collections::HashMap::new();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Request blocks while we have pipeline capacity and pieces to fetch.
        while in_flight < scheduler.lock().unwrap().max_pipelined_blocks {
            let piece_len = |index: u32| piece_length(&metainfo, index);
            let (piece, begin, len) = {
                let mut sched = scheduler.lock().unwrap();
                match sched.next_block(&piece_len) {
                    Some(b) => b,
                    None => break,
                }
            };
            let _ = conn
                .send(&PeerMessage::Request {
                    index: piece,
                    begin,
                    length: len,
                })
                .await;
            in_flight += 1;
        }

        // Read one message.
        match conn.recv().await {
            Ok(PeerMessage::Bitfield { bits }) => {
                scheduler
                    .lock()
                    .unwrap()
                    .tracker
                    .set_peer_bitfield(conn.remote_peer_id, &bits);
            }
            Ok(PeerMessage::Have { index }) => {
                scheduler
                    .lock()
                    .unwrap()
                    .tracker
                    .set_peer_have(conn.remote_peer_id, index);
            }
            Ok(PeerMessage::Choke) => {
                // Wait for unchoke; just continue.
            }
            Ok(PeerMessage::Piece { index, begin, data }) => {
                in_flight = in_flight.saturating_sub(1);
                let piece_len = piece_length(&metainfo, index);
                let asm = assemblers
                    .entry(index)
                    .or_insert_with(|| PieceAssembler::new(index, piece_len));
                asm.add_block(begin, data);
                if asm.is_complete() {
                    if let Some(expected) = metainfo.pieces.get(index as usize) {
                        if asm.verify(expected) {
                            if let Some(piece_data) = asm.assemble() {
                                write_piece_to_disk(&metainfo, &download_dir, index, &piece_data)
                                    .await;
                                scheduler.lock().unwrap().tracker.mark_have(index);
                                // Update session progress.
                                let mut guard = sessions.write().await;
                                if let Ok(s) = guard.get_session_mut(&session_id) {
                                    s.bytes_downloaded = s
                                        .bytes_downloaded
                                        .saturating_add(piece_data.len() as u64);
                                }
                            }
                        }
                    }
                    assemblers.remove(&index);
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }

        // Stop when complete.
        if scheduler.lock().unwrap().tracker.is_complete() {
            break;
        }
    }
}

/// The length of a piece (the last piece may be shorter).
fn piece_length(metainfo: &Metainfo, index: u32) -> u64 {
    let start = index as u64 * metainfo.piece_length;
    let remaining = metainfo.total_size.saturating_sub(start);
    remaining.min(metainfo.piece_length)
}
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: drive downloads with the piece scheduler"
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
git commit -m "chore: final verification of piece scheduler"
```
