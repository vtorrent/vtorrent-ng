use std::sync::Arc;
use tokio::sync::RwLock;
use vtorrent_node::events::NodeEvent;
use vtorrent_rpc::state::{AppState, PeerInfo};

pub(crate) struct SyncStatus {
    peers: Arc<RwLock<Vec<PeerInfo>>>,
    count: Arc<RwLock<usize>>,
    best_height: Arc<RwLock<u64>>,
    syncing: Arc<RwLock<bool>>,
}

impl SyncStatus {
    pub(crate) fn new(state: &AppState) -> Self {
        Self {
            peers: Arc::clone(&state.peer_list),
            count: Arc::clone(&state.peer_count),
            best_height: Arc::clone(&state.best_peer_height),
            syncing: Arc::clone(&state.syncing),
        }
    }

    pub(crate) async fn apply(&self, event: &NodeEvent, local_height: u32) {
        match event {
            NodeEvent::PeerConnected {
                addr,
                user_agent,
                height,
                ..
            } => {
                let mut peers = self.peers.write().await;
                let address = addr.to_string();
                if let Some(peer) = peers.iter_mut().find(|peer| peer.addr == address) {
                    peer.user_agent = user_agent.clone();
                    peer.best_height = *height;
                } else {
                    peers.push(PeerInfo {
                        addr: address,
                        user_agent: user_agent.clone(),
                        services: 0,
                        best_height: *height,
                    });
                }
            }
            NodeEvent::PeerDisconnected { addr } => {
                let address = addr.to_string();
                self.peers.write().await.retain(|peer| peer.addr != address);
            }
            NodeEvent::NewBlock { .. } | NodeEvent::Reorg { .. } => {}
            _ => return,
        }
        self.refresh(local_height).await;
    }

    pub(crate) async fn refresh(&self, local_height: u32) {
        let (count, best_height) = {
            let peers = self.peers.read().await;
            (
                peers.len(),
                peers
                    .iter()
                    .map(|peer| u64::from(peer.best_height))
                    .max()
                    .unwrap_or(0),
            )
        };
        *self.count.write().await = count;
        *self.best_height.write().await = best_height;
        *self.syncing.write().await = count == 0 || u64::from(local_height) < best_height;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> SyncStatus {
        SyncStatus {
            peers: Arc::new(RwLock::new(Vec::new())),
            count: Arc::new(RwLock::new(0)),
            best_height: Arc::new(RwLock::new(0)),
            syncing: Arc::new(RwLock::new(false)),
        }
    }

    fn connected(port: u16, height: u32) -> NodeEvent {
        NodeEvent::PeerConnected {
            addr: ([127, 0, 0, 1], port).into(),
            user_agent: "test-peer".into(),
            version: 3,
            height,
        }
    }

    fn disconnected(port: u16) -> NodeEvent {
        NodeEvent::PeerDisconnected {
            addr: ([127, 0, 0, 1], port).into(),
        }
    }

    #[tokio::test]
    async fn reconnect_at_tip_clears_syncing() {
        let status = tracker();
        status.apply(&connected(1, 100), 100).await;
        status.apply(&disconnected(1), 100).await;
        assert!(*status.syncing.read().await);
        assert_eq!(*status.best_height.read().await, 0);
        status.apply(&connected(1, 100), 100).await;
        assert!(!*status.syncing.read().await);
        assert_eq!(*status.count.read().await, 1);
    }

    #[tokio::test]
    async fn ahead_peer_requires_catchup() {
        let status = tracker();
        status.apply(&connected(1, 102), 100).await;
        assert!(*status.syncing.read().await);
        status.refresh(101).await;
        assert!(*status.syncing.read().await);
        status.refresh(102).await;
        assert!(!*status.syncing.read().await);
        status.refresh(103).await;
        assert!(!*status.syncing.read().await);
    }

    #[tokio::test]
    async fn disconnect_removes_stale_high_target() {
        let status = tracker();
        status.apply(&connected(1, 100), 100).await;
        status.apply(&connected(2, 500), 100).await;
        assert!(*status.syncing.read().await);
        status.apply(&disconnected(2), 100).await;
        assert!(!*status.syncing.read().await);
        assert_eq!(*status.best_height.read().await, 100);
    }

    #[tokio::test]
    async fn duplicate_peer_events_do_not_drift_counts() {
        let status = tracker();
        status.apply(&connected(1, 200), 100).await;
        status.apply(&connected(1, 100), 100).await;
        assert_eq!(*status.count.read().await, 1);
        assert_eq!(*status.best_height.read().await, 100);
        assert!(!*status.syncing.read().await);
        status.apply(&disconnected(2), 100).await;
        assert_eq!(*status.count.read().await, 1);
        status.apply(&disconnected(1), 100).await;
        status.apply(&disconnected(1), 100).await;
        assert_eq!(*status.count.read().await, 0);
        assert!(*status.syncing.read().await);
    }

    #[tokio::test]
    async fn reorg_recomputes_syncing_from_current_height() {
        let status = tracker();
        status.apply(&connected(1, 100), 100).await;
        let event = NodeEvent::Reorg {
            old_tip: [0; 32],
            new_tip: [1; 32],
            depth: 1,
            rolled_back_blocks: Vec::new(),
            applied_fork_blocks: Vec::new(),
        };
        status.apply(&event, 99).await;
        assert!(*status.syncing.read().await);
        status.apply(&event, 100).await;
        assert!(!*status.syncing.read().await);
    }
}
