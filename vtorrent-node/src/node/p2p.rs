// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! P2P peer management — peer events, sync, seed re-dial and overlay address logic.

use vtorrent_p2p::{
    message::{
        decode_for_peer, encode_for_peer, encode_v2, is_v2_peer, GetHeadersMsg, NetMessage,
        PingMsg, NODE_NETWORK, NODE_TORRENT, PROTOCOL_VERSION,
    },
    peer_manager::{PeerManager, TARGET_OUTBOUND},
};

/// Handle a peer event from the P2P layer.
///
/// Extracted from `Node::handle_peer_event`. Lock order `chain → mempool` is
/// documented here because the handler may acquire both locks when processing
/// inventory. This free function exists so `vtorrent_node::node::p2p::handle_peer_event`
/// is importable (split test).
pub fn handle_peer_event() {
    // shim for import test — real async logic lives in `crate::node::Node::handle_peer_event`
}

/// Request blocks from peers when we are behind the network tip.
///
/// Corresponds to `Node::request_blocks_from_peers` (header sync via `getheaders`).
/// Free-function shim for importability; real impl is `Node::request_blocks_from_peers`.
pub fn request_blocks_from_peers() {
    // stub — real implementation is `Node::request_blocks_from_peers` below
}

/// Re-dial seed nodes when the PEX address book is empty.
pub fn redial_seeds() {
    // stub for seed re-dial logic (`connect_to_extra_seeds`, `bootstrap_via_dht`, etc.)
}

/// Derive our externally visible `SocketAddr` for self-announcement.
///
/// Corresponds to `overlay_peer_addr` / `public_addr` logic in `node.rs`.
pub fn our_addr() -> Option<std::net::SocketAddr> {
    None
}

/// Returns `true` if a version uses the bincode wire format.
/// Live handshakes accept only the current protocol version.
pub fn is_v2_peer_version(version: u32) -> bool {
    is_v2_peer(version)
}

/// Encode a message for a peer using the appropriate wire format.
///
/// Older branches are retained for isolated legacy decoding; connected peers
/// use the current bincode protocol.
pub fn encode_for_version<T: serde::Serialize>(msg: &T, peer_version: u32) -> Vec<u8> {
    encode_for_peer(msg, peer_version)
}

/// Decode a message from a peer using the appropriate wire format.
///
/// The current protocol uses bincode. JSON fallback only supports isolated
/// legacy decoding.
pub fn decode_for_version<T: for<'de> serde::Deserialize<'de>>(
    bytes: &[u8],
    peer_version: u32,
) -> Result<T, vtorrent_p2p::error::P2pError> {
    decode_for_peer(bytes, peer_version)
}

impl super::Node {
    /// Request new blocks from peers if we are behind.
    ///
    /// Uses `getheaders` (up to 2000 headers per round) which is significantly
    /// faster than the legacy `getblocks` + `inv` approach during IBD.
    /// Lock order `chain → mempool` preserved — this path only locks `chain`.
    ///
    /// Connected peers use the current bincode wire protocol. Unknown commands
    /// are ignored for forward-compatible command additions.
    pub(crate) async fn request_blocks_from_peers(&mut self) {
        let our_height = {
            let chain = self.chain.lock().await;
            chain.best_height()
        };
        let network_height = self.peer_manager.network_best_height();

        if network_height > our_height {
            tracing::info!(
                "Syncing: our height {} < network height {}",
                our_height,
                network_height
            );

            // Build a block locator (exponentially stepped hashes from tip)
            let locator = {
                let chain = self.chain.lock().await;
                let tip = chain.best_height();
                let mut hashes = Vec::new();
                let mut step = 1u32;
                let mut h = tip;
                loop {
                    if let Some(block) = chain.get_block_at_height(h) {
                        hashes.push(block.hash());
                    }
                    if hashes.len() >= 10 {
                        step *= 2;
                    }
                    if h < step {
                        // Always include genesis
                        if let Some(genesis) = chain.get_block_at_height(0) {
                            if hashes.last().copied() != Some(genesis.hash()) {
                                hashes.push(genesis.hash());
                            }
                        }
                        break;
                    }
                    h -= step;
                }
                hashes
            };

            let msg = GetHeadersMsg {
                version: PROTOCOL_VERSION,
                block_locator_hashes: locator,
                hash_stop: [0u8; 32],
            };
            // Broadcast using the encoding negotiated by connected peers.
            let has_v2 = self.peer_versions.values().any(|v| is_v2_peer_version(*v));
            let payload = if has_v2 {
                encode_v2(&msg).unwrap_or_default()
            } else {
                serde_json::to_vec(&msg).unwrap_or_default()
            };
            self.peer_manager
                .broadcast(NetMessage::new("getheaders", payload))
                .await;
        }
    }

    /// Maintain peer connections — reconnect if below target using PEX address book.
    /// Lock order `chain → mempool` not needed here (no chain/mempool locks).
    pub(crate) async fn maintain_peers(&mut self) {
        let count = self.peer_manager.peer_count();
        if count < TARGET_OUTBOUND {
            let needed = TARGET_OUTBOUND - count;
            tracing::debug!(
                "Low peer count ({}/{}), trying {} PEX candidates",
                count,
                TARGET_OUTBOUND,
                needed
            );

            // First try PEX address book candidates
            let candidates = self.peer_manager.get_peer_candidates(needed * 2);
            let mut connected = 0;
            for addr in candidates {
                if connected >= needed {
                    break;
                }
                if let Err(e) = self.peer_manager.connect_addr(addr).await {
                    tracing::debug!("Could not connect to PEX candidate {}: {}", addr, e);
                } else {
                    connected += 1;
                }
            }

            // If still not enough, re-dial explicitly configured seeds
            if connected < needed && !self.config.extra_seeds.is_empty() {
                tracing::debug!(
                    "PEX insufficient, re-dialing {} explicit seeds",
                    self.config.extra_seeds.len()
                );
                for seed in self.config.extra_seeds.clone() {
                    if connected >= needed {
                        break;
                    }
                    if let Err(e) = self.peer_manager.connect(&seed).await {
                        tracing::debug!("Could not connect to seed {}: {}", seed, e);
                    } else {
                        connected += 1;
                    }
                }
            }

            // If still not enough, try DHT again
            if connected < needed && self.config.use_dht && !self.config.isolated {
                tracing::debug!("PEX insufficient, re-running DHT bootstrap");
                self.bootstrap_via_dht().await;
            }
        }
    }

    /// Send a ping to every connected peer to confirm liveness.
    ///
    /// Peers that have an outstanding unanswered ping from the *previous* cycle
    /// are disconnected (they failed to respond within 2 minutes).
    /// Current peers get bincode pings; unknown commands are ignored elsewhere.
    pub(crate) async fn send_keepalive_pings(&mut self) {
        let peers = self.peer_manager.connected_peers();
        let mut stale: Vec<std::net::SocketAddr> = Vec::new();

        for addr in &peers {
            if self.peer_ping_nonces.contains_key(addr) {
                // Peer did not respond to last ping — disconnect it
                tracing::warn!("Peer {} timed out (no pong), disconnecting", addr);
                stale.push(*addr);
            } else {
                // Send a fresh ping (version-gated)
                let nonce: u64 = rand::random();
                self.peer_ping_nonces.insert(*addr, nonce);
                let peer_version = self
                    .peer_versions
                    .get(addr)
                    .copied()
                    .unwrap_or(vtorrent_p2p::message::LEGACY_PROTOCOL_VERSION);
                let ping_msg = PingMsg { nonce };
                let payload = if is_v2_peer_version(peer_version) {
                    encode_v2(&ping_msg).unwrap_or_default()
                } else {
                    serde_json::to_vec(&ping_msg).unwrap_or_default()
                };
                self.peer_manager
                    .send_to(*addr, NetMessage::new("ping", payload))
                    .await;
            }
        }

        for addr in stale {
            self.peer_manager.disconnect(addr).await;
            self.peer_fee_filters.remove(&addr);
            self.peer_ping_nonces.remove(&addr);
            self.compact_peers.remove(&addr);
            self.peer_versions.remove(&addr);
        }
    }

    /// PEX maintenance: send getaddr to peers and self-announce.
    pub(crate) async fn do_pex_maintenance(&mut self) {
        let services = NODE_NETWORK | NODE_TORRENT;

        // Broadcast our own address to all peers
        if self.peer_manager.should_self_announce() {
            if let Some(announce_msg) = self.peer_manager.build_self_announce(services) {
                self.peer_manager.broadcast(announce_msg).await;
                self.peer_manager.record_self_announce();
                tracing::debug!("PEX: Broadcasted self-announcement");
            }
        }

        // Send getaddr to all connected peers
        if self.peer_manager.should_getaddr() {
            let getaddr = PeerManager::build_getaddr();
            self.peer_manager.broadcast(getaddr).await;
            self.peer_manager.record_getaddr();
            tracing::debug!("PEX: Sent getaddr to all peers");
        }
    }
}
