//! Peer lifecycle and overlay ingress handling.
//!
//! Extracted from `node/mod.rs`: the `PeerEvent` dispatcher (handshake,
//! message routing, disconnect cleanup) and the authenticated overlay ingress
//! path that bridges overlay sessions into the same P2P command pipeline.

use tokio::sync::mpsc;
use vtorrent_p2p::{
    compact::CompactBlockPeerState,
    message::{
        encode_v2, is_v2_peer, GetBlocksMsg, NetMessage, SendCmpctMsg, VersionMsg, PROTOCOL_VERSION,
    },
    peer::{PeerCommand, PeerEvent},
    peer_manager::PeerManager,
};

use crate::{
    error::{NodeError, Result},
    events::NodeEvent,
};

use super::{
    overlay::{encode_overlay_message, overlay_peer_addr},
    Node, OverlayIngress,
};

impl Node {
    /// Handle a peer lifecycle event: handshake, message, or disconnect.
    pub(crate) async fn handle_peer_event(&mut self, event: PeerEvent) -> Result<()> {
        match event {
            PeerEvent::HandshakeComplete { peer_addr, version } => {
                if version.version != PROTOCOL_VERSION {
                    tracing::warn!(
                        "Disconnecting peer {} with incompatible protocol version {}",
                        peer_addr,
                        version.version
                    );
                    self.peer_manager.disconnect(peer_addr).await;
                    return Ok(());
                }
                // Cap the user agent: it is an unbounded peer-supplied string
                // that gets stored and logged per peer.
                let mut user_agent = version.user_agent;
                user_agent.truncate(128);
                tracing::info!(
                    "Peer {} handshake complete: {} (height {}) v{}",
                    peer_addr,
                    user_agent,
                    version.start_height,
                    version.version
                );
                // Track the exact version accepted by the hard-boundary handshake.
                self.peer_versions.insert(peer_addr, version.version);
                self.emit(NodeEvent::PeerConnected {
                    addr: peer_addr,
                    user_agent,
                    version: version.version,
                    height: version.start_height,
                });
                // Negotiate compact block relay (BIP-152)
                // We use low-bandwidth mode (0) by default; high-bandwidth (1) is for the 3 fastest peers
                let sendcmpct_payload = if is_v2_peer(version.version) {
                    encode_v2(&SendCmpctMsg {
                        high_bandwidth: false,
                        version: 1,
                    })
                    .unwrap_or_default()
                } else {
                    match serde_json::to_vec(&SendCmpctMsg {
                        high_bandwidth: false,
                        version: 1,
                    }) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!("Failed to serialize sendcmpct: {}", e);
                            return Ok(());
                        }
                    }
                };
                self.peer_manager
                    .send_to(peer_addr, NetMessage::new("sendcmpct", sendcmpct_payload))
                    .await;
                // Track compact block state for this peer
                self.compact_peers
                    .insert(peer_addr, CompactBlockPeerState::default());
                // Ask peer for blocks if they are ahead of us
                let our_height = {
                    let chain = self.chain.lock().await;
                    chain.best_height()
                };
                if version.start_height > our_height {
                    let best_hash = {
                        let chain = self.chain.lock().await;
                        chain.best_hash().unwrap_or([0u8; 32])
                    };
                    let msg_payload = GetBlocksMsg {
                        version: PROTOCOL_VERSION,
                        block_locator_hashes: vec![best_hash],
                        hash_stop: [0u8; 32],
                    };
                    let payload = if is_v2_peer(version.version) {
                        encode_v2(&msg_payload).unwrap_or_default()
                    } else {
                        serde_json::to_vec(&msg_payload).unwrap_or_default()
                    };
                    let msg = NetMessage::new("getblocks", payload);
                    self.peer_manager.broadcast(msg).await;
                }

                // Immediately request peer's address list (PEX bootstrap)
                if self.peer_manager.should_getaddr() {
                    let getaddr = PeerManager::build_getaddr();
                    self.peer_manager.send_to(peer_addr, getaddr).await;
                    self.peer_manager.record_getaddr();
                }
            }

            PeerEvent::Message { peer_addr, msg } => {
                self.handle_message(peer_addr, msg).await?;
            }

            PeerEvent::Disconnected { peer_addr } => {
                tracing::info!("Peer {} disconnected", peer_addr);
                self.emit(NodeEvent::PeerDisconnected { addr: peer_addr });
                // Clean up per-peer state
                self.compact_peers.remove(&peer_addr);
                self.peer_fee_filters.remove(&peer_addr);
                self.peer_ping_nonces.remove(&peer_addr);
                self.peer_msg_counts.remove(&peer_addr);
                self.peer_versions.remove(&peer_addr);
            }
        }
        Ok(())
    }

    /// Handle an authenticated overlay event through the same P2P command path
    /// used for TCP peers. Overlay sessions retain their own encryption and send
    /// bridge, while protocol semantics stay centralized in `handle_message`.
    pub(crate) async fn handle_overlay_ingress(&mut self, ingress: OverlayIngress) -> Result<()> {
        match ingress {
            OverlayIngress::PeerConnected { node_id } => {
                let peer_addr = overlay_peer_addr(&node_id)?;
                let overlay = self.overlay.clone().ok_or_else(|| {
                    NodeError::Chain("Received overlay peer event before overlay startup".into())
                })?;
                let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
                let relay_node_id = node_id.clone();

                tokio::spawn(async move {
                    while let Some(command) = cmd_rx.recv().await {
                        match command {
                            PeerCommand::Send(msg) => {
                                let packet = encode_overlay_message(&msg);
                                if let Err(e) = overlay.send(&relay_node_id, &packet).await {
                                    tracing::debug!(peer = %relay_node_id, "Overlay send failed: {}", e);
                                    break;
                                }
                            }
                            PeerCommand::Disconnect => break,
                        }
                    }
                });

                self.peer_manager
                    .register_virtual_peer(
                        peer_addr,
                        format!("/vTorrent-overlay:{}/", &node_id[..8]),
                        cmd_tx,
                    )
                    .map_err(|e| {
                        NodeError::Chain(format!("Overlay peer registration failed: {}", e))
                    })?;
                self.overlay_peers.insert(peer_addr, node_id);
                self.overlay_handshaken.remove(&peer_addr);

                let best_height = {
                    let chain = self.chain.lock().await;
                    chain.best_height()
                };
                let version = VersionMsg::new(best_height, &self.config.listen_addr);
                let payload = serde_json::to_vec(&version).map_err(|e| {
                    NodeError::Chain(format!("Overlay version serialization failed: {}", e))
                })?;
                self.peer_manager
                    .send_to(peer_addr, NetMessage::new("version", payload))
                    .await;
            }
            OverlayIngress::PeerDisconnected { node_id } => {
                if let Some(peer_addr) = self
                    .overlay_peers
                    .iter()
                    .find_map(|(addr, known_id)| (known_id == &node_id).then_some(*addr))
                {
                    self.peer_manager.remove_virtual_peer(peer_addr);
                    self.overlay_peers.remove(&peer_addr);
                    self.overlay_handshaken.remove(&peer_addr);
                    self.compact_peers.remove(&peer_addr);
                    self.peer_fee_filters.remove(&peer_addr);
                    self.peer_ping_nonces.remove(&peer_addr);
                    self.peer_versions.remove(&peer_addr);
                    self.emit(NodeEvent::PeerDisconnected { addr: peer_addr });
                }
            }
            OverlayIngress::Message { node_id, msg } => {
                let peer_addr = overlay_peer_addr(&node_id)?;
                if self.overlay_peers.get(&peer_addr) != Some(&node_id) {
                    tracing::debug!(peer = %node_id, "Ignoring message from unknown overlay peer");
                    return Ok(());
                }

                match msg.command_str() {
                    "version" => {
                        let version: VersionMsg = bincode::deserialize::<VersionMsg>(&msg.payload)
                            .or_else(|_| serde_json::from_slice(&msg.payload))
                            .map_err(|e| {
                                NodeError::Chain(format!("Invalid overlay version message: {}", e))
                            })?;
                        self.peer_manager
                            .send_to(peer_addr, NetMessage::new("verack", Vec::new()))
                            .await;
                        if self.overlay_handshaken.insert(peer_addr) {
                            self.handle_peer_event(PeerEvent::HandshakeComplete {
                                peer_addr,
                                version,
                            })
                            .await?;
                        }
                    }
                    "verack" => {
                        tracing::trace!(peer = %node_id, "Overlay peer acknowledged version");
                    }
                    _ if !self.overlay_handshaken.contains(&peer_addr) => {
                        tracing::debug!(peer = %node_id, command = msg.command_str(), "Ignoring pre-handshake overlay message");
                    }
                    _ => self.handle_message(peer_addr, msg).await?,
                }
            }
        }
        Ok(())
    }
}
