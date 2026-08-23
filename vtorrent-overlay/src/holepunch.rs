/// UDP hole punching for NAT traversal with an authenticated handshake.
///
/// Protocol (Noise-KK-style, mutual authentication via static-key DH proofs):
///
/// 1. Both nodes discover their external address via STUN (see stun.rs)
/// 2. Both nodes exchange their external endpoints via the rendezvous layer
/// 3. Handshake (3 datagrams):
///    - PUNCH    (initiator → responder): `[static_i(32) | eph_i(32)]`
///    - PUNCH_ACK (responder → initiator): `[eph_r(32) | mac1(32)]`
///      where mac1 proves the responder owns `static_r`:
///      `k1 = DH(static_r_secret, eph_i)`,
///      `mac1 = H("mac1" || k1 || static_i || eph_i || eph_r)`
///    - PUNCH_CONFIRM (initiator → responder): `[static_i | eph_i | mac2(32)]`
///      where mac2 proves the initiator owns `static_i`:
///      `k2 = DH(static_i_secret, eph_r)`,
///      `mac2 = H("mac2" || k2 || static_i || eph_i || eph_r)`
/// 4. Session key = H("sess-v2" || DH(ee) || DH(es) || DH(se)) — an on-path
///    attacker racing the real peer cannot compute any term without the
///    corresponding static secret.
///
/// Packet tags (1 byte):
///   0x01  PUNCH          — handshake message 1
///   0x02  PUNCH_ACK      — handshake message 2
///   0x03  DATA           — encrypted application data
///   0x04  PUNCH_CONFIRM  — handshake message 3
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{sleep, timeout};

use crate::crypto::{
    derive_session_key, hash_transcript, initiator_handshake_dhs, responder_handshake_dhs,
    EphemeralKeypair, NodeKeypair, SharedKey,
};
use crate::endpoint::Endpoint;
use crate::error::{OverlayError, Result};

pub const TAG_PUNCH: u8 = 0x01;
const TAG_PUNCH_ACK: u8 = 0x02;
pub const TAG_DATA: u8 = 0x03;
pub const TAG_PUNCH_CONFIRM: u8 = 0x04;

/// Maximum number of PUNCH retries before giving up.
const MAX_PUNCH_RETRIES: u32 = 10;
/// Delay between PUNCH retries.
const PUNCH_RETRY_INTERVAL: Duration = Duration::from_millis(200);
/// Total timeout for the hole punch handshake.
const PUNCH_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum number of concurrent hole-punch sessions to prevent memory exhaustion.
const MAX_SESSIONS: usize = 256;
/// Session TTL — idle sessions older than this are evicted.
const SESSION_TTL: Duration = Duration::from_secs(3600);
/// Pending (half-open) handshakes awaiting CONFIRM.
const MAX_PENDING_HANDSHAKES: usize = 1024;
/// Pending handshakes expire if CONFIRM does not arrive in time.
const PENDING_TTL: Duration = Duration::from_secs(30);

/// A successfully established peer session.
pub struct PeerSession {
    pub remote_endpoint: Endpoint,
    pub remote_addr: SocketAddr,
    pub shared_key: SharedKey,
    pub remote_pubkey: [u8; 32],
    pub local_pubkey: [u8; 32],
    pub send_counter: u32,
    /// Highest counter received from the peer (for replay protection).
    pub recv_counter: u32,
    /// When this session was created (for TTL eviction).
    pub created_at: tokio::time::Instant,
}

/// A half-open handshake on the responder side: ACK sent, waiting for the
/// initiator's CONFIRM before promoting to a full session.
struct PendingHandshake {
    peer_eph: [u8; 32],
    expected_mac2: [u8; 32],
    /// Session key derived eagerly — all three DH outputs are known once the
    /// responder has both ephemeral publics and its own static secret.
    session_key: SharedKey,
    peer_addr: SocketAddr,
    created_at: tokio::time::Instant,
}

/// The hole punch engine.
pub struct HolePuncher {
    socket: Arc<UdpSocket>,
    local_keypair: NodeKeypair,
    /// Active sessions keyed by remote node ID.
    sessions: Arc<RwLock<HashMap<String, PeerSession>>>,
    /// Half-open handshakes keyed by remote node ID (responder side).
    pending: Arc<RwLock<HashMap<String, PendingHandshake>>>,
    /// Every datagram the receive loop sees is fanned out here so punch
    /// attempts can await replies without racing the central loop on the
    /// socket (tokio delivers each datagram to exactly one recv()).
    inbound_tx: broadcast::Sender<(Vec<u8>, SocketAddr)>,
}

impl HolePuncher {
    pub fn new(socket: Arc<UdpSocket>, local_keypair: NodeKeypair) -> Self {
        let (inbound_tx, _) = broadcast::channel(256);
        Self {
            socket,
            local_keypair,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            pending: Arc::new(RwLock::new(HashMap::new())),
            inbound_tx,
        }
    }

    /// Feed a received datagram into the fan-out channel. Called by the
    /// overlay receive loop for EVERY packet so concurrent punches observe
    /// their replies without calling recv_from on the shared socket.
    pub fn ingest(&self, data: &[u8], from: SocketAddr) {
        let _ = self.inbound_tx.send((data.to_vec(), from));
    }

    fn subscribe(&self) -> broadcast::Receiver<(Vec<u8>, SocketAddr)> {
        self.inbound_tx.subscribe()
    }

    /// Initiate a hole punch to a remote endpoint.
    ///
    /// Sends PUNCH packets repeatedly until an authenticated PUNCH_ACK is
    /// received, then sends PUNCH_CONFIRM. Returns once the session is fully
    /// established and mutually authenticated.
    pub async fn punch(&self, remote: &Endpoint) -> Result<()> {
        let candidates = remote.candidates();

        // Try each candidate address (LAN first, then external)
        for addr in &candidates {
            match self.punch_addr(remote, *addr).await {
                Ok(_) => {
                    tracing::info!(
                        "Hole punch succeeded to {} via {}",
                        &remote.node_id[..remote.node_id.len().min(8)],
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
            &remote.node_id[..remote.node_id.len().min(8)]
        )))
    }

    /// Punch to a specific address.
    async fn punch_addr(&self, remote: &Endpoint, addr: SocketAddr) -> Result<()> {
        // Decode the remote node's long-term static public key. The claimed
        // identity is authenticated by mac1 below — a MITM answering faster
        // than the real peer cannot produce it.
        let remote_pubkey_bytes =
            hex::decode(&remote.node_id).map_err(|e| OverlayError::Crypto(e.to_string()))?;
        if remote_pubkey_bytes.len() != 32 {
            return Err(OverlayError::Crypto("invalid remote node ID length".into()));
        }
        let remote_static: [u8; 32] = remote_pubkey_bytes.try_into().unwrap();

        let eph = EphemeralKeypair::generate();
        let eph_pub = eph.public_bytes();
        let local_pub = *self.local_keypair.public.as_bytes();

        // PUNCH: [TAG_PUNCH | static_i(32) | eph_i(32)]
        let mut punch_pkt = Vec::with_capacity(65);
        punch_pkt.push(TAG_PUNCH);
        punch_pkt.extend_from_slice(&local_pub);
        punch_pkt.extend_from_slice(&eph_pub);

        let socket = self.socket.clone();
        let punch_pkt_clone = punch_pkt.clone();
        let addr_clone = addr;
        let send_task = tokio::spawn(async move {
            for _ in 0..MAX_PUNCH_RETRIES {
                let _ = socket.send_to(&punch_pkt_clone, addr_clone).await;
                sleep(PUNCH_RETRY_INTERVAL).await;
            }
        });

        // Wait for an authenticated ACK via the fan-out channel (never read
        // the shared socket directly — that races the central receive loop).
        let mut rx = self.subscribe();
        let ack_result = timeout(PUNCH_TIMEOUT, async {
            loop {
                match rx.recv().await {
                    Ok((data, from)) => {
                        if from == addr && data.len() >= 65 && data[0] == TAG_PUNCH_ACK {
                            let eph_r: [u8; 32] = data[1..33].try_into().unwrap();
                            let mac1: [u8; 32] = data[33..65].try_into().unwrap();
                            break Ok::<_, ()>((eph_r, mac1));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break Err(()),
                }
            }
        })
        .await;

        send_task.abort();

        let (eph_r, mac1) = match ack_result {
            Ok(Ok(pair)) => pair,
            Ok(Err(())) => return Err(OverlayError::HolePunch("packet channel closed".into())),
            Err(_) => return Err(OverlayError::Timeout),
        };

        // Verify mac1: proves the ACK came from whoever owns remote_static.
        let (dh_ee, dh_es, dh_se) =
            initiator_handshake_dhs(eph, &self.local_keypair, &eph_r, &remote_static);
        let k1 = dh_es;
        let expected_mac1 = hash_transcript(
            &[b"vt-overlay-mac1-v1"],
            &[&k1, &local_pub, &eph_pub, &eph_r],
        );
        if !constant_time_eq(&mac1, &expected_mac1) {
            return Err(OverlayError::Crypto(
                "PUNCH_ACK failed authentication (mac1 mismatch)".into(),
            ));
        }

        // Derive and store the session BEFORE sending CONFIRM so data can
        // flow immediately after.
        let session_key = SharedKey::from_raw(derive_session_key(dh_ee, dh_es, dh_se));
        let session = PeerSession {
            remote_endpoint: remote.clone(),
            remote_addr: addr,
            shared_key: session_key,
            remote_pubkey: remote_static,
            local_pubkey: local_pub,
            send_counter: 0,
            recv_counter: 0,
            created_at: tokio::time::Instant::now(),
        };
        self.sessions
            .write()
            .await
            .insert(remote.node_id.clone(), session);

        // PUNCH_CONFIRM: [TAG_CONFIRM | static_i(32) | eph_i(32) | mac2(32)]
        let k2 = dh_se;
        let mac2 = hash_transcript(
            &[b"vt-overlay-mac2-v1"],
            &[&k2, &local_pub, &eph_pub, &eph_r],
        );
        let mut confirm = Vec::with_capacity(97);
        confirm.push(TAG_PUNCH_CONFIRM);
        confirm.extend_from_slice(&local_pub);
        confirm.extend_from_slice(&eph_pub);
        confirm.extend_from_slice(&mac2);
        self.socket
            .send_to(&confirm, addr)
            .await
            .map_err(OverlayError::Io)?;

        Ok(())
    }

    /// Handle an incoming PUNCH packet (handshake message 1, responder side).
    ///
    /// Replies with an authenticated ACK and records a pending handshake; the
    /// session is only established when [`Self::handle_punch_confirm`] verifies
    /// the initiator's proof-of-possession of its claimed static key.
    pub async fn handle_punch(&self, from: SocketAddr, data: &[u8]) -> Result<()> {
        if data.len() < 65 || data[0] != TAG_PUNCH {
            return Err(OverlayError::HolePunch("invalid PUNCH packet".into()));
        }

        let peer_static: [u8; 32] = data[1..33].try_into().unwrap();
        let peer_eph: [u8; 32] = data[33..65].try_into().unwrap();
        let node_id = hex::encode(peer_static);

        // Evict stale pending entries and enforce cap before creating one.
        {
            let mut pending = self.pending.write().await;
            pending.retain(|_, p| p.created_at.elapsed() < PENDING_TTL);
            if pending.len() >= MAX_PENDING_HANDSHAKES {
                if let Some(oldest) = pending
                    .iter()
                    .min_by_key(|(_, p)| p.created_at)
                    .map(|(k, _)| k.clone())
                {
                    pending.remove(&oldest);
                }
            }
        }

        // Generate our ephemeral, compute the responder-side DHs, and prove
        // ownership of OUR static key with mac1.
        let our_eph = EphemeralKeypair::generate();
        let our_eph_pub = our_eph.public_bytes();
        let (dh_ee, dh_es, dh_se) =
            responder_handshake_dhs(our_eph, &self.local_keypair, &peer_eph, &peer_static);

        let k1 = dh_es;
        let mac1 = hash_transcript(
            &[b"vt-overlay-mac1-v1"],
            &[&k1, &peer_static, &peer_eph, &our_eph_pub],
        );

        // PUNCH_ACK: [TAG_ACK | our_eph_pub(32) | mac1(32)]
        let mut ack = Vec::with_capacity(65);
        ack.push(TAG_PUNCH_ACK);
        ack.extend_from_slice(&our_eph_pub);
        ack.extend_from_slice(&mac1);
        self.socket
            .send_to(&ack, from)
            .await
            .map_err(OverlayError::Io)?;

        // mac2 the initiator will send (we can precompute the expectation).
        let k2 = dh_se;
        let expected_mac2 = hash_transcript(
            &[b"vt-overlay-mac2-v1"],
            &[&k2, &peer_static, &peer_eph, &our_eph_pub],
        );

        // Stash the DHs inside a reconstructed ephemeral? No — instead store
        // the derived session key material now: the three DH outputs are
        // complete, so the pending entry only needs the expected mac2 and the
        // final key.
        let session_key = SharedKey::from_raw(derive_session_key(dh_ee, dh_es, dh_se));

        self.pending.write().await.insert(
            node_id,
            PendingHandshake {
                peer_eph,
                expected_mac2,
                created_at: tokio::time::Instant::now(),
                session_key,
                peer_addr: from,
            },
        );

        tracing::debug!(%from, "Sent authenticated PUNCH_ACK, awaiting confirm");
        Ok(())
    }

    /// Handle an incoming PUNCH_CONFIRM packet (handshake message 3).
    ///
    /// Verifies the initiator owns its claimed static key, then promotes the
    /// pending handshake to a full session. Returns the peer's node ID on
    /// success so the caller can emit PeerConnected.
    pub async fn handle_punch_confirm(&self, from: SocketAddr, data: &[u8]) -> Result<String> {
        if data.len() < 97 || data[0] != TAG_PUNCH_CONFIRM {
            return Err(OverlayError::HolePunch(
                "invalid PUNCH_CONFIRM packet".into(),
            ));
        }
        let peer_static: [u8; 32] = data[1..33].try_into().unwrap();
        let peer_eph: [u8; 32] = data[33..65].try_into().unwrap();
        let mac2: [u8; 32] = data[65..97].try_into().unwrap();
        let node_id = hex::encode(peer_static);

        // Evict stale pendings first.
        {
            let mut pending = self.pending.write().await;
            pending.retain(|_, p| p.created_at.elapsed() < PENDING_TTL);
        }

        let pending = {
            let mut pending_map = self.pending.write().await;
            pending_map.remove(&node_id)
        };
        let Some(pending) = pending else {
            return Err(OverlayError::HolePunch(format!(
                "no pending handshake for {}",
                &node_id[..node_id.len().min(16)]
            )));
        };

        if pending.peer_eph != peer_eph || pending.peer_addr != from {
            return Err(OverlayError::HolePunch(
                "handshake transcript mismatch".into(),
            ));
        }
        if !constant_time_eq(&mac2, &pending.expected_mac2) {
            return Err(OverlayError::Crypto(
                "PUNCH_CONFIRM failed authentication (mac2 mismatch)".into(),
            ));
        }

        // Evict stale sessions and enforce cap before creating a new one.
        {
            let mut sessions = self.sessions.write().await;
            sessions.retain(|_, s| s.created_at.elapsed() < SESSION_TTL);
            if sessions.len() >= MAX_SESSIONS {
                if let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, s)| s.created_at)
                    .map(|(k, _)| k.clone())
                {
                    sessions.remove(&oldest);
                }
            }
        }

        let session = PeerSession {
            remote_endpoint: Endpoint::new(node_id.clone(), from),
            remote_addr: from,
            shared_key: pending.session_key,
            remote_pubkey: peer_static,
            local_pubkey: *self.local_keypair.public.as_bytes(),
            send_counter: 0,
            recv_counter: 0,
            created_at: tokio::time::Instant::now(),
        };
        self.sessions.write().await.insert(node_id.clone(), session);

        tracing::info!(%from, "Authenticated hole punch established");
        Ok(node_id)
    }

    /// Send encrypted data to a connected peer.
    pub async fn send_data(&self, node_id: &str, payload: &[u8]) -> Result<()> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(node_id)
            .ok_or_else(|| OverlayError::PeerNotFound(node_id.to_string()))?;

        session.send_counter += 1;
        let ct =
            session
                .shared_key
                .encrypt(session.send_counter, &session.local_pubkey, payload)?;

        // Include our static node ID so the receiver can select the correct
        // established session before authenticating and decrypting the packet.
        let mut pkt = Vec::with_capacity(1 + 32 + ct.len());
        pkt.push(TAG_DATA);
        pkt.extend_from_slice(self.local_keypair.public.as_bytes());
        pkt.extend_from_slice(&ct);

        self.socket
            .send_to(&pkt, session.remote_addr)
            .await
            .map_err(OverlayError::Io)?;
        Ok(())
    }

    /// Decrypt application data received from a connected peer.
    ///
    /// The packet must be the encrypted payload after the outer `TAG_DATA` byte.
    /// Rejects replayed packets by tracking the highest received counter.
    pub async fn decrypt_data(&self, node_id: &str, packet: &[u8]) -> Result<Vec<u8>> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(node_id)
            .ok_or_else(|| OverlayError::PeerNotFound(node_id.to_string()))?;

        // Replay protection: the packet's counter must be strictly greater than
        // the last one we accepted.
        if packet.len() < 4 {
            return Err(OverlayError::Crypto("packet too short".into()));
        }
        let counter = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        if counter <= session.recv_counter {
            return Err(OverlayError::Crypto("replayed packet".into()));
        }

        let plaintext = session.shared_key.decrypt(&session.remote_pubkey, packet)?;
        session.recv_counter = counter;
        Ok(plaintext)
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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

    #[tokio::test]
    async fn test_full_authenticated_handshake_over_udp() {
        // Two HolePunchers on loopback with a minimal central dispatch that
        // mirrors what the overlay receive_loop does.
        let sock_a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_b = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr_b = sock_b.local_addr().unwrap();

        let kp_a = NodeKeypair::generate();
        let kp_b = NodeKeypair::generate();
        let id_a = kp_a.node_id();
        let id_b = kp_b.node_id();

        let hp_a = Arc::new(HolePuncher::new(sock_a.clone(), kp_a));
        let hp_b = Arc::new(HolePuncher::new(sock_b.clone(), kp_b));

        // Central loops: read each socket, feed into ingest + handle tags.
        async fn central_loop(hp: Arc<HolePuncher>, sock: Arc<UdpSocket>) {
            let mut buf = [0u8; 65536];
            loop {
                let (n, from) = match sock.recv_from(&mut buf).await {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let data = &buf[..n];
                hp.ingest(data, from);
                match data[0] {
                    TAG_PUNCH => {
                        if let Err(e) = hp.handle_punch(from, data).await {
                            eprintln!("handle_punch error: {}", e);
                        }
                    }
                    TAG_PUNCH_CONFIRM => {
                        if let Err(e) = hp.handle_punch_confirm(from, data).await {
                            eprintln!("handle_punch_confirm error: {}", e);
                        }
                    }
                    _ => {}
                }
            }
        }

        let loop_a = tokio::spawn(central_loop(hp_a.clone(), sock_a.clone()));
        let loop_b = tokio::spawn(central_loop(hp_b.clone(), sock_b.clone()));

        let endpoint_b = Endpoint::new(id_b.clone(), addr_b);
        hp_a.punch(&endpoint_b)
            .await
            .expect("handshake must succeed");

        // The initiator's session exists immediately; the responder's session
        // is established when its central loop processes the CONFIRM, which
        // races our assert — poll briefly.
        assert!(hp_a.is_connected(&id_b).await);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !hp_b.is_connected(&id_a).await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "responder session was not established"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Encrypted data flows both ways.
        hp_a.send_data(&id_b, b"hello bob").await.unwrap();
        hp_b.send_data(&id_a, b"hi alice").await.unwrap();

        loop_a.abort();
        loop_b.abort();
    }

    #[tokio::test]
    async fn test_handshake_rejects_wrong_identity() {
        // The initiator targets node B's ID but node C (different static key)
        // answers. mac1 verification must fail and NO session may be stored.
        let sock_a = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sock_c = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr_c = sock_c.local_addr().unwrap();

        let kp_a = NodeKeypair::generate();
        let kp_b = NodeKeypair::generate(); // claimed identity
        let kp_c = NodeKeypair::generate(); // actual attacker
        let id_b = kp_b.node_id();

        let hp_a = Arc::new(HolePuncher::new(sock_a.clone(), kp_a));

        // Attacker C runs a responder loop using ITS OWN keypair.
        let hp_c = Arc::new(HolePuncher::new(sock_c.clone(), kp_c));
        let c_for_handle = hp_c.clone();
        let loop_c = tokio::spawn(async move {
            let mut buf = [0u8; 65536];
            loop {
                let (n, from) = sock_c.recv_from(&mut buf).await.unwrap();
                let data = &buf[..n];
                if data[0] == TAG_PUNCH {
                    let _ = c_for_handle.handle_punch(from, data).await;
                }
            }
        });

        let result = hp_a.punch(&Endpoint::new(id_b, addr_c)).await;
        assert!(result.is_err(), "MITM impersonation must fail");
        // And no session was established against the wrong identity.
        let count = hp_a.session_count().await;
        assert_eq!(count, 0, "no session should exist after failed auth");

        loop_c.abort();
    }
}
