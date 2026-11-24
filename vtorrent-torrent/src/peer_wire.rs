use crate::error::{Result, TorrentError};
use crate::metainfo::Metainfo;
use serde::{Deserialize, Serialize};

/// Absolute upper bound for a peer-wire frame. Established connections apply a
/// tighter per-torrent limit; this only bounds the decode buffer.
pub(crate) const MAX_MESSAGE_LENGTH: usize = 16 * 1024 * 1024;

/// Largest frame accepted on an established connection.
///
/// Blocks are 16 KiB in practice, extension messages are a few KiB, and the
/// largest legitimate bitfield for a metainfo-sized torrent is 128 KiB. This
/// is deliberately independent of the torrent's piece length: a peer must not
/// be able to pin a multi-megabyte receive buffer by declaring a piece-sized
/// frame it was never asked for.
pub(crate) const MAX_INBOUND_FRAME: usize = 256 * 1024;

/// Canonical BitTorrent block size used by the scheduler and both upload paths.
pub const BLOCK_SIZE: u32 = 16 * 1024;

/// Validate a block request against the scheduler's canonical block geometry.
///
/// The scheduler issues 16 KiB blocks aligned to the piece start. Accepting
/// arbitrary lengths/offsets lets a peer turn each frame into an independent
/// disk open/seek/read, so the upload paths must reject anything else before
/// touching the file.
pub fn valid_block_request(metainfo: &Metainfo, index: u32, begin: u32, length: u32) -> bool {
    if index >= metainfo.piece_count || length == 0 || length > BLOCK_SIZE {
        return false;
    }
    let piece_len = crate::engine_disk::piece_length(metainfo, index);
    if piece_len == 0 || u64::from(begin) >= piece_len || !begin.is_multiple_of(BLOCK_SIZE) {
        return false;
    }
    let expected = (piece_len - u64::from(begin)).min(u64::from(BLOCK_SIZE)) as u32;
    length == expected
}

/// Generate a random 20-byte peer id for handshakes and tracker announces.
///
/// A fixed id collides with other processes (and with the node's own seeder
/// entry) on trackers keyed by peer id, especially behind NAT.
pub fn random_peer_id() -> [u8; 20] {
    use rand::RngCore;
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(b"-VT0001-");
    rand::thread_rng().fill_bytes(&mut id[8..]);
    id
}

/// BitTorrent peer wire protocol message types (BEP-3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerMessage {
    /// Handshake — sent first by both sides.
    Handshake {
        info_hash: [u8; 20],
        peer_id: [u8; 20],
        /// Extension bits (8 bytes) — bit 20 = extension protocol (BEP-10).
        reserved: [u8; 8],
    },
    /// Keep-alive — empty message (length prefix = 0).
    KeepAlive,
    /// Choke — sender will not upload to receiver.
    Choke,
    /// Unchoke — sender will upload to receiver.
    Unchoke,
    /// Interested — receiver wants pieces from sender.
    Interested,
    /// Not interested — receiver does not want pieces.
    NotInterested,
    /// Have — sender has downloaded piece `index`.
    Have { index: u32 },
    /// Bitfield — bitmap of pieces the sender has.
    Bitfield { bits: Vec<u8> },
    /// Request — ask for a block within a piece.
    Request { index: u32, begin: u32, length: u32 },
    /// Piece — a block of data within a piece.
    Piece {
        index: u32,
        begin: u32,
        data: Vec<u8>,
    },
    /// Cancel — cancel a previously requested block.
    Cancel { index: u32, begin: u32, length: u32 },
    /// Port — DHT port (BEP-5).
    Port { port: u16 },
    /// Extension protocol message (BEP-10).
    Extended { id: u8, payload: Vec<u8> },
}

impl PeerMessage {
    /// Encode the message to bytes for sending over the wire.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            PeerMessage::Handshake {
                info_hash,
                peer_id,
                reserved,
            } => {
                let mut buf = Vec::with_capacity(68);
                buf.push(19); // pstrlen
                buf.extend_from_slice(b"BitTorrent protocol");
                buf.extend_from_slice(reserved);
                buf.extend_from_slice(info_hash);
                buf.extend_from_slice(peer_id);
                buf
            }
            PeerMessage::KeepAlive => {
                vec![0, 0, 0, 0]
            }
            PeerMessage::Choke => encode_simple(0),
            PeerMessage::Unchoke => encode_simple(1),
            PeerMessage::Interested => encode_simple(2),
            PeerMessage::NotInterested => encode_simple(3),
            PeerMessage::Have { index } => {
                let mut buf = vec![0, 0, 0, 5, 4];
                buf.extend_from_slice(&index.to_be_bytes());
                buf
            }
            PeerMessage::Bitfield { bits } => {
                let len = (1 + bits.len()) as u32;
                let mut buf = len.to_be_bytes().to_vec();
                buf.push(5);
                buf.extend_from_slice(bits);
                buf
            }
            PeerMessage::Request {
                index,
                begin,
                length,
            } => {
                let mut buf = vec![0, 0, 0, 13, 6];
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(&length.to_be_bytes());
                buf
            }
            PeerMessage::Piece { index, begin, data } => {
                let len = (9 + data.len()) as u32;
                let mut buf = len.to_be_bytes().to_vec();
                buf.push(7);
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(data);
                buf
            }
            PeerMessage::Cancel {
                index,
                begin,
                length,
            } => {
                let mut buf = vec![0, 0, 0, 13, 8];
                buf.extend_from_slice(&index.to_be_bytes());
                buf.extend_from_slice(&begin.to_be_bytes());
                buf.extend_from_slice(&length.to_be_bytes());
                buf
            }
            PeerMessage::Port { port } => {
                let mut buf = vec![0, 0, 0, 3, 9];
                buf.extend_from_slice(&port.to_be_bytes());
                buf
            }
            PeerMessage::Extended { id, payload } => {
                let len = (2 + payload.len()) as u32;
                let mut buf = len.to_be_bytes().to_vec();
                buf.push(20);
                buf.push(*id);
                buf.extend_from_slice(payload);
                buf
            }
        }
    }

    /// Decode a message from a byte buffer (after the handshake).
    /// Returns the message and the number of bytes consumed.
    pub fn decode(buf: &[u8]) -> Result<Option<(Self, usize)>> {
        Self::decode_with_limit(buf, MAX_MESSAGE_LENGTH)
    }

    /// Decode with a caller-supplied maximum frame length.
    ///
    /// Used by established torrent connections to reject oversized frames before
    /// the whole payload is buffered. The absolute [`MAX_MESSAGE_LENGTH`] bound
    /// still applies.
    pub fn decode_with_limit(buf: &[u8], max_length: usize) -> Result<Option<(Self, usize)>> {
        if buf.len() < 4 {
            return Ok(None); // Need at least the length prefix
        }

        let length = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;

        // Reject absurd lengths so a malicious peer cannot force unbounded
        // buffering. The caller may impose a tighter, connection-specific cap.
        if length > max_length.min(MAX_MESSAGE_LENGTH) {
            return Err(TorrentError::PeerWireError(format!(
                "message length {} exceeds maximum {}",
                length,
                max_length.min(MAX_MESSAGE_LENGTH)
            )));
        }

        if length == 0 {
            return Ok(Some((PeerMessage::KeepAlive, 4)));
        }

        if buf.len() < 4 + length {
            return Ok(None); // Not enough data yet
        }

        let id = buf[4];
        let payload = &buf[5..4 + length];

        let msg = match id {
            0 => PeerMessage::Choke,
            1 => PeerMessage::Unchoke,
            2 => PeerMessage::Interested,
            3 => PeerMessage::NotInterested,
            4 => {
                if payload.len() < 4 {
                    return Err(TorrentError::PeerWireError(
                        "Have: payload too short".into(),
                    ));
                }
                PeerMessage::Have {
                    index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                }
            }
            5 => PeerMessage::Bitfield {
                bits: payload.to_vec(),
            },
            6 => {
                if payload.len() < 12 {
                    return Err(TorrentError::PeerWireError(
                        "Request: payload too short".into(),
                    ));
                }
                PeerMessage::Request {
                    index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                    begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                    length: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
                }
            }
            7 => {
                if payload.len() < 8 {
                    return Err(TorrentError::PeerWireError(
                        "Piece: payload too short".into(),
                    ));
                }
                PeerMessage::Piece {
                    index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                    begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                    data: payload[8..].to_vec(),
                }
            }
            8 => {
                if payload.len() < 12 {
                    return Err(TorrentError::PeerWireError(
                        "Cancel: payload too short".into(),
                    ));
                }
                PeerMessage::Cancel {
                    index: u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
                    begin: u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]),
                    length: u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]),
                }
            }
            9 => {
                if payload.len() < 2 {
                    return Err(TorrentError::PeerWireError(
                        "Port: payload too short".into(),
                    ));
                }
                PeerMessage::Port {
                    port: u16::from_be_bytes([payload[0], payload[1]]),
                }
            }
            20 => {
                if payload.is_empty() {
                    return Err(TorrentError::PeerWireError(
                        "Extended: payload too short".into(),
                    ));
                }
                PeerMessage::Extended {
                    id: payload[0],
                    payload: payload[1..].to_vec(),
                }
            }
            _ => {
                // Unknown message IDs are silently skipped per BEP
                // (BitTorrent extension protocol). Returning an error
                // would kill the connection for peers using extensions
                // we don't implement. We must consume the full frame —
                // returning Ok(None) here would be read as "need more
                // data", wedging the connection on this prefix forever.
                tracing::trace!("Ignoring unknown message id: {}", id);
                return Ok(Some((PeerMessage::KeepAlive, 4 + length)));
            }
        };

        Ok(Some((msg, 4 + length)))
    }

    /// Decode a handshake from a byte buffer.
    /// Returns the message and the number of bytes consumed (68 bytes).
    pub fn decode_handshake(buf: &[u8]) -> Result<Option<(Self, usize)>> {
        if buf.len() < 68 {
            return Ok(None);
        }
        if buf[0] != 19 || &buf[1..20] != b"BitTorrent protocol" {
            return Err(TorrentError::PeerWireError(
                "Invalid handshake protocol string".into(),
            ));
        }
        let mut reserved = [0u8; 8];
        reserved.copy_from_slice(&buf[20..28]);
        let mut info_hash = [0u8; 20];
        info_hash.copy_from_slice(&buf[28..48]);
        let mut peer_id = [0u8; 20];
        peer_id.copy_from_slice(&buf[48..68]);
        Ok(Some((
            PeerMessage::Handshake {
                info_hash,
                peer_id,
                reserved,
            },
            68,
        )))
    }
}

fn encode_simple(id: u8) -> Vec<u8> {
    vec![0, 0, 0, 1, id]
}

/// Peer connection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerState {
    /// Initial state — handshake not yet sent.
    Connecting,
    /// Handshake sent, waiting for response.
    Handshaking,
    /// Handshake complete, exchanging messages.
    Connected,
    /// Connection closed.
    Disconnected,
}

/// Tracks the choke/interest state between two peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerChokeState {
    /// Whether we are choking the remote peer (not uploading to them).
    pub am_choking: bool,
    /// Whether we are interested in the remote peer (want to download from them).
    pub am_interested: bool,
    /// Whether the remote peer is choking us (not uploading to us).
    pub peer_choking: bool,
    /// Whether the remote peer is interested in us (wants to download from us).
    pub peer_interested: bool,
}

impl Default for PeerChokeState {
    fn default() -> Self {
        PeerChokeState {
            am_choking: true,
            am_interested: false,
            peer_choking: true,
            peer_interested: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metainfo::TorrentFile;

    fn test_metainfo(piece_count: u32, piece_length: u64, total_size: u64) -> Metainfo {
        Metainfo {
            info_hash: [0x51; 20],
            name: "block-geometry-test".into(),
            total_size,
            piece_length,
            piece_count,
            pieces: Vec::new(),
            announce: None,
            announce_list: Vec::new(),
            files: vec![TorrentFile {
                path: vec!["block-geometry-test".into()],
                length: total_size,
                md5sum: None,
            }],
            created_at: None,
            comment: None,
            is_private: false,
        }
    }

    #[test]
    fn valid_block_requests_are_canonical() {
        let m = test_metainfo(48, 262_144, 12 * 1024 * 1024);
        assert!(valid_block_request(&m, 0, 0, BLOCK_SIZE));
        assert!(!valid_block_request(&m, 0, 1, BLOCK_SIZE));
        assert!(!valid_block_request(&m, 0, 0, 1));
        assert!(!valid_block_request(&m, 0, 0, 2 * BLOCK_SIZE));
        assert!(valid_block_request(&m, 47, 0, BLOCK_SIZE));
        assert!(valid_block_request(&m, 47, 245_760, BLOCK_SIZE));
        assert!(!valid_block_request(&m, 47, 262_144, BLOCK_SIZE));
        assert!(!valid_block_request(&m, 48, 0, BLOCK_SIZE));
        assert!(!valid_block_request(&m, 0, 0, 0));

        let short = test_metainfo(48, 262_144, 12 * 1024 * 1024 - 8_000);
        let last_len = 12 * 1024 * 1024 - 8_000 - 47 * 262_144;
        let last_block = (last_len / 16_384) * 16_384;
        assert!(valid_block_request(
            &short,
            47,
            last_block as u32,
            (last_len - last_block) as u32
        ));
        assert!(!valid_block_request(
            &short,
            47,
            last_block as u32,
            BLOCK_SIZE
        ));
    }

    #[test]
    fn test_encode_decode_choke() {
        let msg = PeerMessage::Choke;
        let encoded = msg.encode();
        assert_eq!(encoded, vec![0, 0, 0, 1, 0]);
        let (decoded, consumed) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, PeerMessage::Choke);
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_encode_decode_have() {
        let msg = PeerMessage::Have { index: 42 };
        let encoded = msg.encode();
        let (decoded, _) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, PeerMessage::Have { index: 42 });
    }

    #[test]
    fn test_encode_decode_request() {
        let msg = PeerMessage::Request {
            index: 5,
            begin: 0,
            length: 16384,
        };
        let encoded = msg.encode();
        let (decoded, _) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(
            decoded,
            PeerMessage::Request {
                index: 5,
                begin: 0,
                length: 16384
            }
        );
    }

    #[test]
    fn test_encode_decode_piece() {
        let data = vec![0xAB; 16384];
        let msg = PeerMessage::Piece {
            index: 3,
            begin: 0,
            data: data.clone(),
        };
        let encoded = msg.encode();
        let (decoded, _) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(
            decoded,
            PeerMessage::Piece {
                index: 3,
                begin: 0,
                data
            }
        );
    }

    #[test]
    fn test_keepalive() {
        let msg = PeerMessage::KeepAlive;
        let encoded = msg.encode();
        assert_eq!(encoded, vec![0, 0, 0, 0]);
        let (decoded, consumed) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(decoded, PeerMessage::KeepAlive);
        assert_eq!(consumed, 4);
    }

    #[test]
    fn test_handshake_encode_decode() {
        let info_hash = [0xAA; 20];
        let peer_id = [0xBB; 20];
        let reserved = [0u8; 8];
        let msg = PeerMessage::Handshake {
            info_hash,
            peer_id,
            reserved,
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 68);
        let (decoded, consumed) = PeerMessage::decode_handshake(&encoded).unwrap().unwrap();
        assert_eq!(consumed, 68);
        if let PeerMessage::Handshake {
            info_hash: ih,
            peer_id: pid,
            ..
        } = decoded
        {
            assert_eq!(ih, [0xAA; 20]);
            assert_eq!(pid, [0xBB; 20]);
        } else {
            panic!("Expected Handshake");
        }
    }

    #[test]
    fn test_partial_buffer_returns_none() {
        // Only 3 bytes — not enough for length prefix
        let buf = vec![0u8, 0, 0];
        assert!(PeerMessage::decode(&buf).unwrap().is_none());
    }

    #[test]
    fn test_encode_decode_extended() {
        let msg = PeerMessage::Extended {
            id: 1,
            payload: vec![0xAB, 0xCD],
        };
        let encoded = msg.encode();
        assert_eq!(encoded[0..5], [0, 0, 0, 4, 20]);
        let (decoded, _) = PeerMessage::decode(&encoded).unwrap().unwrap();
        assert_eq!(
            decoded,
            PeerMessage::Extended {
                id: 1,
                payload: vec![0xAB, 0xCD]
            }
        );
    }
}
