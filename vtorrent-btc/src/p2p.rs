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
use tokio::time::{timeout, Duration};

/// Timeout for the version handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A single connection to a Bitcoin peer.
pub struct BtcPeer {
    stream: TcpStream,
    network: bitcoin::Network,
}

impl BtcPeer {
    /// Connect to a peer and perform the version handshake (mainnet).
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        Self::connect_with_network(addr, bitcoin::Network::Bitcoin).await
    }

    /// Connect to a peer on a specific Bitcoin network and perform the
    /// version handshake.
    pub async fn connect_with_network(addr: SocketAddr, network: bitcoin::Network) -> Result<Self> {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        let version = VersionMessage {
            version: 70016,
            // Advertise NODE_BLOOM so peers honor our `filterload` messages
            // (BIP-37). Without it, Bitcoin Core (protocol >= 70011) silently
            // ignores bloom filters and returns empty merkleblocks.
            services: ServiceFlags::WITNESS | ServiceFlags::BLOOM,
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

        let msg = RawNetworkMessage::new(network.magic(), NetworkMessage::Version(version));
        let payload = serialize(&msg);
        stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        // Complete the version handshake: read the peer's version, reply with
        // verack, then wait for the peer's verack. Bitcoin Core sends
        // wtxidrelay/sendaddrv2/sendheaders/ping around the verack, so ignore
        // those until we see the verack.
        let mut peer = Self { stream, network };
        let mut got_version = false;
        let mut got_verack = false;
        while !got_verack {
            let msg = timeout(HANDSHAKE_TIMEOUT, peer.recv())
                .await
                .map_err(|_| BtcError::P2p("handshake timed out".into()))??;
            tracing::debug!("BTC handshake: received {:?}", msg);
            match msg {
                NetworkMessage::Version(_) => {
                    got_version = true;
                    peer.send(NetworkMessage::Verack).await?;
                }
                NetworkMessage::Verack => got_verack = true,
                NetworkMessage::WtxidRelay
                | NetworkMessage::SendAddrV2
                | NetworkMessage::SendHeaders
                | NetworkMessage::Ping(_)
                | NetworkMessage::FeeFilter(_) => continue,
                other => {
                    return Err(BtcError::P2p(format!(
                        "unexpected message during handshake: {:?}",
                        other
                    )))
                }
            }
        }
        if !got_version {
            return Err(BtcError::P2p("peer did not send version".into()));
        }

        Ok(peer)
    }

    /// Send a raw network message.
    pub async fn send(&mut self, msg: NetworkMessage) -> Result<()> {
        let raw = RawNetworkMessage::new(self.network.magic(), msg);
        let payload = serialize(&raw);
        self.stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        Ok(())
    }

    /// Broadcast a transaction to the network.
    ///
    /// Bitcoin Core does not send an `inv` back to the peer that submitted a
    /// transaction (it only relays to *other* peers), so we cannot wait for an
    /// acknowledgement. Instead, wait briefly for a `reject` message: if none
    /// arrives before the timeout, the transaction was accepted and relayed.
    pub async fn broadcast_tx(&mut self, tx: &bitcoin::Transaction) -> Result<()> {
        self.send(NetworkMessage::Tx(tx.clone())).await?;
        loop {
            match timeout(HANDSHAKE_TIMEOUT, self.recv()).await {
                // No message (and no reject) before the timeout: accepted.
                Err(_) => return Ok(()),
                Ok(Err(e)) => return Err(e),
                Ok(Ok(NetworkMessage::Reject(rej))) => {
                    return Err(BtcError::P2p(format!(
                        "transaction rejected: {}",
                        rej.reason
                    )));
                }
                Ok(Ok(NetworkMessage::Ping(nonce))) => {
                    self.send(NetworkMessage::Pong(nonce)).await?;
                }
                // Ignore pre-tx chatter (sendcmpct, feefilter, etc.).
                Ok(Ok(_)) => continue,
            }
        }
    }

    /// Read one raw network message.
    pub async fn recv(&mut self) -> Result<NetworkMessage> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let len = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
        let mut payload = vec![0u8; len];
        self.stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let mut full = header.to_vec();
        full.extend_from_slice(&payload);
        let raw: RawNetworkMessage =
            deserialize(&full).map_err(|e| BtcError::P2p(e.to_string()))?;
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
