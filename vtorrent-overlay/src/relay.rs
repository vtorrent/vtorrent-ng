/// Peer relay fallback for strict symmetric NAT.
///
/// When direct UDP hole punching fails (rare, typically <5% of nodes),
/// we relay traffic through a third node that both peers can already reach.
///
/// This is fully peer-to-peer — no central relay server. Any connected peer
/// can act as a relay. The relay node sees only encrypted ciphertext and
/// cannot read the content.
///
/// Protocol:
///   RELAY_REQUEST  [0x10 | target_node_id(32) | payload]
///   RELAY_FORWARD  [0x11 | source_node_id(32) | payload]
///   RELAY_DECLINE  [0x12 | target_node_id(32)]
///
/// The relay node simply forwards RELAY_REQUEST packets to the target as
/// RELAY_FORWARD packets. If it cannot reach the target, it sends RELAY_DECLINE.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use crate::error::{OverlayError, Result};

pub const TAG_RELAY_REQUEST: u8 = 0x10;
pub const TAG_RELAY_FORWARD: u8 = 0x11;
pub const TAG_RELAY_DECLINE: u8 = 0x12;

/// A relay session — traffic for `target_node_id` is forwarded to `target_addr`.
#[derive(Debug, Clone)]
pub struct RelaySession {
    pub target_node_id: String,
    pub target_addr: SocketAddr,
    pub requester_addr: SocketAddr,
}

/// The relay engine — handles forwarding for peers that cannot hole-punch.
pub struct RelayEngine {
    socket: Arc<UdpSocket>,
    /// Sessions this node is currently relaying.
    sessions: Arc<RwLock<Vec<RelaySession>>>,
    /// Max number of relay sessions this node will accept.
    max_sessions: usize,
}

impl RelayEngine {
    pub fn new(socket: Arc<UdpSocket>, max_sessions: usize) -> Self {
        Self {
            socket,
            sessions: Arc::new(RwLock::new(Vec::new())),
            max_sessions,
        }
    }

    /// Handle an incoming RELAY_REQUEST packet.
    ///
    /// If we know the target, forward the payload to them as RELAY_FORWARD.
    /// If we don't know the target or are at capacity, send RELAY_DECLINE.
    pub async fn handle_relay_request(
        &self,
        from: SocketAddr,
        data: &[u8],
        known_peers: &HashMap<String, SocketAddr>,
    ) -> Result<()> {
        if data.len() < 33 || data[0] != TAG_RELAY_REQUEST {
            return Err(OverlayError::HolePunch("invalid RELAY_REQUEST".into()));
        }

        let target_id = hex::encode(&data[1..33]);
        let payload = &data[33..];

        // Check capacity
        if self.sessions.read().await.len() >= self.max_sessions {
            let decline = build_relay_decline(&data[1..33]);
            self.socket
                .send_to(&decline, from)
                .await
                .map_err(OverlayError::Io)?;
            return Ok(());
        }

        // Look up target
        match known_peers.get(&target_id) {
            Some(&target_addr) => {
                // Forward the payload
                let mut fwd = Vec::with_capacity(33 + payload.len());
                fwd.push(TAG_RELAY_FORWARD);
                // Include the requester's node ID so the target knows who sent it
                // (we use the first 32 bytes of `from`'s IP as a placeholder;
                //  in practice the requester embeds their node ID in the payload)
                fwd.extend_from_slice(&[0u8; 32]); // source placeholder
                fwd.extend_from_slice(payload);
                self.socket
                    .send_to(&fwd, target_addr)
                    .await
                    .map_err(OverlayError::Io)?;

                // Record the session
                let session = RelaySession {
                    target_node_id: target_id,
                    target_addr,
                    requester_addr: from,
                };
                self.sessions.write().await.push(session);
                tracing::debug!(
                    "Relaying {} bytes from {} to {}",
                    payload.len(),
                    from,
                    target_addr
                );
            }
            None => {
                let decline = build_relay_decline(&data[1..33]);
                self.socket
                    .send_to(&decline, from)
                    .await
                    .map_err(OverlayError::Io)?;
            }
        }

        Ok(())
    }

    /// Number of active relay sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Clear all relay sessions (called periodically to free resources).
    pub async fn clear_sessions(&self) {
        self.sessions.write().await.clear();
    }
}

fn build_relay_decline(target_id_bytes: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(33);
    pkt.push(TAG_RELAY_DECLINE);
    pkt.extend_from_slice(&target_id_bytes[..32.min(target_id_bytes.len())]);
    pkt
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_relay_engine_session_count_starts_zero() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let engine = RelayEngine::new(socket, 10);
        assert_eq!(engine.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_relay_engine_clear_sessions() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let engine = RelayEngine::new(socket, 10);
        engine.clear_sessions().await;
        assert_eq!(engine.session_count().await, 0);
    }

    #[test]
    fn test_build_relay_decline_length() {
        let target = [0xabu8; 32];
        let pkt = build_relay_decline(&target);
        assert_eq!(pkt.len(), 33);
        assert_eq!(pkt[0], TAG_RELAY_DECLINE);
    }
}
