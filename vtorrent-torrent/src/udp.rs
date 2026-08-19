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
