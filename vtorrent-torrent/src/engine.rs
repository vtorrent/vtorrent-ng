//! Torrent download/upload engine: piece assembly, file layout, peer I/O.

use crate::engine_disk::{
    piece_length, piece_on_disk_matches, read_block_from_disk, resume_file_path,
    write_piece_to_disk,
};
use crate::error::{Result, TorrentError};
use crate::metainfo::{Metainfo, TorrentFile};
use crate::peer_wire::{PeerMessage, MAX_MESSAGE_LENGTH};
use crate::scheduler::SchedulerState;
use crate::session::{SessionManager, SessionState};
use crate::tracker::{tracker_target_allowed_with, AnnounceEvent, AnnounceRequest, HttpTracker};
use serde_bencode::value::Value;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

struct PieceWriteClaim {
    scheduler: Arc<StdMutex<SchedulerState>>,
    index: u32,
}

impl PieceWriteClaim {
    fn acquire(scheduler: Arc<StdMutex<SchedulerState>>, index: u32) -> Option<Self> {
        {
            let mut state = scheduler.lock().unwrap_or_else(|e| e.into_inner());
            if state.has_piece(index) || !state.writing_pieces.insert(index) {
                return None;
            }
        }
        Some(Self { scheduler, index })
    }

    fn finish(self) {
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_have(self.index);
    }
}

impl Drop for PieceWriteClaim {
    fn drop(&mut self) {
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .writing_pieces
            .remove(&self.index);
    }
}

/// Tracker-supplied peers must pass the same address policy as magnet metadata
/// peers: no loopback, link-local, private, or zero-port targets.
fn peer_target_allowed(peer: &crate::tracker::TrackerPeer, allow_private: bool) -> bool {
    peer.port != 0
        && peer
            .ip
            .parse()
            .map(|ip| tracker_target_allowed_with(ip, allow_private))
            .unwrap_or(false)
}
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// BEP-10 extension bit: reserved[5] bit 0x10.
const EXTENSION_RESERVED: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00];

/// Maximum accepted blocks per piece. Bounds the map overhead a peer can
/// create by sending many tiny, non-overlapping blocks.
const MAX_BLOCKS_PER_PIECE: usize = 4096;

/// Peer entries retained on the session, and peer tasks spawned per torrent.
/// Both are bounded because tracker responses are untrusted.
const MAX_SESSION_PEERS: usize = 200;
const MAX_PEERS: usize = 200;

/// Per-peer assembly budget. Pieces are verified as a whole, so a fixed
/// assembler *count* either starves a normal 256 KiB piece or lets a 16 MiB
/// piece hold the same count. Budget by bytes instead.
const MAX_ASSEMBLER_BYTES: usize = 8 * 1024 * 1024;
const MIN_ASSEMBLERS: usize = 8;
const MAX_ASSEMBLERS: usize = 64;

/// Assembles blocks into a full piece and verifies its SHA1.
pub struct PieceAssembler {
    expected_length: u64,
    blocks: HashMap<u32, Vec<u8>>,
    ranges: Vec<(u32, u32)>,
    received: u64,
}

impl PieceAssembler {
    pub fn new(_piece_index: u32, expected_length: u64) -> Self {
        Self {
            expected_length,
            blocks: HashMap::new(),
            ranges: Vec::new(),
            received: 0,
        }
    }

    /// Add a block of data at the given byte offset within the piece.
    ///
    /// Returns `true` if the block was accepted, or `false` if it was rejected
    /// (duplicate offset or an offset/length that would run past the end of the
    /// piece). Rejecting out-of-range blocks prevents a malicious peer from
    /// inflating `received` or producing a piece larger than `expected_length`.
    pub fn add_block(&mut self, begin: u32, data: Vec<u8>) -> bool {
        if data.is_empty() {
            return false;
        }
        if self.blocks.len() >= MAX_BLOCKS_PER_PIECE {
            return false;
        }
        let end = begin as u64 + data.len() as u64;
        if end > self.expected_length {
            return false;
        }
        if self.ranges.iter().any(|&(start, existing_end)| {
            (begin as u64) < existing_end as u64 && (start as u64) < end
        }) {
            return false;
        }
        self.ranges.push((begin, end as u32));
        self.received += data.len() as u64;
        self.blocks.insert(begin, data);
        true
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
            // saturating: a crafted metainfo with huge per-file lengths must
            // not panic the layout build under overflow-checks.
            offset = offset.saturating_add(f.length);
        }
        Self {
            ranges,
            piece_length,
        }
    }

    /// Map a piece's data to (file_index, file_offset, bytes) segments.
    pub fn piece_segments(
        &self,
        piece_index: u32,
        piece_data: &[u8],
    ) -> Vec<(usize, u64, Vec<u8>)> {
        let piece_start = (piece_index as u64).saturating_mul(self.piece_length);
        let piece_end = piece_start.saturating_add(piece_data.len() as u64);
        let mut segments = Vec::new();
        let mut data_offset = 0usize;
        for (file_index, file_start, file_len) in &self.ranges {
            let file_end = file_start.saturating_add(*file_len);
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

    /// Map a piece of a known length to `(file_index, file_offset, byte_count)`
    /// ranges, without requiring the piece bytes in memory.
    pub fn piece_segment_ranges(&self, piece_index: u32, piece_len: u64) -> Vec<(usize, u64, u64)> {
        let piece_start = piece_index as u64 * self.piece_length;
        let piece_end = piece_start + piece_len;
        let mut segments = Vec::new();
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
            segments.push((*file_index, seg_start - file_start, seg_end - seg_start));
        }
        segments
    }
}

/// Releases every assembly-budget reservation held by one peer task when the
/// task exits, including on cancellation or error paths.
struct AssemblyBudget {
    scheduler: Arc<StdMutex<SchedulerState>>,
    reserved: usize,
}

impl AssemblyBudget {
    fn new(scheduler: Arc<StdMutex<SchedulerState>>) -> Self {
        Self {
            scheduler,
            reserved: 0,
        }
    }

    fn reserve(&mut self, piece_len: u64) -> bool {
        let piece_len = piece_len as usize;
        if !self
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve_assembler(piece_len as u64)
        {
            return false;
        }
        self.reserved = self.reserved.saturating_add(piece_len);
        true
    }

    fn release(&mut self, piece_len: u64) {
        let amount = self.reserved.min(piece_len as usize);
        if amount == 0 {
            return;
        }
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_assembler(amount as u64);
        self.reserved -= amount;
    }
}

impl Drop for AssemblyBudget {
    fn drop(&mut self) {
        if self.reserved == 0 {
            return;
        }
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_assembler(self.reserved as u64);
        self.reserved = 0;
    }
}

/// Releases per-piece ownership held by a peer task when it exits, so another
/// task can fetch a piece whose partial assembler was lost.
struct PieceOwnership {
    scheduler: Arc<StdMutex<SchedulerState>>,
    task_id: [u8; 20],
}

impl PieceOwnership {
    fn new(scheduler: Arc<StdMutex<SchedulerState>>, task_id: [u8; 20]) -> Self {
        Self { scheduler, task_id }
    }
}

impl Drop for PieceOwnership {
    fn drop(&mut self) {
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .release_owned(self.task_id);
    }
}

/// Timeouts for the peer-wire socket. A malicious tracker or peer must not be
/// able to pin a task forever in connect, handshake, or write.
const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// A single peer connection: handshake + message read/write.
pub struct PeerConnection {
    stream: TcpStream,
    /// The remote peer's ID (from the handshake).
    pub remote_peer_id: [u8; 20],
    /// Bytes read past the current message. TCP is a stream, so one read can
    /// contain several peer-wire frames (for example a burst of `Piece`
    /// messages). Those bytes must survive until the next `recv` call.
    read_buf: Vec<u8>,
    /// Cursor into `read_buf`. Advancing a cursor avoids shifting the whole
    /// buffer for every small frame (a keep-alive flood would otherwise be
    /// quadratic in the buffered byte count).
    read_pos: usize,
    /// Largest frame this connection will buffer. The engine tightens this to
    /// the torrent's real piece/bitfield size after the handshake.
    max_message_len: usize,
}

impl PeerConnection {
    /// Connect to a peer and perform the handshake.
    pub async fn connect(
        addr: SocketAddr,
        info_hash: [u8; 20],
        our_peer_id: [u8; 20],
    ) -> Result<Self> {
        let mut stream = tokio::time::timeout(PEER_CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer connect timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;

        let handshake = PeerMessage::Handshake {
            info_hash,
            peer_id: our_peer_id,
            reserved: EXTENSION_RESERVED,
        };
        tokio::time::timeout(PEER_CONNECT_TIMEOUT, stream.write_all(&handshake.encode()))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer handshake write timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;

        let mut buf = [0u8; 68];
        tokio::time::timeout(PEER_CONNECT_TIMEOUT, stream.read_exact(&mut buf))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer handshake read timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
        let (hs, _) = PeerMessage::decode_handshake(&buf)?
            .ok_or_else(|| TorrentError::PeerWireError("incomplete handshake".into()))?;
        let remote_peer_id = match hs {
            PeerMessage::Handshake {
                peer_id,
                info_hash: their_info_hash,
                ..
            } => {
                // Protocol contract: the peer must echo OUR info-hash. A
                // handshake for a different torrent must not proceed into
                // bitfield/piece exchange.
                if their_info_hash != info_hash {
                    return Err(TorrentError::PeerWireError(
                        "handshake info-hash mismatch".into(),
                    ));
                }
                peer_id
            }
            _ => return Err(TorrentError::PeerWireError("expected handshake".into())),
        };

        Ok(Self {
            stream,
            remote_peer_id,
            read_buf: Vec::new(),
            read_pos: 0,
            max_message_len: MAX_MESSAGE_LENGTH,
        })
    }

    /// Accept an inbound peer and perform the server side of the handshake.
    ///
    /// The inbound peer chooses the torrent in its handshake, so the caller
    /// looks up the session and either replies with the matching info-hash or
    /// drops the connection. Nothing is sent back until `info_hash` is known to
    /// belong to a torrent we actually hold.
    pub async fn accept(mut stream: TcpStream) -> Result<([u8; 20], [u8; 20], Self)> {
        let mut buf = [0u8; 68];
        tokio::time::timeout(PEER_CONNECT_TIMEOUT, stream.read_exact(&mut buf))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer handshake read timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
        let (hs, _) = PeerMessage::decode_handshake(&buf)?
            .ok_or_else(|| TorrentError::PeerWireError("incomplete handshake".into()))?;
        let (info_hash, peer_id, reserved) = match hs {
            PeerMessage::Handshake {
                info_hash,
                peer_id,
                reserved,
            } => (info_hash, peer_id, reserved),
            _ => return Err(TorrentError::PeerWireError("expected handshake".into())),
        };
        // Refuse to talk to a client that does not advertise BEP-10: without
        // the extension handshake there is no VTR address and no receipt, so
        // such a peer could only ever consume upload bandwidth unpaid.
        if reserved[5] & 0x10 == 0 {
            return Err(TorrentError::PeerWireError(
                "peer does not support the extension protocol".into(),
            ));
        }
        Ok((
            info_hash,
            peer_id,
            Self {
                stream,
                remote_peer_id: peer_id,
                read_buf: Vec::new(),
                read_pos: 0,
                max_message_len: MAX_MESSAGE_LENGTH,
            },
        ))
    }

    /// Complete an inbound handshake by echoing `info_hash` and our peer id.
    pub async fn accept_reply(&mut self, info_hash: [u8; 20], our_peer_id: [u8; 20]) -> Result<()> {
        let reply = PeerMessage::Handshake {
            info_hash,
            peer_id: our_peer_id,
            reserved: EXTENSION_RESERVED,
        };
        tokio::time::timeout(PEER_CONNECT_TIMEOUT, self.stream.write_all(&reply.encode()))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer handshake write timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))
    }

    /// Tighten the maximum frame size for this connection. Never raises the
    /// absolute wire limit.
    pub fn set_max_message_len(&mut self, max: usize) {
        self.max_message_len = max.clamp(1, MAX_MESSAGE_LENGTH);
    }

    /// Send a message.
    pub async fn send(&mut self, msg: &PeerMessage) -> Result<()> {
        let bytes = msg.encode();
        tokio::time::timeout(PEER_SEND_TIMEOUT, self.stream.write_all(&bytes))
            .await
            .map_err(|_| TorrentError::PeerWireError("peer send timed out".into()))?
            .map_err(|e| TorrentError::PeerWireError(e.to_string()))
    }

    /// Receive one message (blocking until a full message arrives).
    pub async fn recv(&mut self) -> Result<PeerMessage> {
        const RECEIVE_DEADLINE: Duration = Duration::from_secs(180);
        let deadline = tokio::time::Instant::now() + RECEIVE_DEADLINE;
        let mut tmp = [0u8; 4096];
        loop {
            if let Some((msg, consumed)) = PeerMessage::decode_with_limit(
                &self.read_buf[self.read_pos..],
                self.max_message_len,
            )? {
                self.read_pos += consumed;
                if self.read_pos == self.read_buf.len() {
                    self.read_buf.clear();
                    self.read_pos = 0;
                } else if self.read_pos >= 64 * 1024 {
                    self.read_buf.drain(..self.read_pos);
                    self.read_pos = 0;
                }
                return Ok(msg);
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TorrentError::PeerWireError("peer receive timed out".into()));
            }
            let n = tokio::time::timeout(remaining, self.stream.read(&mut tmp))
                .await
                .map_err(|_| TorrentError::PeerWireError("peer receive timed out".into()))?
                .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
            if n == 0 {
                return Err(TorrentError::PeerWireError("connection closed".into()));
            }
            self.read_buf.extend_from_slice(&tmp[..n]);
        }
    }
}

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
    limits: Arc<crate::limits::TorrentRateLimiter>,
    seed_ratio_target: Arc<RwLock<f64>>,
    cancel: CancellationToken,
    local_wif: Option<zeroize::Zeroizing<String>>,
) {
    // Snapshot the metainfo and tracker list.
    let (metainfo, trackers) = {
        let guard = sessions.read().await;
        match guard.get_session(&session_id) {
            Ok(s) => (s.metainfo.clone(), s.metainfo.all_trackers()),
            Err(_) => return,
        }
    };
    let receipt_key = local_wif
        .as_ref()
        .and_then(|wif| vtorrent_core::keys::PrivateKey::from_wif(wif).ok())
        .map(Arc::new);

    // Mark connecting.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.state = SessionState::Connecting;
        }
    }

    // Announce to trackers (HTTP and UDP), collecting peers. Trackers are
    // untrusted and each attempt has its own timeout, so cap the number of
    // sequential attempts and the whole prelude's wall-clock time.
    const MAX_TRACKER_ATTEMPTS: usize = 8;
    const ANNOUNCE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
    let announce_deadline = tokio::time::Instant::now() + ANNOUNCE_DEADLINE;
    let peer_id = crate::peer_wire::random_peer_id(); // unique per session/process
    let trackers: Vec<String> = trackers.into_iter().take(MAX_TRACKER_ATTEMPTS).collect();
    let mut peers = match tokio::time::timeout_at(
        announce_deadline,
        announce_trackers(&trackers, &metainfo, peer_id),
    )
    .await
    {
        Ok(Some(p)) => p,
        Ok(None) => {
            let mut guard = sessions.write().await;
            if let Ok(s) = guard.get_session_mut(&session_id) {
                s.state = SessionState::Stopped;
            }
            return;
        }
        Err(_) => {
            tracing::warn!("Torrent tracker announce timed out; continuing without peers");
            Vec::new()
        }
    };

    // If no peers were found via trackers, fall back to DHT (BEP-5).
    if peers.is_empty() {
        let dht = crate::dht::DhtClient::with_default_bootstrap();
        if let Ok(Ok(dht_peers)) =
            tokio::time::timeout_at(announce_deadline, dht.get_peers(&metainfo.info_hash)).await
        {
            peers = dht_peers;
        }
    }

    // Update the session's peer list. Bound and deduplicate it: the list is
    // retained for the session lifetime and comes from an untrusted tracker.
    {
        let mut seen = std::collections::HashSet::new();
        peers.retain(|peer| {
            seen.insert((peer.ip.clone(), peer.port)) && seen.len() <= MAX_SESSION_PEERS
        });
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.peers = peers.clone();
        }
    }

    // If this is a magnet link (no pieces), fetch the info dict from a peer.
    let mut metainfo = metainfo;
    if metainfo.pieces.is_empty() {
        // The info dict has no tracker list; keep the magnet's own trackers so
        // the completed torrent can still announce itself after registration.
        let magnet_announce = metainfo.announce.clone();
        let magnet_announce_list = metainfo.announce_list.clone();
        let metadata_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        for peer in peers
            .iter()
            .filter(|peer| peer_target_allowed(peer, crate::tracker::private_targets_allowed()))
            .take(8)
        {
            let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let connected = tokio::time::timeout_at(
                metadata_deadline,
                PeerConnection::connect(addr, metainfo.info_hash, peer_id),
            )
            .await;
            if let Ok(Ok(mut conn)) = connected {
                let fetched = tokio::time::timeout_at(
                    metadata_deadline,
                    fetch_metadata_from_peer(&mut conn, metainfo.info_hash),
                )
                .await;
                if let Ok(Some(full)) = fetched {
                    metainfo = full;
                    break;
                }
            }
            if tokio::time::Instant::now() >= metadata_deadline {
                tracing::warn!("Magnet metadata fetch timed out; continuing without peers");
                break;
            }
        }
        if metainfo.announce.is_none() {
            metainfo.announce = magnet_announce;
        }
        if metainfo.announce_list.is_empty() {
            metainfo.announce_list = magnet_announce_list;
        }
        // Persist the fetched metainfo back into the session.
        {
            let mut guard = sessions.write().await;
            if let Ok(s) = guard.get_session_mut(&session_id) {
                s.metainfo = metainfo.clone();
            }
        }
        if metainfo.pieces.is_empty() || metainfo.piece_count == 0 || metainfo.total_size == 0 {
            tracing::warn!("Magnet metadata fetch failed; stopping the session");
            let mut guard = sessions.write().await;
            if let Ok(s) = guard.get_session_mut(&session_id) {
                s.state = SessionState::Stopped;
            }
            return;
        }
    }

    // Build the shared scheduler state and load any resume bitfield.
    // Resume files are keyed by info-hash (not display name — two different
    // torrents can share a name) and the bitfield is validated against the
    // actual piece data on disk before any piece is trusted.
    let scheduler = Arc::new(StdMutex::new(SchedulerState::new(metainfo.piece_count)));
    {
        // Phase 1: load the bitfield and list claimed pieces (lock dropped
        // before any await — StdMutexGuard is not Send).
        let claimed: Vec<u32> = {
            let mut sched = scheduler.lock().unwrap_or_else(|e| e.into_inner());
            let resume_path = resume_file_path(&download_dir, &metainfo.info_hash);
            if let Ok(bytes) = std::fs::read(&resume_path) {
                sched.tracker.load_have_bitfield(&bytes);
            }
            (0..metainfo.piece_count)
                .filter(|&i| sched.tracker.has_piece(i))
                .collect()
        };
        // Phase 2: verify each claimed piece against disk (async, no lock).
        let mut unverified: Vec<u32> = Vec::new();
        for i in claimed {
            if !piece_on_disk_matches(&metainfo, &download_dir, i).await {
                unverified.push(i);
            }
        }
        // Phase 3: clear bits for pieces that failed verification.
        if !unverified.is_empty() {
            let mut sched = scheduler.lock().unwrap_or_else(|e| e.into_inner());
            for &i in &unverified {
                tracing::warn!(
                    "Resume bitfield claims piece {} but it is missing/corrupt on disk; re-fetching",
                    i
                );
                sched.tracker.clear_requested(i);
                sched.tracker.mark_not_have(i);
            }
        }
    }

    // Spawn one task per peer, capped at MAX_PEERS to prevent resource exhaustion.
    // Share the metainfo by reference: a full clone per peer would copy the
    // entire piece hash list up to 200 times for large torrents.
    let shared_metainfo = Arc::new(metainfo);
    let shared_download_dir = Arc::new(download_dir);
    let shared_cancel = cancel.clone();
    let shared_session_id = Arc::new(session_id);
    let shared_limits = Arc::clone(&limits);
    let shared_seed_ratio = Arc::clone(&seed_ratio_target);
    let mut peer_tasks = Vec::new();
    for (index, peer) in peers.into_iter().enumerate() {
        if peer_tasks.len() >= MAX_PEERS {
            break;
        }
        if !peer_target_allowed(&peer, crate::tracker::private_targets_allowed()) {
            continue;
        }
        let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
            Ok(a) => a,
            Err(_) => continue,
        };
        // Scheduler leases and rarest-first counters are keyed by this id. It
        // must be unique per task: the remote peer id is self-reported, so two
        // peers could otherwise share one lease namespace.
        let mut task_id = [0u8; 20];
        task_id[..8].copy_from_slice(&(index as u64).to_be_bytes());
        task_id[8] = 0xE2;
        task_id[9] = 0xE2;
        let scheduler = Arc::clone(&scheduler);
        let metainfo = Arc::clone(&shared_metainfo);
        let download_dir = Arc::clone(&shared_download_dir);
        let cancel = shared_cancel.clone();
        let sessions = Arc::clone(&sessions);
        let session_id = Arc::clone(&shared_session_id);
        let limits = Arc::clone(&shared_limits);
        let seed_ratio_target = Arc::clone(&shared_seed_ratio);
        let receipt_key = receipt_key.clone();
        peer_tasks.push(tokio::spawn(async move {
            run_peer_task(
                addr,
                PeerTaskContext {
                    metainfo,
                    peer_id,
                    task_id,
                    scheduler,
                    download_dir: (*download_dir).clone(),
                    sessions,
                    session_id: (*session_id).clone(),
                    limits,
                    seed_ratio_target,
                    cancel,
                    receipt_key,
                },
            )
            .await;
        }));
    }

    // Wait for all peer tasks to finish (or cancellation).
    for task in peer_tasks {
        let _ = task.await;
    }

    // Persist the resume bitfield and update final state. The shared Arcs
    // are used here because the originals were moved into the spawn loop.
    let downloaded = {
        let sched = scheduler.lock().unwrap_or_else(|e| e.into_inner());
        let resume_path = resume_file_path(&shared_download_dir, &shared_metainfo.info_hash);
        let _ = std::fs::write(&resume_path, sched.tracker.serialize_have_bitfield());
        sched
            .tracker
            .have_bytes(&|index| piece_length(&shared_metainfo, index))
    };

    // Final state update. "Seeding" marks a complete download (the RPC or
    // desktop layer registers it with the inbound seeder). An incomplete
    // engine has no running peer tasks, so "Stopped" is terminal and must not
    // consume one of the active-session slots forever.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&shared_session_id) {
            s.bytes_downloaded = downloaded;
            s.state = if shared_metainfo.total_size > 0 && downloaded >= shared_metainfo.total_size
            {
                SessionState::Seeding
            } else {
                SessionState::Stopped
            };
            s.last_active = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
    }
}

/// Shared context passed to each per-peer task.
///
/// `metainfo` is shared by reference (`Arc`) — a full clone per peer would
/// copy the entire piece hash list up to 200 times for large torrents.
struct PeerTaskContext {
    metainfo: Arc<Metainfo>,
    peer_id: [u8; 20],
    /// Stable, unique scheduler key for this peer task. Never the remote's
    /// self-reported id.
    task_id: [u8; 20],
    scheduler: Arc<StdMutex<SchedulerState>>,
    download_dir: PathBuf,
    sessions: Arc<RwLock<SessionManager>>,
    session_id: String,
    limits: Arc<crate::limits::TorrentRateLimiter>,
    seed_ratio_target: Arc<RwLock<f64>>,
    cancel: CancellationToken,
    receipt_key: Option<Arc<vtorrent_core::keys::PrivateKey>>,
}

/// Record a peer's advertised pieces during the handshake phase.
///
/// The first valid bitfield wins; a repeated or oversized one is ignored so a
/// peer cannot rewrite its availability after requests are issued.
fn record_peer_bitfield(
    scheduler: &Arc<StdMutex<SchedulerState>>,
    metainfo: &Metainfo,
    task_id: [u8; 20],
    bitfield_seen: &mut bool,
    bits: &[u8],
    addr: SocketAddr,
) {
    let expected = (metainfo.piece_count as usize).div_ceil(8);
    if *bitfield_seen || bits.len() > expected {
        tracing::debug!(
            "Ignoring invalid or repeated bitfield from {} ({} bytes, expected at most {})",
            addr,
            bits.len(),
            expected
        );
        return;
    }
    *bitfield_seen = true;
    scheduler
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tracker
        .set_peer_bitfield(task_id, bits);
}

/// Drive a single peer connection: exchange bitfields, request blocks, and
/// write verified pieces to disk, coordinated through the shared scheduler.
async fn run_peer_task(addr: SocketAddr, ctx: PeerTaskContext) {
    let PeerTaskContext {
        metainfo,
        peer_id,
        task_id,
        scheduler,
        download_dir,
        sessions,
        session_id,
        limits,
        seed_ratio_target,
        cancel,
        receipt_key,
    } = ctx;

    let mut conn = match PeerConnection::connect(addr, metainfo.info_hash, peer_id).await {
        Ok(c) => c,
        Err(_) => return,
    };
    // A download connection receives blocks (16 KiB) and small control or
    // extension messages. A bitfield for a metainfo-sized torrent is at most
    // 128 KiB; never size the inbound buffer by the piece length.
    conn.set_max_message_len(crate::peer_wire::MAX_INBOUND_FRAME);

    // Send our (empty) bitfield and interested.
    let _ = conn.send(&PeerMessage::Bitfield { bits: vec![] }).await;
    let _ = conn.send(&PeerMessage::Interested).await;

    // Exchange VTR addresses via the ut_vtr extension (BEP-10).
    let mut peer_vtr_address: Option<String> = None;
    let mut peer_receipt_id: Option<u8> = None;
    let mut local_address = String::new();
    // Whether the peer's bitfield has been recorded. Declared before the
    // extension handshake because a real peer announces its pieces
    // immediately after the wire handshake, before the extension handshake.
    let mut bitfield_seen = false;
    // One deadline for the whole discovery phase. Each `recv` has its own
    // 180 s deadline, and a peer that sends one keepalive per window could
    // otherwise stretch ten messages per loop into tens of minutes.
    let handshake_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    {
        let handshake = crate::metadata::build_ut_vtr_handshake(crate::metadata::OUR_UT_VTR_ID);
        let _ = conn
            .send(&PeerMessage::Extended {
                id: 0,
                payload: handshake,
            })
            .await;
        // Read the peer's extension handshake to learn its ut_vtr id. Peers
        // send their bitfield and unchoke before (or interleaved with) the
        // extension handshake, so those messages must be recorded here: the
        // scheduler only requests pieces that at least one peer advertised,
        // and dropping the bitfield would stall the whole download.
        let mut ut_vtr_id = None;
        for _ in 0..10 {
            let message = match tokio::time::timeout_at(handshake_deadline, conn.recv()).await {
                Ok(Ok(message)) => message,
                Ok(Err(_)) | Err(_) => break,
            };
            match message {
                PeerMessage::Extended { id: 0, payload } => {
                    if let Some(Ok(Value::Dict(d))) =
                        crate::bencode_guard::parse_untrusted::<Value>(&payload)
                    {
                        if let Some(Value::Dict(m)) = d.get(b"m".as_slice()) {
                            if let Some(Value::Int(id)) = m.get(b"ut_vtr".as_slice()) {
                                if (1..=u8::MAX as i64).contains(id) {
                                    ut_vtr_id = Some(*id as u8);
                                }
                            }
                            if let Some(Value::Int(id)) = m.get(b"ut_vtr_receipt".as_slice()) {
                                if (1..=u8::MAX as i64).contains(id) {
                                    peer_receipt_id = Some(*id as u8);
                                }
                            }
                        }
                    }
                    break;
                }
                PeerMessage::Bitfield { bits } => {
                    record_peer_bitfield(
                        &scheduler,
                        &metainfo,
                        task_id,
                        &mut bitfield_seen,
                        &bits,
                        addr,
                    );
                    continue;
                }
                PeerMessage::Have { index } => {
                    scheduler
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .tracker
                        .set_peer_have(task_id, index);
                    continue;
                }
                PeerMessage::Extended { payload, .. } => {
                    // Some peers send the address before their extension
                    // handshake. Keep it rather than discarding it and making
                    // the transfer unpayable.
                    if let Ok(address) = crate::metadata::parse_ut_vtr_address(&payload) {
                        peer_vtr_address = Some(address);
                    }
                    continue;
                }
                _ => continue,
            }
        }
        if let Some(id) = ut_vtr_id {
            // Send our address, then read the peer's address.
            local_address = {
                let guard = sessions.read().await;
                guard
                    .get_session(&session_id)
                    .map(|s| s.wallet_address.clone())
                    .unwrap_or_default()
            };
            let _ = conn
                .send(&PeerMessage::Extended {
                    id,
                    payload: crate::metadata::build_ut_vtr_address(&local_address),
                })
                .await;
            for _ in 0..10 {
                // A peer may have sent its address before its extension
                // handshake; do not wait for a second copy.
                if peer_vtr_address.is_some() {
                    break;
                }
                let message = match tokio::time::timeout_at(handshake_deadline, conn.recv()).await {
                    Ok(Ok(message)) => message,
                    Ok(Err(_)) | Err(_) => break,
                };
                match message {
                    // The peer sends its address to us on the id WE
                    // advertised. Parse the payload instead of matching the
                    // peer's own id, which is only for our outbound address.
                    PeerMessage::Extended { payload, .. } => {
                        if let Ok(addr) = crate::metadata::parse_ut_vtr_address(&payload) {
                            peer_vtr_address = Some(addr);
                            break;
                        }
                        continue;
                    }
                    PeerMessage::Bitfield { bits } => {
                        record_peer_bitfield(
                            &scheduler,
                            &metainfo,
                            task_id,
                            &mut bitfield_seen,
                            &bits,
                            addr,
                        );
                        continue;
                    }
                    PeerMessage::Have { index } => {
                        scheduler
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .tracker
                            .set_peer_have(task_id, index);
                        continue;
                    }
                    _ => continue,
                }
            }
        }
    }

    // Track in-flight blocks for this peer (recomputed from the scheduler at
    // the top of every loop iteration).
    let mut in_flight: usize;
    // Track partial piece assembly across multiple blocks. Each piece is owned
    // by exactly one task; this map only ever holds pieces this task owns.
    let mut assemblers: std::collections::HashMap<u32, PieceAssembler> =
        std::collections::HashMap::new();
    // Release our pieces if this task exits for any reason.
    let _ownership = PieceOwnership::new(Arc::clone(&scheduler), task_id);
    // Releases every assembly-budget reservation still held when this peer task
    // exits. Without this, a task that ends mid-piece (cancellation, timeout,
    // error) would charge the shared budget forever and starve later pieces.
    let mut assembly = AssemblyBudget::new(Arc::clone(&scheduler));
    // Upload-side request throttling state (per peer task): key → expiry.
    // A repeated identical request inside the window is dropped; entries are
    // evicted once expired so the map cannot grow without bound.
    let served_requests: std::sync::Mutex<
        std::collections::HashMap<(u32, u32, u32), std::time::Instant>,
    > = std::sync::Mutex::new(std::collections::HashMap::new());
    let outstanding_served = std::sync::atomic::AtomicUsize::new(0);
    // Rolling-window counters for speed estimation.
    let mut downloaded_window: u64 = 0;
    let mut uploaded_window: u64 = 0;
    let mut speed_window_start = std::time::Instant::now();
    let mut next_receipt = std::time::Instant::now();
    let mut finish_deadline: Option<std::time::Instant> = None;
    let mut last_receipt_at: Option<std::time::Instant> = None;
    let mut last_sent_receipt_at: Option<std::time::Instant> = None;
    // A peer that repeatedly serves a piece whose SHA-1 fails is corrupt or
    // malicious; retrying it forever only burns bandwidth and CPU.
    let mut hash_failures: u32 = 0;
    // A swarm that keeps the socket alive but never completes a piece must not
    // hold an active session forever. This is global progress, not per-peer.
    let mut last_have_count = scheduler
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tracker
        .have_count();
    let mut last_piece_at = std::time::Instant::now();
    const NO_PROGRESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(900);

    loop {
        if cancel.is_cancelled() {
            break;
        }
        // Recompute from the scheduler. A lease that expired, was satisfied by
        // another task, or completed must not leave this task's pipeline
        // permanently full.
        let (depth, reclaimed) = scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refresh_outstanding(task_id);
        in_flight = depth;
        for piece in reclaimed {
            // The scheduler reclaimed this piece from us after a stall. Drop
            // the partial assembler so another peer can fetch the piece and
            // release the budget it held.
            if assemblers.remove(&piece).is_some() {
                assembly.release(piece_length(&metainfo, piece));
            }
        }

        // Check completion before blocking on a read. A task with no remaining
        // work and a silent peer would otherwise wait for the 180 s receive
        // timeout before finalizing, delaying seeder registration and the
        // session state transition.
        if finish_deadline.is_none()
            && scheduler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .tracker
                .is_complete()
        {
            // A short download can finish before the next periodic tick. Send
            // one final cumulative receipt so the transfer is payable.
            // The receiver drops receipts within 30 s of the previous one, so
            // wait out that window before closing; otherwise the final receipt
            // is silently discarded.
            if let Some(last) = last_sent_receipt_at {
                let elapsed = last.elapsed();
                if elapsed < std::time::Duration::from_secs(30) {
                    let wait = std::time::Duration::from_secs(30) - elapsed;
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = cancel.cancelled() => break,
                    }
                }
            }
            send_bandwidth_receipt(
                &mut conn,
                metainfo.info_hash,
                receipt_key.as_deref(),
                peer_receipt_id,
                peer_vtr_address.as_deref(),
                &local_address,
                &session_id,
                &sessions,
            )
            .await;
            // The seeder sends cumulative receipts at most every 30 s. Keep the
            // connection readable for longer than that so its final receipt can
            // cover the last window; 10 s would leave it unpaid on a normal
            // completion.
            finish_deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(35));
        }
        if finish_deadline.is_none() {
            let have = scheduler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .tracker
                .have_count();
            if have > last_have_count {
                last_have_count = have;
                last_piece_at = std::time::Instant::now();
            } else if last_piece_at.elapsed() >= NO_PROGRESS_TIMEOUT {
                tracing::warn!(
                    peer = %addr,
                    "Torrent made no verified progress for 15 minutes; stopping this peer task"
                );
                break;
            }
        }

        if std::time::Instant::now() >= next_receipt {
            if send_bandwidth_receipt(
                &mut conn,
                metainfo.info_hash,
                receipt_key.as_deref(),
                peer_receipt_id,
                peer_vtr_address.as_deref(),
                &local_address,
                &session_id,
                &sessions,
            )
            .await
            {
                last_sent_receipt_at = Some(std::time::Instant::now());
            }
            next_receipt = std::time::Instant::now() + std::time::Duration::from_secs(60);
        }

        // Request blocks while we have pipeline capacity and pieces to fetch.
        while in_flight
            < scheduler
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .max_pipelined_blocks
        {
            let piece_len = |index: u32| piece_length(&metainfo, index);
            let (piece, begin, len) = {
                let mut sched = scheduler.lock().unwrap_or_else(|e| e.into_inner());
                match sched.next_block(&piece_len, task_id) {
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

        // Periodically fold the current window into the session's speed fields.
        {
            let elapsed = speed_window_start.elapsed().as_secs_f64();
            if elapsed >= 5.0 {
                let ds = (downloaded_window as f64 / elapsed).round() as u64;
                let us = (uploaded_window as f64 / elapsed).round() as u64;
                let mut guard = sessions.write().await;
                if let Ok(s) = guard.get_session_mut(&session_id) {
                    s.download_speed = ds;
                    s.upload_speed = us;
                }
                downloaded_window = 0;
                uploaded_window = 0;
                speed_window_start = std::time::Instant::now();
            }
        }

        // Read one message. After the download completes, bound the wait by a
        // short grace period so a peer's final receipt can still be processed.
        let received = match finish_deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, conn.recv()).await {
                    Ok(result) => result,
                    Err(_) => break,
                }
            }
            None => {
                // A task with no remaining work can be waiting while another
                // task completes the torrent. A 10 s slice lets the top-of-loop
                // completion/cancel check run instead of blocking for the full
                // 180 s receive deadline.
                match tokio::time::timeout(std::time::Duration::from_secs(10), conn.recv()).await {
                    Ok(result) => result,
                    Err(_) => continue,
                }
            }
        };
        match received {
            Ok(PeerMessage::Bitfield { bits }) => {
                record_peer_bitfield(
                    &scheduler,
                    &metainfo,
                    task_id,
                    &mut bitfield_seen,
                    &bits,
                    addr,
                );
            }
            Ok(PeerMessage::Have { index }) => {
                scheduler
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tracker
                    .set_peer_have(task_id, index);
            }
            Ok(PeerMessage::Choke) => {
                // Wait for unchoke; just continue.
            }
            Ok(PeerMessage::Interested)
            | Ok(PeerMessage::NotInterested)
            | Ok(PeerMessage::Unchoke) => {
                // Interest/choke bookkeeping is not required for a simple
                // seeder; requests are served regardless.
            }
            Ok(PeerMessage::Request {
                index,
                begin,
                length,
            }) => {
                // ── Request sanity limits (upload-bandwidth protection) ──
                // A single socket must not be able to extract unlimited
                // redundant upload bandwidth.
                const MAX_SERVED_BLOCK: u32 = 32 * 1024;
                const MAX_OUTSTANDING_PER_PEER: usize = 64;

                // Reject requests for blocks larger than a sane maximum —
                // real clients request 16 KiB blocks.
                if length == 0 || length > MAX_SERVED_BLOCK {
                    tracing::debug!("Rejecting oversized request (len {}) from peer", length);
                    continue;
                }

                // Bound outstanding requests per peer: dedupe identical
                // in-flight requests so loops of the same Request don't each
                // trigger a disk read + send.
                let key = (index, begin, length);
                const REQUEST_DEDUPE_WINDOW: std::time::Duration =
                    std::time::Duration::from_secs(10);
                let is_duplicate = {
                    let mut inflight = served_requests.lock().unwrap_or_else(|e| e.into_inner());
                    // Evict expired entries opportunistically so the map stays
                    // bounded under adversarial request patterns.
                    inflight.retain(|_, deadline| *deadline > std::time::Instant::now());
                    inflight.contains_key(&key)
                };
                if is_duplicate {
                    tracing::trace!("Dropping duplicate in-flight request {:?}", key);
                    continue;
                }

                let outstanding = outstanding_served.fetch_add(1, Ordering::SeqCst);
                if outstanding >= MAX_OUTSTANDING_PER_PEER {
                    outstanding_served.fetch_sub(1, Ordering::SeqCst);
                    tracing::debug!("Peer has too many outstanding requests; dropping");
                    continue;
                }

                // Serve the requested block if we hold the piece.
                let ratio_reached = {
                    let target = *seed_ratio_target.read().await;
                    if target > 0.0 {
                        let guard = sessions.read().await;
                        guard
                            .get_session(&session_id)
                            .map(|s| {
                                s.bytes_downloaded > 0
                                    && (s.bytes_uploaded as f64)
                                        >= (s.bytes_downloaded as f64) * target
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    }
                };
                if ratio_reached {
                    outstanding_served.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let have_piece = scheduler
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .tracker
                    .has_piece(index);
                if have_piece {
                    // Serve only the requested range. Reading the whole piece
                    // per block would let one peer multiply disk I/O by the
                    // piece size.
                    if let Some(block) =
                        read_block_from_disk(&metainfo, &download_dir, index, begin, length).await
                    {
                        if conn
                            .send(&PeerMessage::Piece {
                                index,
                                begin,
                                data: block.clone(),
                            })
                            .await
                            .is_ok()
                        {
                            // Only a delivered block gets a dedupe tombstone and
                            // is credited. A failed read/send must be retryable.
                            served_requests
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(key, std::time::Instant::now() + REQUEST_DEDUPE_WINDOW);
                            uploaded_window = uploaded_window.saturating_add(block.len() as u64);
                            let mut guard = sessions.write().await;
                            if let Ok(s) = guard.get_session_mut(&session_id) {
                                // Accounting keys must be unique per task. The
                                // remote id is self-reported and can collide.
                                let peer_key = peer_vtr_address
                                    .clone()
                                    .unwrap_or_else(|| hex::encode(task_id));
                                s.record_upload(&peer_key, block.len() as u64);
                            }
                            limits.consume_upload(block.len() as u64, &cancel).await;
                        }
                    }
                }
                outstanding_served.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(PeerMessage::Cancel { index, begin, .. }) => {
                scheduler
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear_block(index, begin, task_id);
            }
            Ok(PeerMessage::Piece { index, begin, data }) => {
                // The lease belongs to this peer task, and its length is the
                // exact byte count we asked for. A peer that answers another
                // peer's lease, another piece's offset, or a different length
                // must not be credited, and must not clear our in-flight count.
                let leased = scheduler
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .requested_block_length(index, begin, task_id);
                let Some(expected_len) = leased else {
                    tracing::debug!(
                        "Ignoring unsolicited block {}:{} from {}",
                        index,
                        begin,
                        addr
                    );
                    continue;
                };
                if data.is_empty() || data.len() as u32 != expected_len {
                    tracing::debug!(
                        "Ignoring block {}:{} from {} with length {} (expected {})",
                        index,
                        begin,
                        addr,
                        data.len(),
                        expected_len
                    );
                    continue;
                }
                let piece_len = piece_length(&metainfo, index);
                // Cap concurrent assemblers per peer to prevent memory exhaustion
                // from a malicious peer sending blocks for many different pieces.
                // Seed peers hold every piece, so many assemblers are expected
                // and the cap must scale with the piece size, not the count.
                let max_assemblers = (MAX_ASSEMBLER_BYTES / piece_len.max(1) as usize)
                    .clamp(MIN_ASSEMBLERS, MAX_ASSEMBLERS);
                if !assemblers.contains_key(&index) && assemblers.len() >= max_assemblers {
                    tracing::warn!(
                        "Peer sent block for piece {} but assembler cap reached",
                        index
                    );
                    scheduler
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear_block(index, begin, task_id);
                    continue;
                }
                if !assemblers.contains_key(&index) && !assembly.reserve(piece_len) {
                    tracing::warn!(
                        "Peer sent block for piece {} but the shared assembly budget is full",
                        index
                    );
                    scheduler
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear_block(index, begin, task_id);
                    continue;
                }
                let asm = assemblers
                    .entry(index)
                    .or_insert_with(|| PieceAssembler::new(index, piece_len));
                let accepted = asm.add_block(begin, data);
                let marked = if accepted {
                    // Mark the lease satisfied only now that the block is
                    // actually held. If the assembler cap or budget rejected
                    // it, leaving the lease unreceived lets it expire and be
                    // requested again instead of losing the block forever.
                    scheduler
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .mark_received(index, begin, task_id)
                } else {
                    true
                };
                if marked && asm.is_complete() {
                    if let Some(expected) = metainfo.pieces.get(index as usize) {
                        if asm.verify(expected) {
                            if let Some(piece_data) = asm.assemble() {
                                let Some(claim) =
                                    PieceWriteClaim::acquire(scheduler.clone(), index)
                                else {
                                    // A replayed piece we already hold must not
                                    // earn another payout.
                                    if assemblers.remove(&index).is_some() {
                                        assembly.release(piece_len);
                                    }
                                    continue;
                                };
                                let written = write_piece_to_disk(
                                    &metainfo,
                                    &download_dir,
                                    index,
                                    &piece_data,
                                )
                                .await;
                                // Publish only after the write completes. A failed
                                // write releases the claim for a later retry.
                                if written {
                                    claim.finish();
                                    // Deterministic progress: verified_bytes is sum of
                                    // SHA1-verified piece lengths via have_bytes, not
                                    // raw received bytes. Prevents progress inflation
                                    // from unverified/corrupt blocks or duplicates.
                                    let verified_bytes = {
                                        let sched =
                                            scheduler.lock().unwrap_or_else(|e| e.into_inner());
                                        sched
                                            .tracker
                                            .have_bytes(&|idx| piece_length(&metainfo, idx))
                                    };
                                    downloaded_window =
                                        downloaded_window.saturating_add(piece_data.len() as u64);
                                    // Update session progress deterministically:
                                    // progress = verified_bytes / total_bytes
                                    let mut guard = sessions.write().await;
                                    if let Ok(s) = guard.get_session_mut(&session_id) {
                                        let peer_key = peer_vtr_address
                                            .clone()
                                            .unwrap_or_else(|| hex::encode(task_id));
                                        s.record_download(&peer_key, piece_data.len() as u64);
                                        // Overwrite incremental sum with verified count
                                        // to ensure determinism and avoid double-count.
                                        s.bytes_downloaded = verified_bytes;
                                    }
                                    limits
                                        .consume_download(piece_data.len() as u64, &cancel)
                                        .await;
                                } else {
                                    tracing::warn!(
                                        piece = index,
                                        "Could not write a verified piece to disk; re-fetching it"
                                    );
                                    scheduler
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .clear_piece(index, task_id);
                                }
                            }
                        } else {
                            hash_failures = hash_failures.saturating_add(1);
                            tracing::warn!(
                                piece = index,
                                failures = hash_failures,
                                "Peer sent a piece that failed SHA-1 verification; re-fetching it"
                            );
                            scheduler
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .clear_piece(index, task_id);
                            if hash_failures >= 3 {
                                tracing::warn!(
                                    peer = %addr,
                                    "Dropping peer after repeated corrupt pieces"
                                );
                                break;
                            }
                        }
                    }
                    if assemblers.remove(&index).is_some() {
                        assembly.release(piece_len);
                    }
                }
                if !marked {
                    // The piece was reclaimed and now belongs to another task.
                    // Do not keep an unowned assembler or write its result.
                    if assemblers.remove(&index).is_some() {
                        assembly.release(piece_len);
                    }
                    continue;
                }
            }
            Ok(PeerMessage::Extended { id, payload }) if id == crate::metadata::OUR_RECEIPT_ID => {
                let Some(peer_addr) = peer_vtr_address.as_ref() else {
                    continue;
                };
                // Receipt verification is ECDSA over attacker-supplied bytes.
                // One receipt per 30s per peer is far above the 5-minute
                // settlement cadence and bounds CPU per connection.
                if let Some(last) = last_receipt_at {
                    // The seeder sends at most every 30 s. A 25 s receiver
                    // window absorbs timer drift so a receipt sent 30 s after
                    // the previous send is not dropped because it arrived a
                    // little under 30 s after the previous arrival.
                    if last.elapsed() < std::time::Duration::from_secs(25) {
                        continue;
                    }
                }
                last_receipt_at = Some(std::time::Instant::now());
                let Ok(envelope) = crate::metadata::decode_receipt(&payload) else {
                    continue;
                };
                let receipt = &envelope.receipt;
                // A receipt claiming no bytes in either direction proves
                // nothing and must not be stored or journaled.
                if receipt.uploaded_by_me == 0 && receipt.downloaded_by_me == 0 {
                    continue;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                // Bound attacker-chosen times and ids: an unvalidated
                // `u64::MAX` settlement id would permanently reject every
                // later receipt from this peer.
                if receipt.window_start > receipt.window_end
                    || receipt.window_end > now.saturating_add(600)
                    || receipt.settlement_id > now.saturating_add(600)
                {
                    continue;
                }
                if receipt.info_hash != metainfo.info_hash
                    || receipt.peer != local_address
                    || receipt.signer != *peer_addr
                {
                    continue;
                }
                // Cheap replay rejection before the expensive ECDSA verify.
                let (last_id, peer_epoch) = {
                    let guard = sessions.read().await;
                    guard
                        .get_session(&session_id)
                        .ok()
                        .and_then(|s| s.incentive_accounts.get(peer_addr))
                        .map(|a| (a.last_receipt_id, a.peer_epoch))
                        .unwrap_or((0, None))
                };
                // A new peer epoch may legitimately restart settlement ids;
                // only enforce monotonicity within the same epoch.
                if peer_epoch == Some(receipt.epoch) && receipt.settlement_id <= last_id {
                    continue;
                }
                if !envelope.verify().unwrap_or(false) {
                    continue;
                }
                let journal = {
                    let guard = sessions.read().await;
                    guard.journal()
                };
                let updated = {
                    let mut guard = sessions.write().await;
                    guard.get_session_mut(&session_id).ok().and_then(|session| {
                        let account = session.incentive_accounts.get_mut(peer_addr)?;
                        if !account.record_receipt(receipt.clone()) {
                            tracing::debug!(
                                peer = %peer_addr,
                                settlement_id = %receipt.settlement_id,
                                "Ignoring replayed bandwidth receipt"
                            );
                            return None;
                        }
                        Some(account.clone())
                    })
                };
                if let (Some(journal), Some(account)) = (journal, updated) {
                    let info_hash = metainfo.info_hash;
                    let peer_address = peer_addr.clone();
                    match tokio::task::spawn_blocking(move || {
                        journal.upsert(info_hash, &peer_address, account.receipt_epoch, &account)
                    })
                    .await
                    {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => {
                            tracing::debug!(
                                peer = %peer_addr,
                                "Incentive journal already holds a newer account state"
                            );
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                peer = %peer_addr,
                                "Could not journal incentive receipt: {}",
                                error
                            );
                        }
                        Err(error) => {
                            tracing::warn!(
                                peer = %peer_addr,
                                "Incentive journal task failed: {}",
                                error
                            );
                        }
                    }
                }
            }
            Ok(PeerMessage::KeepAlive) => {}
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}

/// Send one cumulative bandwidth receipt to the peer.
///
/// Cumulative totals are signed; the receiver settles on deltas from the last
/// verified receipt, so sending the same data twice is harmless.
#[allow(clippy::too_many_arguments)]
async fn send_bandwidth_receipt(
    conn: &mut PeerConnection,
    info_hash: [u8; 20],
    key: Option<&vtorrent_core::keys::PrivateKey>,
    receipt_id: Option<u8>,
    peer_vtr_address: Option<&str>,
    local_address: &str,
    session_id: &str,
    sessions: &Arc<RwLock<SessionManager>>,
) -> bool {
    let (Some(key), Some(id), Some(peer_addr)) = (key, receipt_id, peer_vtr_address) else {
        return false;
    };
    if local_address.is_empty() {
        return false;
    }
    let account = {
        let guard = sessions.read().await;
        guard
            .get_session(session_id)
            .ok()
            .and_then(|session| session.incentive_accounts.get(peer_addr).cloned())
    };
    let Some(account) = account else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let receipt = vtorrent_core::receipt::BandwidthReceipt {
        info_hash,
        window_start: account.last_settlement,
        window_end: now,
        uploaded_by_me: account.total_bytes_uploaded,
        downloaded_by_me: account.total_bytes_downloaded,
        peer: peer_addr.to_string(),
        signer: local_address.to_string(),
        settlement_id: now,
        epoch: account.receipt_epoch,
    };
    let Ok(signature) = receipt.sign(key) else {
        return false;
    };
    let Ok(pubkey) = key.public_key() else {
        return false;
    };
    let pubkey_bytes = if key.is_compressed() {
        pubkey.serialize().to_vec()
    } else {
        pubkey.serialize_uncompressed().to_vec()
    };
    let envelope = vtorrent_core::receipt::SignedBandwidthReceipt {
        receipt,
        pubkey: pubkey_bytes,
        signature,
    };
    conn.send(&PeerMessage::Extended {
        id,
        payload: crate::metadata::encode_receipt(&envelope),
    })
    .await
    .is_ok()
}

/// Fetch the full metainfo from a peer via the ut_metadata extension (BEP-9).
///
/// Used for magnet links, where the .torrent payload (piece hashes) must be
/// retrieved from a peer before any piece can be requested. Returns `None`
/// if the peer does not support the extension or the data fails validation.
async fn fetch_metadata_from_peer(
    conn: &mut PeerConnection,
    expected_info_hash: [u8; 20],
) -> Option<Metainfo> {
    use crate::metadata;
    use sha1::{Digest as _, Sha1};

    conn.set_max_message_len(crate::peer_wire::MAX_INBOUND_FRAME);

    // Send the extension handshake (id 0) advertising ut_metadata id 1.
    let handshake = metadata::build_extension_handshake(1, 0);
    let _ = conn
        .send(&PeerMessage::Extended {
            id: 0,
            payload: handshake,
        })
        .await;

    // Read the peer's extension handshake to learn its ut_metadata id and size.
    let mut ut_metadata_id = None;
    let mut metadata_size = 0u64;
    for _ in 0..10 {
        match conn.recv().await {
            Ok(PeerMessage::Extended { id: 0, payload }) => {
                if let Some(Ok(Value::Dict(d))) =
                    crate::bencode_guard::parse_untrusted::<Value>(&payload)
                {
                    if let Some(Value::Dict(m)) = d.get(b"m".as_slice()) {
                        if let Some(Value::Int(id)) = m.get(&b"ut_metadata".to_vec()) {
                            ut_metadata_id = Some(*id as u8);
                        }
                    }
                    if let Some(Value::Int(sz)) = d.get(b"metadata_size".as_slice()) {
                        metadata_size = *sz as u64;
                    }
                }
                break;
            }
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
    let ut_metadata_id = ut_metadata_id?;
    if metadata_size == 0 {
        return None;
    }
    // Cap the metadata size from the peer so we don't loop over a huge
    // piece count or allocate unbounded memory.
    if metadata_size > 64 * 1024 * 1024 {
        return None;
    }

    // Request the metadata in 16 KiB pieces.
    const PIECE_LEN: u64 = 16 * 1024;
    let piece_count = metadata_size.div_ceil(PIECE_LEN);
    let mut pieces: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();
    // Running byte total: `metadata_size` bounds the piece *count*, but each
    // data message can be up to MAX_MESSAGE_LENGTH (16 MiB), so a hostile peer
    // answering with forged piece indices could otherwise pin ~64 GB.
    let mut received_bytes: u64 = 0;

    for piece in 0..piece_count as u32 {
        let req = metadata::build_request(piece);
        let _ = conn
            .send(&PeerMessage::Extended {
                id: ut_metadata_id,
                payload: req,
            })
            .await;

        // Read until we get the data for this piece.
        for _ in 0..20 {
            match conn.recv().await {
                Ok(PeerMessage::Extended { id, payload }) if id == ut_metadata_id => {
                    if let Ok((p, _total, data)) = metadata::parse_data(&payload) {
                        // Reject out-of-range indices and oversized payloads,
                        // and abort once the declared metadata size is exceeded.
                        if (p as u64) < piece_count
                            && data.len() as u64 <= PIECE_LEN
                            && received_bytes.saturating_add(data.len() as u64) <= metadata_size
                        {
                            received_bytes += data.len() as u64;
                            pieces.insert(p, data);
                        } else {
                            return None;
                        }
                    }
                    break;
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    let info_dict = metadata::reassemble_metadata(&pieces, metadata_size).ok()?;

    // BEP-9: the reassembled blob is the raw `info` dict. Its SHA1 must equal
    // the magnet's info_hash — without this check any peer could serve forged
    // metadata and we would download attacker-chosen content under this
    // magnet's identity.
    let computed: [u8; 20] = Sha1::digest(&info_dict).into();
    if computed != expected_info_hash {
        tracing::warn!(
            "ut_metadata info_hash mismatch: got {}, expected {}",
            hex::encode(computed),
            hex::encode(expected_info_hash)
        );
        return None;
    }

    // Wrap the raw info dict in a minimal torrent structure so the standard
    // parser (which expects a top-level "info" key) can read it.
    let mut wrapped = Vec::with_capacity(info_dict.len() + 10);
    wrapped.extend_from_slice(b"d4:info");
    wrapped.extend_from_slice(&info_dict);
    wrapped.extend_from_slice(b"e");
    Metainfo::from_bytes(&wrapped).ok()
}

/// Announce to every tracker (HTTP and UDP) until one returns peers.
///
/// UDP tracker URLs with hostnames are resolved via `lookup_host` —
/// `SocketAddr::parse` only accepts literal IPs, which silently skipped
/// the common `udp://tracker.example.org:6881` form.
/// Returns `None` when the HTTP tracker client cannot be created.
async fn announce_trackers(
    trackers: &[String],
    metainfo: &Metainfo,
    peer_id: [u8; 20],
) -> Option<Vec<crate::tracker::TrackerPeer>> {
    let tracker = match HttpTracker::new() {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Failed to create HTTP tracker client: {}", e);
            return None;
        }
    };
    let mut peers = Vec::new();
    for url in trackers {
        if url.starts_with("udp://") {
            // UDP tracker (BEP-15). Resolve hostnames — tracker URLs are
            // commonly `udp://tracker.example.org:6881`, and `SocketAddr::
            // parse` only accepts literal IPs (those trackers were silently
            // skipped before).
            let host_port = url.trim_start_matches("udp://");
            let addr: SocketAddr = match host_port.parse() {
                Ok(a) => a,
                Err(_) => {
                    let (host, port) = match host_port.rsplit_once(':') {
                        Some((h, p)) => (h, p),
                        None => continue,
                    };
                    let port: u16 = match port.parse() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    match tokio::net::lookup_host((host, port)).await {
                        Ok(mut it) => match it.next() {
                            Some(a) => a,
                            None => continue,
                        },
                        Err(_) => continue,
                    }
                }
            };
            let udp = crate::udp::UdpTracker::new(addr);
            // Same SSRF guard as the HTTP path: tracker URLs come from
            // untrusted torrents, so a UDP announce must not reach internal
            // addresses either.
            if !crate::tracker::tracker_target_allowed(addr.ip()) {
                tracing::warn!("Skipping UDP tracker {} — non-public address", addr.ip());
                continue;
            }
            let params = crate::udp::UdpAnnounceParams {
                info_hash: &metainfo.info_hash,
                peer_id: &peer_id,
                downloaded: 0,
                left: metainfo.total_size,
                uploaded: 0,
                event: AnnounceEvent::Started,
                port: 6881,
            };
            if let Ok(p) = udp.announce(&params).await {
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
    Some(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_disk::sanitize_path;

    fn sha1_of(data: &[u8]) -> [u8; 20] {
        let mut hasher = Sha1::new();
        hasher.update(data);
        let digest = hasher.finalize();
        let mut out = [0u8; 20];
        out.copy_from_slice(&digest);
        out
    }

    #[test]
    fn test_sanitize_path_rejects_traversal() {
        let base = std::path::Path::new("/downloads");

        // Normal path is accepted.
        let ok = sanitize_path(base, &["dir".to_string(), "file.txt".to_string()]);
        assert_eq!(
            ok,
            Some(std::path::PathBuf::from("/downloads/dir/file.txt"))
        );

        // `..` escapes the base.
        assert!(sanitize_path(base, &["..".to_string()]).is_none());
        assert!(
            sanitize_path(base, &["a".to_string(), "..".to_string(), "b".to_string()]).is_none()
        );

        // Absolute path escapes the base.
        assert!(sanitize_path(base, &["/etc/passwd".to_string()]).is_none());

        // Embedded separators are rejected.
        assert!(sanitize_path(base, &["a/b".to_string()]).is_none());
        assert!(sanitize_path(base, &["a\\b".to_string()]).is_none());

        // Empty and `.` components are rejected.
        assert!(sanitize_path(base, &["".to_string()]).is_none());
        assert!(sanitize_path(base, &[".".to_string()]).is_none());
    }

    #[test]
    fn replayed_pieces_cannot_be_credited_twice() {
        let scheduler = Arc::new(StdMutex::new(SchedulerState::new(4)));
        PieceWriteClaim::acquire(scheduler.clone(), 0)
            .unwrap()
            .finish();
        assert!(PieceWriteClaim::acquire(scheduler.clone(), 0).is_none());
        assert!(scheduler.lock().unwrap().has_piece(0));
    }

    #[test]
    fn piece_claim_does_not_publish_before_write_and_cancellation_releases_it() {
        let scheduler = Arc::new(StdMutex::new(SchedulerState::new(1)));
        let claim = PieceWriteClaim::acquire(scheduler.clone(), 0).unwrap();
        assert!(!scheduler.lock().unwrap().has_piece(0));
        assert!(PieceWriteClaim::acquire(scheduler.clone(), 0).is_none());
        drop(claim);
        let claim = PieceWriteClaim::acquire(scheduler.clone(), 0).unwrap();
        claim.finish();
        assert!(scheduler.lock().unwrap().has_piece(0));
        assert!(PieceWriteClaim::acquire(scheduler.clone(), 0).is_none());
    }

    #[tokio::test]
    async fn failed_piece_write_leaves_the_piece_unpublished_and_reclaimable() {
        use crate::metainfo::{Metainfo, TorrentFile};

        let dir = std::env::temp_dir().join(format!(
            "vtr-piece-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The torrent directory name is occupied by a regular file, so the
        // write must fail without touching anything.
        std::fs::write(dir.join("torrent"), b"not a directory").unwrap();
        let metainfo = Metainfo {
            info_hash: [1u8; 20],
            name: "torrent".into(),
            total_size: 4,
            piece_length: 4,
            piece_count: 1,
            pieces: vec![[2u8; 20]],
            announce: None,
            announce_list: Vec::new(),
            files: vec![TorrentFile {
                path: vec!["file.bin".into()],
                length: 4,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        };
        let scheduler = Arc::new(StdMutex::new(SchedulerState::new(1)));
        let claim = PieceWriteClaim::acquire(scheduler.clone(), 0).unwrap();
        let written = write_piece_to_disk(&metainfo, &dir, 0, b"data").await;
        assert!(!written, "write into a file path must fail");
        drop(claim);
        assert!(!scheduler.lock().unwrap().has_piece(0));
        assert!(
            PieceWriteClaim::acquire(scheduler.clone(), 0).is_some(),
            "a failed write must release the claim"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracker_peers_must_pass_the_target_filter() {
        use crate::tracker::TrackerPeer;

        let mut peer = TrackerPeer {
            ip: "8.8.8.8".to_string(),
            port: 6881,
            peer_id: None,
        };
        assert!(peer_target_allowed(&peer, false));
        peer.ip = "127.0.0.1".to_string();
        assert!(!peer_target_allowed(&peer, false));
        assert!(peer_target_allowed(&peer, true));
        peer.ip = "169.254.169.254".to_string();
        assert!(!peer_target_allowed(&peer, false));
        peer.ip = "8.8.8.8".to_string();
        peer.port = 0;
        assert!(!peer_target_allowed(&peer, true));
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
    fn test_piece_assembler_rejects_empty_blocks() {
        let mut asm = PieceAssembler::new(0, 10);
        for _ in 0..5_000 {
            assert!(!asm.add_block(0, Vec::new()));
        }
        assert!(asm.blocks.is_empty());
        assert_eq!(asm.received, 0);
        assert!(!asm.is_complete());
    }

    #[test]
    fn test_piece_assembler_rejects_overlapping_blocks() {
        let mut asm = PieceAssembler::new(0, 10);
        assert!(asm.add_block(0, vec![1; 5]));
        assert!(!asm.add_block(3, vec![2; 5]), "overlap must be rejected");
        assert!(asm.add_block(5, vec![3; 5]));
        assert!(asm.is_complete());
        assert_eq!(asm.assemble().unwrap(), vec![1, 1, 1, 1, 1, 3, 3, 3, 3, 3]);
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

    #[test]
    fn test_piece_assembler_rejects_out_of_bounds_block() {
        let mut asm = PieceAssembler::new(0, 10);
        assert!(!asm.add_block(8, vec![0u8; 4]));
        assert!(!asm.is_complete());
        assert!(asm.assemble().is_none());
        assert!(!asm.add_block(10, vec![0u8; 1]));
    }

    #[test]
    fn test_piece_assembler_rejects_duplicate_offset() {
        let mut asm = PieceAssembler::new(0, 10);
        assert!(asm.add_block(0, vec![1u8; 5]));
        assert!(!asm.add_block(0, vec![2u8; 5]));
        assert_eq!(asm.received, 5);
    }

    #[test]
    fn test_file_layout_single_file() {
        let files = vec![TorrentFile {
            path: vec!["a.bin".to_string()],
            length: 100,
            md5sum: None,
        }];
        let layout = FileLayout::new(&files, 50);
        let segs = layout.piece_segments(0, &[0u8; 50]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].0, 0);
        assert_eq!(segs[0].1, 0);
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
        let segs = layout.piece_segments(0, &[0u8; 50]);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].0, 0);
        assert_eq!(segs[0].1, 0);
        assert_eq!(segs[0].2.len(), 30);
        assert_eq!(segs[1].0, 1);
        assert_eq!(segs[1].1, 0);
        assert_eq!(segs[1].2.len(), 20);
    }

    #[test]
    fn test_piece_segment_ranges_matches_segments() {
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
        // Last piece of the multi-file torrent is 50 bytes too.
        let segs = layout.piece_segments(1, &[0u8; 50]);
        let ranges = layout.piece_segment_ranges(1, 50);
        assert_eq!(segs.len(), ranges.len());
        for (s, r) in segs.iter().zip(ranges.iter()) {
            assert_eq!(s.0, r.0);
            assert_eq!(s.1, r.1);
            assert_eq!(s.2.len() as u64, r.2);
        }
    }

    #[tokio::test]
    async fn test_peer_connection_handshake() {
        use tokio::net::TcpListener;

        let info_hash = [0xAA; 20];
        let our_peer_id = [0x11; 20];
        let their_peer_id = [0x22; 20];

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

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

        let conn = PeerConnection::connect(addr, info_hash, our_peer_id)
            .await
            .unwrap();
        assert_eq!(conn.remote_peer_id, their_peer_id);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn test_peer_connection_recv_preserves_coalesced_messages() {
        use tokio::net::TcpListener;

        let info_hash = [0xAB; 20];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 68];
            sock.read_exact(&mut buf).await.unwrap();
            let mut bytes = PeerMessage::Handshake {
                info_hash,
                peer_id: [0x22; 20],
                reserved: [0u8; 8],
            }
            .encode();
            bytes.extend(PeerMessage::Interested.encode());
            bytes.extend(PeerMessage::Bitfield { bits: vec![0xF0] }.encode());
            bytes.extend(
                PeerMessage::Piece {
                    index: 0,
                    begin: 0,
                    data: vec![1, 2, 3, 4],
                }
                .encode(),
            );
            bytes.extend(
                PeerMessage::Piece {
                    index: 0,
                    begin: 4,
                    data: vec![5, 6, 7, 8],
                }
                .encode(),
            );
            sock.write_all(&bytes).await.unwrap();
            sock.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let mut conn = PeerConnection::connect(addr, info_hash, [0x11; 20])
            .await
            .unwrap();
        assert_eq!(conn.recv().await.unwrap(), PeerMessage::Interested);
        assert_eq!(
            conn.recv().await.unwrap(),
            PeerMessage::Bitfield { bits: vec![0xF0] }
        );
        assert_eq!(
            conn.recv().await.unwrap(),
            PeerMessage::Piece {
                index: 0,
                begin: 0,
                data: vec![1, 2, 3, 4],
            }
        );
        assert_eq!(
            conn.recv().await.unwrap(),
            PeerMessage::Piece {
                index: 0,
                begin: 4,
                data: vec![5, 6, 7, 8],
            }
        );

        server.await.unwrap();
    }
}
