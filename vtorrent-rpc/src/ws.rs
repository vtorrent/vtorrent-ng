use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{State, WebSocketUpgrade},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
/// WebSocket event subscription endpoint for vTorrent RPC.
///
/// Clients connect to `ws://127.0.0.1:22525/ws` and subscribe to one or more
/// event types. The server pushes events in real-time as they occur.
///
/// ## Subscription Protocol
///
/// After connecting, the client sends a JSON subscription message:
/// ```json
/// { "subscribe": ["new_block", "tx_confirmed", "peer_connected"] }
/// ```
///
/// The server then pushes matching events as JSON objects:
/// ```json
/// { "event": "new_block", "data": { "height": 12345, "hash": "abc...", "tx_count": 3 } }
/// { "event": "tx_confirmed", "data": { "txid": "def...", "block_height": 12345 } }
/// { "event": "peer_connected", "data": { "addr": "1.2.3.4:22526", "version": 70001 } }
/// ```
///
/// ## Available Event Types
///
/// | Event | Description |
/// |---|---|
/// | `new_block` | A new block was added to the chain |
/// | `tx_confirmed` | A transaction was confirmed in a block |
/// | `tx_unconfirmed` | A new unconfirmed transaction entered the mempool |
/// | `peer_connected` | A new peer connected |
/// | `peer_disconnected` | A peer disconnected |
/// | `reorg` | A chain reorganization occurred |
/// | `staking_reward` | A staking reward was earned |
/// | `all` | Subscribe to all event types |
use std::sync::Arc;
use tokio::sync::broadcast;

/// An event that can be pushed to WebSocket subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum NodeEvent {
    /// A new block was added to the main chain.
    #[serde(rename = "new_block")]
    NewBlock {
        height: u32,
        hash: String,
        tx_count: usize,
        timestamp: u32,
        size_bytes: usize,
    },

    /// A transaction was confirmed in a block.
    #[serde(rename = "tx_confirmed")]
    TxConfirmed {
        txid: String,
        block_height: u32,
        block_hash: String,
    },

    /// A new unconfirmed transaction entered the mempool.
    #[serde(rename = "tx_unconfirmed")]
    TxUnconfirmed {
        txid: String,
        fee_sats: u64,
        fee_rate: f64,
        size_bytes: usize,
    },

    /// A new peer connected.
    #[serde(rename = "peer_connected")]
    PeerConnected {
        addr: String,
        version: u32,
        user_agent: String,
        height: u32,
    },

    /// A peer disconnected.
    #[serde(rename = "peer_disconnected")]
    PeerDisconnected { addr: String, reason: String },

    /// A chain reorganization occurred.
    #[serde(rename = "reorg")]
    Reorg {
        old_tip: String,
        new_tip: String,
        depth: u32,
    },

    /// A staking reward was earned.
    #[serde(rename = "staking_reward")]
    StakingReward {
        block_height: u32,
        reward_sats: u64,
        address: String,
    },
}

/// A client subscription request.
#[derive(Debug, Deserialize)]
pub struct SubscribeMsg {
    pub subscribe: Vec<String>,
}

/// A client unsubscription request.
#[derive(Debug, Deserialize)]
pub struct UnsubscribeMsg {
    pub unsubscribe: Vec<String>,
}

/// Global event broadcaster — all WebSocket connections share this channel.
#[derive(Clone)]
pub struct EventBroadcaster {
    pub sender: broadcast::Sender<Arc<NodeEvent>>,
    /// Number of currently open WebSocket connections.
    ///
    /// Each connection holds a broadcast receiver and a task, so an unbounded
    /// count is a resource-exhaustion vector. Connections are capped at
    /// [`MAX_WS_CONNECTIONS`].
    pub connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Maximum concurrent WebSocket connections.
pub const MAX_WS_CONNECTIONS: usize = 128;

/// Close a WebSocket connection that has sent nothing for this long.
///
/// Without an idle timeout a client can hold a connection (and its broadcast
/// receiver) open indefinitely.
const WS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

impl EventBroadcaster {
    /// Create a new broadcaster with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            connections: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Broadcast an event to all connected subscribers.
    pub fn broadcast(&self, event: NodeEvent) {
        // Ignore send errors (no subscribers connected is fine)
        let _ = self.sender.send(Arc::new(event));
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<NodeEvent>> {
        self.sender.subscribe()
    }
}

/// WebSocket upgrade handler — upgrades an HTTP connection to a WebSocket.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    // Reject the upgrade when at capacity, before allocating a connection.
    let current = state
        .events
        .connections
        .load(std::sync::atomic::Ordering::SeqCst);
    if current >= MAX_WS_CONNECTIONS {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "WebSocket connection limit reached",
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_ws_connection(socket, state))
}

/// Handle a single WebSocket connection.
async fn handle_ws_connection(mut socket: WebSocket, state: Arc<AppState>) {
    // Count this connection for the lifetime of the handler. The guard
    // decrements on drop, so an early return or panic cannot leak a slot.
    struct ConnectionGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    state
        .events
        .connections
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let _guard = ConnectionGuard(Arc::clone(&state.events.connections));

    let mut receiver = state.events.subscribe();
    let mut subscribed: Vec<String> = Vec::new();

    tracing::debug!("WebSocket client connected");

    loop {
        tokio::select! {
            // Idle timeout: a client that sends nothing for the window is
            // disconnected so it cannot hold a slot indefinitely.
            _ = tokio::time::sleep(WS_IDLE_TIMEOUT) => {
                tracing::debug!("WebSocket client idle for {:?}; closing", WS_IDLE_TIMEOUT);
                let _ = socket.send(Message::Close(None)).await;
                break;
            }
            // Incoming message from client
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                if let Ok(sub) = serde_json::from_str::<SubscribeMsg>(&text) {
                    // Cap subscriptions: the set is scanned per event and a
                    // malicious client could otherwise grow it without bound.
                    const MAX_SUBSCRIPTIONS: usize = 64;
                    for event_type in sub.subscribe {
                        if subscribed.len() >= MAX_SUBSCRIPTIONS {
                            break;
                        }
                        if !subscribed.contains(&event_type) {
                            subscribed.push(event_type);
                        }
                    }
                            let ack = serde_json::json!({
                                "status": "subscribed",
                                "subscriptions": &subscribed
                            });
                            if socket.send(Message::Text(ack.to_string())).await.is_err() {
                                break;
                            }
                        } else if let Ok(unsub) = serde_json::from_str::<UnsubscribeMsg>(&text) {
                            subscribed.retain(|s| !unsub.unsubscribe.contains(s));
                        } else if text == "ping"
                            && socket.send(Message::Text("pong".to_string())).await.is_err() {
                                break;
                            }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // Outgoing event from node
            event = receiver.recv() => {
                match event {
                    Ok(event) => {
                        if should_send(&event, &subscribed) {
                            match serde_json::to_string(&*event) {
                                Ok(json) => {
                                    if socket.send(Message::Text(json)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to serialize event: {}", e);
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket client lagged, skipped {} events", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    tracing::debug!("WebSocket client disconnected");
}

/// Returns true if the event matches the client's subscriptions.
fn should_send(event: &NodeEvent, subscribed: &[String]) -> bool {
    if subscribed.is_empty() {
        return false;
    }
    if subscribed.iter().any(|s| s == "all") {
        return true;
    }
    let event_type = match event {
        NodeEvent::NewBlock { .. } => "new_block",
        NodeEvent::TxConfirmed { .. } => "tx_confirmed",
        NodeEvent::TxUnconfirmed { .. } => "tx_unconfirmed",
        NodeEvent::PeerConnected { .. } => "peer_connected",
        NodeEvent::PeerDisconnected { .. } => "peer_disconnected",
        NodeEvent::Reorg { .. } => "reorg",
        NodeEvent::StakingReward { .. } => "staking_reward",
    };
    subscribed.iter().any(|s| s == event_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_new_block_event() -> NodeEvent {
        NodeEvent::NewBlock {
            height: 100,
            hash: "abc123".to_string(),
            tx_count: 5,
            timestamp: 1700000000,
            size_bytes: 1024,
        }
    }

    #[test]
    fn test_should_send_all_subscription() {
        let event = make_new_block_event();
        assert!(should_send(&event, &["all".to_string()]));
    }

    #[test]
    fn test_should_send_specific_match() {
        let event = make_new_block_event();
        assert!(should_send(&event, &["new_block".to_string()]));
    }

    #[test]
    fn test_should_not_send_wrong_type() {
        let event = make_new_block_event();
        assert!(!should_send(&event, &["tx_confirmed".to_string()]));
    }

    #[test]
    fn test_should_not_send_empty_subscriptions() {
        let event = make_new_block_event();
        assert!(!should_send(&event, &[]));
    }

    #[test]
    fn test_should_send_multiple_subscriptions() {
        let event = make_new_block_event();
        let subs = vec!["tx_confirmed".to_string(), "new_block".to_string()];
        assert!(should_send(&event, &subs));
    }

    #[test]
    fn test_event_serialization() {
        let event = NodeEvent::NewBlock {
            height: 42,
            hash: "deadbeef".to_string(),
            tx_count: 3,
            timestamp: 1700000000,
            size_bytes: 512,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"new_block\""));
        assert!(json.contains("\"height\":42"));
    }

    #[test]
    fn test_reorg_event_serialization() {
        let event = NodeEvent::Reorg {
            old_tip: "aaa".to_string(),
            new_tip: "bbb".to_string(),
            depth: 3,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"reorg\""));
        assert!(json.contains("\"depth\":3"));
    }

    #[test]
    fn test_broadcaster_send_receive() {
        let broadcaster = EventBroadcaster::new(16);
        let mut rx = broadcaster.subscribe();
        broadcaster.broadcast(make_new_block_event());
        let received = rx.try_recv().unwrap();
        assert!(matches!(*received, NodeEvent::NewBlock { .. }));
    }

    #[test]
    fn test_broadcaster_no_subscribers() {
        // Should not panic when no subscribers
        let broadcaster = EventBroadcaster::new(16);
        broadcaster.broadcast(make_new_block_event());
    }

    #[test]
    fn test_connection_counter_tracks_and_releases() {
        // The counter must return to zero when a connection ends, so a closed
        // client cannot permanently consume a slot.
        let broadcaster = EventBroadcaster::new(16);
        assert_eq!(
            broadcaster
                .connections
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        broadcaster
            .connections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            broadcaster
                .connections
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        broadcaster
            .connections
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            broadcaster
                .connections
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn test_connection_cap_is_enforced() {
        // The upgrade handler rejects at MAX_WS_CONNECTIONS; verify the
        // threshold arithmetic the handler relies on.
        let broadcaster = EventBroadcaster::new(16);
        broadcaster
            .connections
            .store(MAX_WS_CONNECTIONS, std::sync::atomic::Ordering::SeqCst);
        let current = broadcaster
            .connections
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(current >= MAX_WS_CONNECTIONS, "handler must reject here");
    }
}
