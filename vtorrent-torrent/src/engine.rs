//! Torrent download/upload engine: piece assembly, file layout, peer I/O.

use crate::error::{Result, TorrentError};
use crate::metainfo::{Metainfo, TorrentFile};
use crate::peer_wire::PeerMessage;
use crate::scheduler::SchedulerState;
use crate::session::{SessionManager, SessionState};
use crate::tracker::{AnnounceEvent, AnnounceRequest, HttpTracker};
use serde_bencode::value::Value;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// BEP-10 extension bit: reserved[5] bit 0x10.
const EXTENSION_RESERVED: [u8; 8] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00];

/// Assembles blocks into a full piece and verifies its SHA1.
pub struct PieceAssembler {
    expected_length: u64,
    blocks: HashMap<u32, Vec<u8>>,
    received: u64,
}

impl PieceAssembler {
    pub fn new(_piece_index: u32, expected_length: u64) -> Self {
        Self {
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
    pub fn piece_segments(
        &self,
        piece_index: u32,
        piece_data: &[u8],
    ) -> Vec<(usize, u64, Vec<u8>)> {
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
            reserved: EXTENSION_RESERVED,
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

    // Update the session's peer list.
    {
        let mut guard = sessions.write().await;
        if let Ok(s) = guard.get_session_mut(&session_id) {
            s.peers = peers.clone();
        }
    }

    // If this is a magnet link (no pieces), fetch the info dict from a peer.
    let mut metainfo = metainfo;
    if metainfo.pieces.is_empty() {
        for peer in &peers {
            let addr: SocketAddr = match format!("{}:{}", peer.ip, peer.port).parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if let Ok(mut conn) = PeerConnection::connect(addr, metainfo.info_hash, peer_id).await {
                if let Some(full) = fetch_metadata_from_peer(&mut conn).await {
                    metainfo = full;
                    break;
                }
            }
        }
        // Persist the fetched metainfo back into the session.
        {
            let mut guard = sessions.write().await;
            if let Ok(s) = guard.get_session_mut(&session_id) {
                s.metainfo = metainfo.clone();
            }
        }
    }

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
                PeerTaskContext {
                    metainfo,
                    peer_id,
                    scheduler,
                    download_dir,
                    sessions,
                    session_id,
                    cancel,
                },
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
        sched.tracker.have_count() as u64 * metainfo.piece_length
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

/// Shared context passed to each per-peer task.
struct PeerTaskContext {
    metainfo: Metainfo,
    peer_id: [u8; 20],
    scheduler: Arc<StdMutex<SchedulerState>>,
    download_dir: PathBuf,
    sessions: Arc<RwLock<SessionManager>>,
    session_id: String,
    cancel: CancellationToken,
}

/// Drive a single peer connection: exchange bitfields, request blocks, and
/// write verified pieces to disk, coordinated through the shared scheduler.
async fn run_peer_task(addr: SocketAddr, ctx: PeerTaskContext) {
    let PeerTaskContext {
        metainfo,
        peer_id,
        scheduler,
        download_dir,
        sessions,
        session_id,
        cancel,
    } = ctx;

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
                                    s.bytes_downloaded =
                                        s.bytes_downloaded.saturating_add(piece_data.len() as u64);
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

/// Fetch the info dict from a peer via BEP-9 `ut_metadata`, returning the
/// parsed `Metainfo`. Returns `None` if the peer does not support extensions.
async fn fetch_metadata_from_peer(conn: &mut PeerConnection) -> Option<Metainfo> {
    use crate::metadata;

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
                if let Ok(Value::Dict(d)) = serde_bencode::from_bytes::<Value>(&payload) {
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

    // Request the metadata in 16 KiB pieces.
    const PIECE_LEN: u64 = 16 * 1024;
    let piece_count = metadata_size.div_ceil(PIECE_LEN);
    let mut pieces: std::collections::HashMap<u32, Vec<u8>> = std::collections::HashMap::new();

    for piece in 0..piece_count as u32 {
        let req = metadata::build_request(ut_metadata_id, piece);
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
                        pieces.insert(p, data);
                    }
                    break;
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }

    let info_dict = metadata::reassemble_metadata(&pieces, metadata_size).ok()?;
    Metainfo::from_bytes(&info_dict).ok()
}

/// Write a verified piece's data to the correct file(s) on disk.
async fn write_piece_to_disk(
    metainfo: &Metainfo,
    download_dir: &std::path::Path,
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
            .truncate(false)
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
}
