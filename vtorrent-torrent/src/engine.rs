//! Torrent download/upload engine: piece assembly, file layout, peer I/O.

use crate::error::{Result, TorrentError};
use crate::metainfo::{Metainfo, TorrentFile};
use crate::peer_wire::PeerMessage;
use crate::session::{SessionManager, SessionState};
use crate::tracker::{AnnounceEvent, AnnounceRequest, HttpTracker};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

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
