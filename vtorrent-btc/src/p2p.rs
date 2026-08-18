//! Minimal Bitcoin P2P client for header sync and transaction broadcast.

use crate::error::{BtcError, Result};
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_network::VersionMessage;
use bitcoin::p2p::Address as BtcAddress;
use bitcoin::p2p::ServiceFlags;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A single connection to a Bitcoin peer.
pub struct BtcPeer {
    stream: TcpStream,
}

impl BtcPeer {
    /// Connect to a peer and perform the version handshake.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        let version = VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            receiver: BtcAddress::new(&addr, ServiceFlags::NONE),
            sender: BtcAddress::new(&addr, ServiceFlags::NONE),
            nonce: 0,
            user_agent: "/vtorrent-btc:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };

        let msg = RawNetworkMessage::new(
            bitcoin::Network::Bitcoin.magic(),
            NetworkMessage::Version(version),
        );
        let payload = serialize(&msg);
        stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        // Read the verack (ignore the peer's version for now).
        let mut buf = [0u8; 24];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        Ok(Self { stream })
    }

    /// Send a raw network message.
    pub async fn send(&mut self, msg: NetworkMessage) -> Result<()> {
        let raw = RawNetworkMessage::new(bitcoin::Network::Bitcoin.magic(), msg);
        let payload = serialize(&raw);
        self.stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        Ok(())
    }

    /// Read one raw network message.
    pub async fn recv(&mut self) -> Result<NetworkMessage> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let len = u32::from_le_bytes([header[20], header[21], header[22], header[23]]) as usize;
        let mut payload = vec![0u8; len];
        self.stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let mut full = header.to_vec();
        full.extend_from_slice(&payload);
        let raw: RawNetworkMessage = deserialize(&full).map_err(|e| BtcError::P2p(e.to_string()))?;
        Ok(raw.payload().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_message_serializes() {
        let addr: SocketAddr = "127.0.0.1:8333".parse().unwrap();
        let version = VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS,
            timestamp: 0,
            receiver: BtcAddress::new(&addr, ServiceFlags::NONE),
            sender: BtcAddress::new(&addr, ServiceFlags::NONE),
            nonce: 0,
            user_agent: "/vtorrent-btc:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };
        let msg = RawNetworkMessage::new(
            bitcoin::Network::Bitcoin.magic(),
            NetworkMessage::Version(version),
        );
        let bytes = serialize(&msg);
        assert!(!bytes.is_empty());
    }
}
