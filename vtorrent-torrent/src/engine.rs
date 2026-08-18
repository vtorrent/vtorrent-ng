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
        let segs = layout.piece_segments(0, &vec![0u8; 50]);
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
        let segs = layout.piece_segments(0, &vec![0u8; 50]);
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

        let mut conn = PeerConnection::connect(addr, info_hash, our_peer_id)
            .await
            .unwrap();
        assert_eq!(conn.remote_peer_id, their_peer_id);

        server.await.unwrap();
    }
}
