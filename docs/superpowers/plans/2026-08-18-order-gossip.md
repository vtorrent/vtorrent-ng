# P2P Order Gossip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the DEX order book networked by propagating public order announcements between nodes via a new `dexorder` P2P message with flooding and dedup.

**Architecture:** Add a serializable `OrderAnnouncement` type (no preimage) to `vtorrent-node`. Give the `Node` a shared `order_book` handle and a `seen_orders` dedup set. Add a `dexorder` arm to `handle_message` that deserializes, adds to the book, and re-broadcasts. The daemon shares one `SwapOrderBook` between the node and `AppState`.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`, `tokio`.

**Spec:** `docs/superpowers/specs/2026-08-18-order-gossip-design.md`

---

## File Structure

**Modified:**
- `vtorrent-node/src/atomic_swap.rs` — add `OrderAnnouncement` type
- `vtorrent-node/src/node.rs` — add `order_book` + `seen_orders` fields, `dexorder` handler, `broadcast_order`
- `vtorrent-daemon/src/main.rs` — share the order book between node and AppState

---

## Task 1: Add `OrderAnnouncement` type

**Files:**
- Modify: `vtorrent-node/src/atomic_swap.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `atomic_swap.rs`:

```rust
    #[test]
    fn test_order_announcement_roundtrip() {
        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            DEFAULT_HTLC_LOCKTIME,
        );
        let ann = OrderAnnouncement::from_order(&order);
        let json = serde_json::to_string(&ann).unwrap();
        let back: OrderAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(back.order_id, ann.order_id);
        assert_eq!(back.maker_address, ann.maker_address);
        assert_eq!(back.vtr_amount, ann.vtr_amount);
        assert_eq!(back.target_asset, ann.target_asset);
        assert_eq!(back.target_amount, ann.target_amount);
        assert_eq!(back.expiry, ann.expiry);
    }

    #[test]
    fn test_order_announcement_excludes_preimage() {
        let mut order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            DEFAULT_HTLC_LOCKTIME,
        );
        order.preimage = Some([7u8; 32]);
        order.funding_txid = Some([9u8; 32]);
        let ann = OrderAnnouncement::from_order(&order);
        let json = serde_json::to_string(&ann).unwrap();
        // The preimage and funding txid must never appear in the wire form.
        assert!(!json.contains("preimage"));
        assert!(!json.contains("funding_txid"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-node test_order_announcement`
Expected: FAIL (compile error — `OrderAnnouncement` not defined).

- [ ] **Step 3: Implement `OrderAnnouncement`**

Add to `vtorrent-node/src/atomic_swap.rs` (after the `SwapOrder` struct, before `OrderStatus`):

```rust
/// A public, serializable view of a swap order for P2P gossip.
///
/// Excludes the secret preimage and the private funding txid, which must never
/// leave the maker's node.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrderAnnouncement {
    pub order_id: [u8; 32],
    pub maker_address: String,
    pub vtr_amount: u64,
    pub target_asset: String,
    pub target_amount: u64,
    pub hash_lock: Option<[u8; 32]>,
    pub expiry: u32,
}

impl OrderAnnouncement {
    /// Build a public announcement from a full order.
    pub fn from_order(order: &SwapOrder) -> Self {
        Self {
            order_id: order.order_id,
            maker_address: order.maker_address.clone(),
            vtr_amount: order.vtr_amount,
            target_asset: order.target_asset.clone(),
            target_amount: order.target_amount,
            hash_lock: order.hash_lock,
            expiry: order.expiry,
        }
    }

    /// Reconstruct a `SwapOrder` from an announcement (no preimage/funding).
    pub fn to_order(&self) -> SwapOrder {
        SwapOrder {
            order_id: self.order_id,
            maker_address: self.maker_address.clone(),
            vtr_amount: self.vtr_amount,
            target_asset: self.target_asset.clone(),
            target_amount: self.target_amount,
            hash_lock: self.hash_lock,
            funding_txid: None,
            preimage: None,
            expiry: self.expiry,
            status: OrderStatus::Open,
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-node test_order_announcement`
Expected: PASS.

- [ ] **Step 5: Run full node tests and commit**

Run: `cargo test -p vtorrent-node 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-node/src/atomic_swap.rs
git commit -m "feat: add OrderAnnouncement type for P2P order gossip"
```

---

## Task 2: Add order-book handle and dedup set to the Node

**Files:**
- Modify: `vtorrent-node/src/node.rs`

- [ ] **Step 1: Add the fields**

In `vtorrent-node/src/node.rs`, add to the `Node` struct (after `peer_msg_counts`):

```rust
    /// Shared DEX order book (set by the daemon; used for gossip).
    order_book: Option<Arc<RwLock<SwapOrderBook>>>,
    /// Order IDs already seen via gossip, for deduplication.
    seen_orders: HashSet<[u8; 32]>,
```

Add the import at the top of the file (near the other `vtorrent_node` imports):

```rust
use crate::atomic_swap::{OrderAnnouncement, SwapOrderBook};
```

Also update the `tokio::sync` import to include `RwLock` (currently `use tokio::sync::{mpsc, Mutex};`):

```rust
use tokio::sync::{mpsc, Mutex, RwLock};
```

- [ ] **Step 2: Initialize the fields in both constructors**

In `Node::new` and `Node::new_with_chain`, add to the `Ok(Self { ... })` literal (after `peer_msg_counts`):

```rust
            order_book: None,
            seen_orders: HashSet::new(),
```

- [ ] **Step 3: Add a setter**

Add a method to `impl Node` (near `set_event_sender`):

```rust
    /// Attach the shared DEX order book so the node can gossip orders.
    pub fn set_order_book(&mut self, order_book: Arc<RwLock<SwapOrderBook>>) {
        self.order_book = Some(order_book);
    }
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-node 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-node/src/node.rs
git commit -m "feat: add order-book handle and dedup set to node"
```

---

## Task 3: Add the `dexorder` message handler and broadcast

**Files:**
- Modify: `vtorrent-node/src/node.rs`

- [ ] **Step 1: Add the `dexorder` arm to `handle_message`**

In `vtorrent-node/src/node.rs`, in `handle_message`, add a new arm before the `cmd =>` catch-all (after the `"headers"` arm):

```rust
            // ── DEX order gossip ─────────────────────────────────────────────
            "dexorder" => {
                if let Ok(ann) = serde_json::from_slice::<OrderAnnouncement>(&msg.payload) {
                    let order_id = ann.order_id;
                    if self.seen_orders.insert(order_id) {
                        if let Some(book) = &self.order_book {
                            book.write().await.add_order(ann.to_order());
                        }
                        // Re-broadcast to all peers except the sender.
                        let payload = serde_json::to_vec(&ann).unwrap_or_default();
                        for peer in self.peer_manager.connected_peers() {
                            if peer != peer_addr {
                                self.peer_manager
                                    .send_to(peer, NetMessage::new("dexorder", payload.clone()))
                                    .await;
                            }
                        }
                        tracing::debug!("DEX gossip: received order {}", hex::encode(order_id));
                    }
                } else {
                    self.peer_manager
                        .record_misbehaviour(peer_addr, Misbehaviour::MalformedMessage);
                }
            }
```

- [ ] **Step 2: Add a `broadcast_order` method**

Add to `impl Node` (after `set_order_book`):

```rust
    /// Broadcast a new order announcement to all connected peers.
    pub async fn broadcast_order(&mut self, order: &SwapOrder) {
        let ann = OrderAnnouncement::from_order(order);
        self.seen_orders.insert(order.order_id);
        let payload = serde_json::to_vec(&ann).unwrap_or_default();
        self.peer_manager
            .broadcast(NetMessage::new("dexorder", payload))
            .await;
    }
```

- [ ] **Step 3: Write the failing test**

Add to the `mod tests` block in `node.rs`:

```rust
    #[tokio::test]
    async fn test_dexorder_gossip_adds_to_book() {
        use crate::atomic_swap::{OrderAnnouncement, SwapOrder, SwapOrderBook};
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let mut node = test_node();
        let book = Arc::new(RwLock::new(SwapOrderBook::new()));
        node.set_order_book(book.clone());

        let order = SwapOrder::new(
            "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            1_000_000_000,
            "BTC".to_string(),
            100_000,
            48 * 3600,
        );
        let ann = OrderAnnouncement::from_order(&order);
        let payload = serde_json::to_vec(&ann).unwrap();
        let peer_addr: std::net::SocketAddr = "127.0.0.1:12347".parse().unwrap();

        node.handle_message(peer_addr, NetMessage::new("dexorder", payload))
            .await
            .unwrap();

        assert_eq!(book.read().await.open_order_count(), 1);
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-node test_dexorder_gossip_adds_to_book`
Expected: PASS.

- [ ] **Step 5: Run full node tests and commit**

Run: `cargo test -p vtorrent-node 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-node/src/node.rs
git commit -m "feat: add dexorder gossip handler and broadcast to node"
```

---

## Task 4: Share the order book between node and AppState

**Files:**
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Create a shared order book and wire it into the node**

In `vtorrent-daemon/src/main.rs`, after building `rpc_state` (around line 193), create a shared order book and attach it to the node. Replace the `AppState::new_with_shared` call and add the wiring:

```rust
    let chain_arc = node.chain_arc();
    let mempool_arc = node.mempool_arc();
    let tx_submit_sender = node.tx_submit_sender();
    let mut rpc_state = AppState::new_with_shared(chain_arc, mempool_arc);
    // Wire the tx broadcast channel so RPC wallet can push txs into the P2P loop.
    rpc_state.tx_submit = Some(tx_submit_sender);
    rpc_state.rpc_api_key = cli.rpc_api_key.clone();
    let rpc_addr = cli.rpc_addr.clone();

    // Share the DEX order book between the node (for gossip) and RPC (for the
    // order-book API), so received orders are visible to both.
    node.set_order_book(Arc::clone(&rpc_state.order_book));
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/src/main.rs
git commit -m "feat: share DEX order book between node and RPC"
```

---

## Final Verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features 2>&1 | rg "test result: FAILED|error\["`
Expected: no failures.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | rg "warning:|error:"`
Expected: no output.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 3: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of order gossip"
```
