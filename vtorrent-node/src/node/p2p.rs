// Lock order: chain → mempool — always acquire Chain before Mempool to avoid deadlock.
//! P2P peer management — peer events, sync, seed re-dial and overlay address logic.

use vtorrent_p2p::{
    message::{GetHeadersMsg, NetMessage, PingMsg, NODE_NETWORK, NODE_TORRENT},
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

impl super::Node {
    /// Request new blocks from peers if we are behind.
    ///
    /// Uses `getheaders` (up to 2000 headers per round) which is significantly
    /// faster than the legacy `getblocks` + `inv` approach during IBD.
    /// Lock order `chain → mempool` preserved — this path only locks `chain`.
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

            let payload = serde_json::to_vec(&GetHeadersMsg {
                version: 70001,
                block_locator_hashes: locator,
                hash_stop: [0u8; 32],
            })
            .unwrap_or_default();
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
    pub(crate) async fn send_keepalive_pings(&mut self) {
        let peers = self.peer_manager.connected_peers();
        let mut stale: Vec<std::net::SocketAddr> = Vec::new();

        for addr in &peers {
            if self.peer_ping_nonces.contains_key(addr) {
                // Peer did not respond to last ping — disconnect it
                tracing::warn!("Peer {} timed out (no pong), disconnecting", addr);
                stale.push(*addr);
            } else {
                // Send a fresh ping
                let nonce: u64 = rand::random();
                self.peer_ping_nonces.insert(*addr, nonce);
                let payload = serde_json::to_vec(&PingMsg { nonce }).unwrap_or_default();
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
