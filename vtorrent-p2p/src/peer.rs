use serde_json;
/// Peer connection handler.
///
/// Manages a single TCP connection to a remote peer, handling the version
/// handshake, message dispatch, and keepalive pings.
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::{
    codec::VtrCodec,
    message::{NetMessage, PingMsg, VersionMsg},
};

/// Events emitted by a peer connection to the node.
#[derive(Debug)]
pub enum PeerEvent {
    /// A new message was received from the peer.
    Message {
        peer_addr: SocketAddr,
        msg: NetMessage,
    },
    /// The peer disconnected.
    Disconnected { peer_addr: SocketAddr },
    /// The version handshake completed successfully.
    HandshakeComplete {
        peer_addr: SocketAddr,
        version: VersionMsg,
    },
}

/// Commands sent from the node to a peer connection.
#[derive(Debug)]
pub enum PeerCommand {
    /// Send a message to this peer.
    Send(NetMessage),
    /// Disconnect from this peer.
    Disconnect,
}

/// State of a peer connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerState {
    /// Initial state — version handshake not yet complete.
    Connecting,
    /// Version handshake complete — peer is fully connected.
    Connected,
    /// Peer is disconnecting.
    Disconnecting,
}

/// A connected peer.
pub struct Peer {
    pub addr: SocketAddr,
    pub state: PeerState,
    pub best_height: u32,
    pub user_agent: String,
    pub services: u64,
    /// Channel for sending commands to this peer's task.
    pub cmd_tx: mpsc::Sender<PeerCommand>,
}

/// Spawn a peer connection task.
///
/// Handles the full lifecycle of a peer connection:
/// 1. Send our version message.
/// 2. Wait for the peer's version + verack.
/// 3. Dispatch incoming messages to the node via `event_tx`.
/// 4. Forward outgoing messages from `cmd_rx` to the peer.
///
/// `sent_nonces` carries version nonces we have sent, shared across peer
/// tasks so a node can detect its own connection to itself (NAT-reflected
/// dials etc.) and drop it.
pub type SentNonceRegistry = std::sync::Arc<std::sync::Mutex<std::collections::HashSet<u64>>>;

pub struct PeerTaskConfig {
    pub our_best_height: u32,
    pub our_addr: String,
    pub event_tx: mpsc::Sender<PeerEvent>,
    pub cmd_rx: mpsc::Receiver<PeerCommand>,
    pub sent_nonces: SentNonceRegistry,
    pub network_magic: [u8; 4],
}

pub async fn run_peer(stream: TcpStream, addr: SocketAddr, config: PeerTaskConfig) {
    use futures::{SinkExt, StreamExt};

    let mut framed = Framed::new(stream, VtrCodec::new(config.network_magic));
    let mut cmd_rx = config.cmd_rx;

    // Send our version message
    let version = VersionMsg::new(config.our_best_height, &config.our_addr);
    // Record OUR nonce so a reflected self-connection (we receive a version
    // carrying a nonce we ourselves sent) is detected and dropped.
    {
        let mut reg = config.sent_nonces.lock().unwrap_or_else(|e| e.into_inner());
        if reg.len() > 1024 {
            reg.clear();
        }
        reg.insert(version.nonce);
    }
    let payload = match serde_json::to_vec(&version) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to serialize version: {}", e);
            return;
        }
    };

    if let Err(e) = framed.send(NetMessage::new("version", payload)).await {
        tracing::warn!("Failed to send version to {}: {}", addr, e);
        return;
    }

    let mut handshake_done = false;
    let mut peer_version: Option<VersionMsg> = None;

    // The handshake must complete within this window or the connection is
    // dropped. Use a fixed deadline (not a re-created sleep) so a peer cannot
    // reset the timer by trickling messages.
    let handshake_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);

    // Post-handshake idle timeout: a connected peer must send *something*
    // (ping, message, or complete handshake) within this window. Without it
    // a silent peer holds its inbound slot and its receive buffer forever.
    const IDLE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(15 * 60);
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;

    loop {
        tokio::select! {
            // Incoming message from peer
            Some(result) = framed.next() => {
                match result {
                    Ok(msg) => {
                        // Reset the idle window on any traffic.
                        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
                        let cmd = msg.command_str().to_string();
                        tracing::debug!("Received '{}' from {}", cmd, addr);

                        // Before the handshake completes only version/verack/
                        // ping are processed; everything else is dropped so
                        // unauthenticated (or banned-pending-disconnect)
                        // peers cannot drive node logic.
                        if !handshake_done
                            && !matches!(cmd.as_str(), "version" | "verack" | "ping")
                        {
                            tracing::debug!(
                                "Dropping pre-handshake '{}' from {}",
                                cmd,
                                addr
                            );
                            continue;
                        }

                        match cmd.as_str() {
                            "version" => {
                                // Parse and store peer version (V2 bincode with JSON fallback)
                                let parsed = bincode::deserialize::<VersionMsg>(&msg.payload)
                                    .or_else(|_| serde_json::from_slice(&msg.payload));
                                if let Ok(v) = parsed {
                                    // Self-connection detection: if their
                                    // nonce is one WE recently sent, this
                                    // socket loops back to us.
                                    let is_self = config
                                        .sent_nonces
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .contains(&v.nonce);
                                    if is_self {
                                        tracing::warn!(
                                            "Self-connection detected from {} (nonce match); dropping",
                                            addr
                                        );
                                        break;
                                    }
                                    peer_version = Some(v);
                                }
                                // Send verack
                                let _ = framed.send(NetMessage::new("verack", vec![])).await;
                            }
                            "verack" => {
                                if !handshake_done {
                                    handshake_done = true;
                                    if let Some(ref v) = peer_version {
                                        let _ = config.event_tx.send(PeerEvent::HandshakeComplete {
                                            peer_addr: addr,
                                            version: v.clone(),
                                        }).await;
                                    }
                                }
                            }
                            "ping" => {
                                // Respond with pong (V2 bincode with JSON fallback — ping is tiny so JSON size is fine, but handle both)
                                let ping = bincode::deserialize::<PingMsg>(&msg.payload)
                                    .or_else(|_| serde_json::from_slice(&msg.payload));
                                if let Ok(ping) = ping {
                                    let pong = PingMsg { nonce: ping.nonce };
                                    // Keep pong as JSON for maximum compatibility (peer may be legacy)
                                    // Try bincode if peer already advertised V2, else JSON — but at peer level we don't know version yet, so send JSON
                                    if let Ok(payload) = serde_json::to_vec(&pong) {
                                        let _ = framed.send(NetMessage::new("pong", payload)).await;
                                    }
                                }
                            }
                            _ => {
                                // Forward all other messages to the node
                                let _ = config.event_tx.send(PeerEvent::Message {
                                    peer_addr: addr,
                                    msg,
                                }).await;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Peer {} error: {}", addr, e);
                        break;
                    }
                }
            }

            // Outgoing command from node
            Some(cmd) = cmd_rx.recv() => {
                match cmd {
                    PeerCommand::Send(msg) => {
                        if let Err(e) = framed.send(msg).await {
                            tracing::warn!("Failed to send to {}: {}", addr, e);
                            break;
                        }
                    }
                    PeerCommand::Disconnect => {
                        tracing::info!("Disconnecting from {}", addr);
                        break;
                    }
                }
            }

            // Handshake deadline: drop the connection if the peer never
            // completes the version/verack exchange. After the handshake,
            // enforce the idle timeout instead.
            _ = tokio::time::sleep_until(handshake_deadline), if !handshake_done => {
                tracing::warn!("Handshake timeout from {}", addr);
                break;
            }

            _ = tokio::time::sleep_until(idle_deadline), if handshake_done => {
                tracing::warn!("Idle timeout from {}", addr);
                break;
            }

            else => break,
        }
    }

    let _ = config
        .event_tx
        .send(PeerEvent::Disconnected { peer_addr: addr })
        .await;
    tracing::info!("Peer {} disconnected", addr);
}
