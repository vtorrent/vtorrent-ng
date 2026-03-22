/// UDP hole punching for NAT traversal.
///
/// Protocol:
/// 1. Both nodes discover their external address via STUN (see stun.rs)
/// 2. Both nodes exchange their external endpoints via the rendezvous layer
///    (DHT or PEX addr messages — see rendezvous.rs)
/// 3. Both nodes simultaneously send PUNCH packets to each other's external
///    address. This causes both NAT routers to open a mapping for the other's
///    IP:port, allowing the reply to pass through.
/// 4. Once a PUNCH_ACK is received, the hole is open and the session key is
///    established via ephemeral X25519 DH embedded in the handshake.
///
/// Packet types (1 byte tag):
///   0x01  PUNCH      — "I am here, please open your NAT"
///   0x02  PUNCH_ACK  — "I received your PUNCH, hole is open"
///   0x03  DATA       — encrypted application data

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, timeout};

use crate::crypto::{ephemeral_dh, NodeKeypair, SharedKey};
use crate::endpoint::Endpoint;
use crate::error::{OverlayError, Result};

pub const TAG_PUNCH: u8 = 0x01;
const TAG_PUNCH_ACK: u8 = 0x02;
pub const TAG_DATA: u8 = 0x03;

/// Maximum number of PUNCH retries before giving up.
const MAX_PUNCH_RETRIES: u32 = 10;
/// Delay between PUNCH retries.
const PUNCH_RETRY_INTERVAL: Duration = Duration::from_millis(200);
/// Total timeout for the hole punch handshake.
const PUNCH_TIMEOUT: Duration = Duration::from_secs(5);

/// A successfully established peer session.
pub struct PeerSession {
    pub remote_endpoint: Endpoint,
    pub remote_addr: SocketAddr,
    pub shared_key: SharedKey,
    pub remote_pubkey: [u8; 32],
    pub send_counter: u32,
}

/// The hole punch engine.
pub struct HolePuncher {
    socket: Arc<UdpSocket>,
    local_keypair: NodeKeypair,
    /// Active sessions keyed by remote node ID.
    sessions: Arc<RwLock<HashMap<String, PeerSession>>>,
}

impl HolePuncher {
    pub fn new(socket: Arc<UdpSocket>, local_keypair: NodeKeypair) -> Self {
        Self {
            socket,
            local_keypair,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Initiate a hole punch to a remote endpoint.
    ///
    /// Sends PUNCH packets repeatedly until a PUNCH_ACK is received or timeout.
    /// Returns a `PeerSession` with the established shared key.
    pub async fn punch(&self, remote: &Endpoint) -> Result<()> {
        let candidates = remote.candidates();

        // Try each candidate address (LAN first, then external)
        for addr in &candidates {
            match self.punch_addr(remote, *addr).await {
                Ok(_) => {
                    tracing::info!(
                        "Hole punch succeeded to {} via {}",
                        remote.node_id[..8].to_string(),
                        addr
                    );
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!("Hole punch to {} failed: {}", addr, e);
                }
            }
        }

        Err(OverlayError::HolePunch(format!(
            "all candidates failed for {}",
            &remote.node_id[..8]
        )))
    }

    /// Punch to a specific address.
    async fn punch_addr(&self, remote: &Endpoint, addr: SocketAddr) -> Result<()> {
        // Generate ephemeral DH keypair for this session
        let remote_pubkey_bytes = hex::decode(&remote.node_id)
            .map_err(|e| OverlayError::Crypto(e.to_string()))?;
        if remote_pubkey_bytes.len() != 32 {
            return Err(OverlayError::Crypto("invalid remote node ID length".into()));
        }
        let remote_pubkey: [u8; 32] = remote_pubkey_bytes.try_into().unwrap();
        let (eph_pub, session_key_bytes) = ephemeral_dh(&remote_pubkey);

        // Build PUNCH packet: [TAG_PUNCH | local_node_id(32) | eph_pub(32)]
        let local_pub = self.local_keypair.public.as_bytes();
        let mut punch_pkt = Vec::with_capacity(65);
        punch_pkt.push(TAG_PUNCH);
        punch_pkt.extend_from_slice(local_pub);
        punch_pkt.extend_from_slice(&eph_pub);

        // Send PUNCH packets repeatedly while waiting for PUNCH_ACK
        let socket = self.socket.clone();
        let punch_pkt_clone = punch_pkt.clone();
        let addr_clone = addr;

        let send_task = tokio::spawn(async move {
            for _ in 0..MAX_PUNCH_RETRIES {
                let _ = socket.send_to(&punch_pkt_clone, addr_clone).await;
                sleep(PUNCH_RETRY_INTERVAL).await;
            }
        });

        // Wait for PUNCH_ACK
        let mut buf = [0u8; 512];
        let result = timeout(PUNCH_TIMEOUT, async {
            loop {
                let (n, from) = self.socket.recv_from(&mut buf).await?;
                if from == addr && n >= 1 && buf[0] == TAG_PUNCH_ACK {
                    return Ok::<(), std::io::Error>(());
                }
            }
        })
        .await;

        send_task.abort();

        match result {
            Ok(Ok(())) => {
                // Store the session
                let shared_key = SharedKey::from_raw(session_key_bytes);
                let session = PeerSession {
                    remote_endpoint: remote.clone(),
                    remote_addr: addr,
                    shared_key,
                    remote_pubkey,
                    send_counter: 0,
                };
                self.sessions
                    .write()
                    .await
                    .insert(remote.node_id.clone(), session);
                Ok(())
            }
            Ok(Err(e)) => Err(OverlayError::Io(e)),
            Err(_) => Err(OverlayError::Timeout),
        }
    }

    /// Handle an incoming PUNCH packet from a remote node.
    /// Sends back a PUNCH_ACK and establishes the session.
    pub async fn handle_punch(&self, from: SocketAddr, data: &[u8]) -> Result<()> {
        if data.len() < 65 || data[0] != TAG_PUNCH {
            return Err(OverlayError::HolePunch("invalid PUNCH packet".into()));
        }

        let remote_pubkey: [u8; 32] = data[1..33].try_into().unwrap();
        let remote_eph_pub: [u8; 32] = data[33..65].try_into().unwrap();

        // Derive session key from our static secret + their ephemeral public key
        let (our_eph_pub, session_key_bytes) = ephemeral_dh(&remote_eph_pub);

        // Send PUNCH_ACK: [TAG_PUNCH_ACK | our_eph_pub(32)]
        let mut ack = Vec::with_capacity(33);
        ack.push(TAG_PUNCH_ACK);
        ack.extend_from_slice(&our_eph_pub);
        self.socket.send_to(&ack, from).await.map_err(OverlayError::Io)?;

        let node_id = hex::encode(remote_pubkey);
        let shared_key = SharedKey::from_raw(session_key_bytes);
        let session = PeerSession {
            remote_endpoint: Endpoint::new(node_id.clone(), from),
            remote_addr: from,
            shared_key,
            remote_pubkey,
            send_counter: 0,
        };
        self.sessions.write().await.insert(node_id, session);

        tracing::info!("Accepted hole punch from {}", from);
        Ok(())
    }

    /// Send encrypted data to a connected peer.
    pub async fn send_data(&self, node_id: &str, payload: &[u8]) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(node_id)
            .ok_or_else(|| OverlayError::PeerNotFound(node_id.to_string()))?;

        session.send_counter += 1;
        let ct = session
            .shared_key
            .encrypt(session.send_counter, &session.remote_pubkey, payload)?;

        let mut pkt = Vec::with_capacity(1 + ct.len());
        pkt.push(TAG_DATA);
        pkt.extend_from_slice(&ct);

        self.socket
            .send_to(&pkt, session.remote_addr)
            .await
            .map_err(OverlayError::Io)?;
        Ok(())
    }

    /// Returns true if a session exists for the given node ID.
    pub async fn is_connected(&self, node_id: &str) -> bool {
        self.sessions.read().await.contains_key(node_id)
    }

    /// Returns the number of active sessions.
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::NodeKeypair;

    #[tokio::test]
    async fn test_holepuncher_session_count_starts_zero() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let kp = NodeKeypair::generate();
        let hp = HolePuncher::new(socket, kp);
        assert_eq!(hp.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_holepuncher_is_connected_false_for_unknown() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let kp = NodeKeypair::generate();
        let hp = HolePuncher::new(socket, kp);
        assert!(!hp.is_connected("unknown_node_id").await);
    }
}
