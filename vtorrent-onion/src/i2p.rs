use crate::{
    config::TransportConfig,
    error::{OnionError, Result},
    hidden_service::HiddenServiceInfo,
};
/// I2P transport via the SAM (Simple Anonymous Messaging) bridge protocol.
///
/// The SAM bridge is a local TCP service (default: 127.0.0.1:7656) that
/// provides a simple text-based protocol for creating I2P sessions and
/// making connections to I2P destinations.
///
/// ## SAM Protocol Overview
///
/// 1. Connect to SAM bridge TCP port
/// 2. Send `HELLO VERSION MIN=3.0 MAX=3.3`
/// 3. Create a STREAM session: `SESSION CREATE STYLE=STREAM ID=<id> DESTINATION=TRANSIENT`
/// 4. Connect to a destination: `STREAM CONNECT ID=<id> DESTINATION=<b32addr>`
/// 5. The TCP stream is now tunneled through I2P
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// I2P SAM transport.
pub struct I2pTransport {
    config: TransportConfig,
}

impl I2pTransport {
    pub fn new(config: TransportConfig) -> Self {
        Self { config }
    }

    /// Check if the I2P SAM bridge is reachable.
    pub async fn is_available(&self) -> bool {
        let timeout = Duration::from_secs(3);
        tokio::time::timeout(timeout, TcpStream::connect(&self.config.i2p_sam_addr))
            .await
            .is_ok_and(|r| r.is_ok())
    }

    /// Connect to an I2P destination through the SAM bridge.
    pub async fn connect(&self, dest: &str, _port: u16) -> Result<TcpStream> {
        let timeout = Duration::from_secs(self.config.connect_timeout_secs);

        // Connect to SAM bridge
        let stream = tokio::time::timeout(timeout, TcpStream::connect(&self.config.i2p_sam_addr))
            .await
            .map_err(|_| OnionError::Timeout(self.config.connect_timeout_secs))?
            .map_err(|e| OnionError::I2pUnavailable {
                addr: self.config.i2p_sam_addr.clone(),
                source: e,
            })?;

        let mut reader = sam_handshake(stream).await?;

        // Create a STREAM session with a transient (one-time) destination
        let session_id = format!("vtorrent-{}", rand::random::<u32>());
        let session_cmd = format!(
            "SESSION CREATE STYLE=STREAM ID={} DESTINATION=TRANSIENT\n",
            session_id
        );
        reader.get_mut().write_all(session_cmd.as_bytes()).await?;

        let resp = read_sam_line(&mut reader).await?;
        if !resp.contains("RESULT=OK") {
            return Err(OnionError::SamError(format!(
                "SESSION CREATE failed: {}",
                resp.trim()
            )));
        }

        // Connect to the destination
        let connect_cmd = format!(
            "STREAM CONNECT ID={} DESTINATION={} SILENT=false\n",
            session_id, dest
        );
        reader.get_mut().write_all(connect_cmd.as_bytes()).await?;

        let resp = read_sam_line(&mut reader).await?;
        if !resp.contains("RESULT=OK") {
            return Err(OnionError::SamError(format!(
                "STREAM CONNECT failed: {}",
                resp.trim()
            )));
        }

        Ok(reader.into_inner())
    }

    /// Create an I2P hidden service (server-side destination).
    pub async fn create_destination(&self, local_port: u16) -> Result<HiddenServiceInfo> {
        let stream = TcpStream::connect(&self.config.i2p_sam_addr)
            .await
            .map_err(|e| OnionError::I2pUnavailable {
                addr: self.config.i2p_sam_addr.clone(),
                source: e,
            })?;

        let mut reader = sam_handshake(stream).await?;

        // Generate a new destination key pair
        reader
            .get_mut()
            .write_all(b"DEST GENERATE SIGNATURE_TYPE=EdDSA_SHA512_Ed25519\n")
            .await?;
        let resp = read_sam_line(&mut reader).await?;

        // Parse DEST=<pub> PRIV=<priv>
        let pub_key = resp
            .split_whitespace()
            .find(|s| s.starts_with("PUB="))
            .and_then(|s| s.strip_prefix("PUB="))
            .ok_or_else(|| OnionError::SamError("Missing PUB in DEST GENERATE".to_string()))?
            .to_string();

        let priv_key = resp
            .split_whitespace()
            .find(|s| s.starts_with("PRIV="))
            .and_then(|s| s.strip_prefix("PRIV="))
            .ok_or_else(|| OnionError::SamError("Missing PRIV in DEST GENERATE".to_string()))?
            .to_string();

        // Derive the .b32.i2p address from the destination. The SAM `PUB=`
        // field is the base64-encoded destination (public key + certificate);
        // the b32 address is base32(SHA-256(decoded destination bytes)).
        let decoded = base64_decode(&pub_key).ok_or_else(|| {
            OnionError::SamError("Invalid base64 destination in DEST GENERATE".to_string())
        })?;
        let hash = sha256_bytes(&decoded);
        let b32_addr = base32_encode(&hash);
        let i2p_addr = format!("{}.b32.i2p:{}", b32_addr.to_lowercase(), local_port);

        tracing::info!("Created I2P destination: {}", i2p_addr);

        Ok(HiddenServiceInfo {
            onion_addr: i2p_addr,
            private_key: Some(priv_key),
            local_port,
            virtual_port: local_port,
        })
    }
}

/// Perform the SAM HELLO handshake, returning a buffered stream for the rest
/// of the session. The buffer is kept across calls so bytes read ahead by the
/// bridge are not lost between protocol steps.
async fn sam_handshake(stream: TcpStream) -> Result<BufReader<TcpStream>> {
    let mut reader = BufReader::new(stream);
    reader
        .get_mut()
        .write_all(b"HELLO VERSION MIN=3.0 MAX=3.3\n")
        .await?;
    let resp = read_sam_line(&mut reader).await?;
    if !resp.contains("RESULT=OK") {
        return Err(OnionError::SamError(format!(
            "SAM HELLO failed: {}",
            resp.trim()
        )));
    }
    Ok(reader)
}

/// Read a single line from a SAM stream.
async fn read_sam_line(reader: &mut BufReader<TcpStream>) -> Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(line)
}

/// Compute SHA-256 of bytes for address derivation.
fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Decode a base64 string (standard alphabet, no padding tolerance).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .ok()
}

/// Base32 encode (RFC 4648, no padding).
fn base32_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut result = String::new();
    let mut buffer: u32 = 0;
    let mut bits_left: u32 = 0;

    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits_left += 8;
        while bits_left >= 5 {
            bits_left -= 5;
            result.push(ALPHABET[((buffer >> bits_left) & 0x1f) as usize] as char);
        }
    }
    if bits_left > 0 {
        result.push(ALPHABET[((buffer << (5 - bits_left)) & 0x1f) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TransportConfig;

    #[test]
    fn test_base32_encode() {
        let data = b"hello";
        let encoded = base32_encode(data);
        assert!(!encoded.is_empty());
        assert!(encoded
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn test_i2p_unavailable() {
        let config = TransportConfig {
            i2p_sam_addr: "127.0.0.1:17656".to_string(), // wrong port
            ..Default::default()
        };
        let i2p = I2pTransport::new(config);
        assert!(!i2p.is_available().await);
    }
}
