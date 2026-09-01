/// Tokio codec for framing vTorrent P2P messages.
///
/// Frame format (Bitcoin-compatible):
///   [4 bytes: magic]
///   [12 bytes: command (null-padded)]
///   [4 bytes: payload_length (LE)]
///   [4 bytes: checksum (first 4 bytes of SHA256d(payload))]
///   [N bytes: payload]
use bytes::{Buf, BufMut, BytesMut};
use sha2::{Digest, Sha256};
use tokio_util::codec::{Decoder, Encoder};

use crate::{
    error::P2pError,
    message::{NetMessage, MAX_PAYLOAD_SIZE, NETWORK_MAGIC},
};

/// Header size: magic(4) + command(12) + length(4) + checksum(4) = 24 bytes.
const HEADER_SIZE: usize = 24;

/// Compute the 4-byte message checksum (first 4 bytes of SHA256d).
fn message_checksum(payload: &[u8]) -> [u8; 4] {
    let first = Sha256::digest(payload);
    let second = Sha256::digest(first);
    [second[0], second[1], second[2], second[3]]
}

/// The vTorrent P2P message codec.
pub struct VtrCodec {
    magic: [u8; 4],
}

impl VtrCodec {
    pub fn new(magic: [u8; 4]) -> Self {
        Self { magic }
    }
}

impl Default for VtrCodec {
    fn default() -> Self {
        Self::new(NETWORK_MAGIC)
    }
}

impl Decoder for VtrCodec {
    type Item = NetMessage;
    type Error = P2pError;

    fn decode(
        &mut self,
        src: &mut BytesMut,
    ) -> std::result::Result<Option<Self::Item>, Self::Error> {
        // Need at least a full header
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        // Check magic bytes
        if src[..4] != self.magic {
            return Err(P2pError::Protocol(format!(
                "Invalid magic: {:02x}{:02x}{:02x}{:02x}",
                src[0], src[1], src[2], src[3]
            )));
        }

        // Read payload length
        let payload_len = u32::from_le_bytes([src[16], src[17], src[18], src[19]]) as usize;

        if payload_len > MAX_PAYLOAD_SIZE as usize {
            return Err(P2pError::Protocol(format!(
                "Payload too large: {} bytes",
                payload_len
            )));
        }

        // Wait for the full message. Reserve only a bounded chunk: reserving
        // the full declared length lets a 24-byte header force a ~MAX_PAYLOAD
        // allocation per connection before any payload byte arrives (memory
        // amplification DoS). BytesMut grows as real data arrives.
        if src.len() < HEADER_SIZE + payload_len {
            const RESERVE_CHUNK: usize = 64 * 1024;
            let needed = HEADER_SIZE + payload_len - src.len();
            src.reserve(needed.min(RESERVE_CHUNK));
            return Ok(None);
        }

        // Read command (12 bytes)
        let mut command = [0u8; 12];
        command.copy_from_slice(&src[4..16]);

        // Read expected checksum
        let expected_checksum = [src[20], src[21], src[22], src[23]];

        // Advance past header
        src.advance(HEADER_SIZE);

        // Read payload
        let payload = src[..payload_len].to_vec();
        src.advance(payload_len);

        // Verify checksum
        let actual_checksum = message_checksum(&payload);
        if actual_checksum != expected_checksum {
            return Err(P2pError::Protocol("Checksum mismatch".into()));
        }

        Ok(Some(NetMessage { command, payload }))
    }
}

impl Encoder<NetMessage> for VtrCodec {
    type Error = P2pError;

    fn encode(
        &mut self,
        msg: NetMessage,
        dst: &mut BytesMut,
    ) -> std::result::Result<(), Self::Error> {
        let checksum = message_checksum(&msg.payload);
        let payload_len = msg.payload.len() as u32;

        dst.reserve(HEADER_SIZE + msg.payload.len());

        // Magic
        dst.put_slice(&self.magic);
        // Command
        dst.put_slice(&msg.command);
        // Payload length (LE)
        dst.put_u32_le(payload_len);
        // Checksum
        dst.put_slice(&checksum);
        // Payload
        dst.put_slice(&msg.payload);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let original = NetMessage::new("ping", b"test payload".to_vec());

        let mut codec = VtrCodec::default();
        let mut buf = BytesMut::new();

        codec
            .encode(original.clone(), &mut buf)
            .expect("Encode failed");
        let decoded = codec
            .decode(&mut buf)
            .expect("Decode failed")
            .expect("No message");

        assert_eq!(decoded.command_str(), "ping");
        assert_eq!(decoded.payload, b"test payload");
    }

    #[test]
    fn test_partial_message_returns_none() {
        let mut codec = VtrCodec::default();
        let mut buf = BytesMut::from(&b"VTRX"[..]);
        let result = codec.decode(&mut buf).expect("Should not error");
        assert!(result.is_none());
    }
}
