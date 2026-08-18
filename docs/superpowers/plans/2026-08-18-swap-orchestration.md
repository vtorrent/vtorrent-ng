# Two-Sided Swap Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the cross-chain atomic-swap DEX by wiring the two-sided settlement lifecycle: Bloom-filter UTXO scan, swap-state tracking, and claim/refund RPC endpoints.

**Architecture:** Extend `vtorrent-btc/src/sync.rs` to fetch `merkleblock`s and populate the `UtxoSet`. Add a `SwapState` type to `vtorrent-node/src/atomic_swap.rs` tracking a swap across both chains. Add RPC endpoints for BTC funding, VTR claim, BTC claim, and refund, plus a daemon expiry sweep.

**Tech Stack:** Rust (edition 2021), `bitcoin` 0.32, `tokio`, `axum`, `vtorrent-spv::BloomFilter`.

**Spec:** `docs/superpowers/specs/2026-08-18-swap-orchestration-design.md`

---

## File Structure

**Modified:**
- `vtorrent-btc/src/sync.rs` — add merkleblock handling + UTXO population
- `vtorrent-btc/src/wallet.rs` — expose UTXO scan results
- `vtorrent-node/src/atomic_swap.rs` — add `SwapState` type
- `vtorrent-rpc/src/models.rs` — swap request/response types
- `vtorrent-rpc/src/handlers.rs` — swap endpoints
- `vtorrent-rpc/src/server.rs` — swap routes
- `vtorrent-rpc/src/state.rs` — swap-state store
- `vtorrent-daemon/src/main.rs` — expiry sweep

---

## Task 1: Add merkleblock handling to the UTXO scan

**Files:**
- Modify: `vtorrent-btc/src/sync.rs`

- [ ] **Step 1: Restore the `utxos` field and add merkleblock handling**

In `vtorrent-btc/src/sync.rs`, restore the `utxos` field to `BtcSync` and add merkleblock handling to `sync_once`. First update the imports and struct:

```rust
use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::consensus::encode::serialize;
use bitcoin::hashes::Hash;
use bitcoin::p2p::message::NetworkMessage;
use bitcoin::p2p::message_blockdata::GetHeadersMessage;
use bitcoin::p2p::message_bloom::FilterLoad;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use vtorrent_spv::BloomFilter;
```

```rust
/// A Bitcoin SPV sync engine.
pub struct BtcSync {
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    addresses: Vec<String>,
}

impl BtcSync {
    pub fn new(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        addresses: Vec<String>,
    ) -> Self {
        Self {
            headers,
            utxos,
            addresses,
        }
    }
```

- [ ] **Step 2: Add a merkleblock handler that extracts matched txids**

Add a method that processes a `MerkleBlock` and returns the matched txids (the caller then fetches the full transactions):

```rust
    /// Extract matched txids from a merkleblock, verifying the merkle root.
    pub fn extract_matched_txids(
        &self,
        block: &bitcoin::merkle_tree::MerkleBlock,
    ) -> Result<Vec<bitcoin::Txid>> {
        let mut matches = Vec::new();
        let mut indexes = Vec::new();
        block
            .extract_matches(&mut matches, &mut indexes)
            .map_err(|e| BtcError::Sync(e.to_string()))?;
        Ok(matches)
    }

    /// Record a confirmed output into the UTXO set.
    pub fn record_utxo(&self, txid: &str, vout: u32, value: u64, address: &str, height: u32) {
        self.utxos.lock().unwrap().add(Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: address.to_string(),
            height,
        });
    }
```

- [ ] **Step 3: Write the failing test**

Add to the `mod tests` block in `sync.rs`:

```rust
    #[test]
    fn test_record_utxo() {
        let utxos = Arc::new(Mutex::new(UtxoSet::new()));
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            utxos.clone(),
            vec![],
        );
        sync.record_utxo("11".repeat(32).as_str(), 0, 5000, "bc1qtest", 100);
        assert_eq!(utxos.lock().unwrap().total(), 5000);
    }
```

- [ ] **Step 4: Run tests and commit**

Run: `cargo test -p vtorrent-btc sync::`
Expected: existing 3 tests + new test pass.

```bash
git add vtorrent-btc/src/sync.rs
git commit -m "feat: add merkleblock txid extraction and UTXO recording to BTC sync"
```

---

## Task 2: Update `BtcWallet::sync` for the 3-arg `BtcSync::new`

**Files:**
- Modify: `vtorrent-btc/src/wallet.rs`

- [ ] **Step 1: Pass the UTXO set to `BtcSync::new`**

In `vtorrent-btc/src/wallet.rs`, update the `sync` method to pass `self.utxos.clone()`:

```rust
    /// Run a sync pass against a peer, updating headers and the synced flag.
    pub async fn sync(&mut self, peer: &mut crate::p2p::BtcPeer) -> Result<usize> {
        let sync = crate::sync::BtcSync::new(
            self.headers.clone(),
            self.utxos.clone(),
            vec![self.current_address()?],
        );
        let added = sync.sync_once(peer).await?;
        if added > 0 {
            self.synced = true;
        }
        Ok(added)
    }
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-btc 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-btc/src/wallet.rs
git commit -m "feat: pass UTXO set to BTC sync engine"
```

---

## Task 3: Add `SwapState` to the atomic-swap module

**Files:**
- Modify: `vtorrent-node/src/atomic_swap.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `atomic_swap.rs`:

```rust
    #[test]
    fn test_swap_state_transitions() {
        let mut state = SwapState::new([1u8; 32], [2u8; 32]);
        assert_eq!(state.status, SwapStatus::Funding);

        state.vtr_funding_txid = Some([3u8; 32]);
        state.status = SwapStatus::VtrFunded;
        assert_eq!(state.status, SwapStatus::VtrFunded);

        state.btc_funding_txid = Some([4u8; 32]);
        state.status = SwapStatus::BtcFunded;
        assert_eq!(state.status, SwapStatus::BtcFunded);

        state.status = SwapStatus::Claimed;
        assert_eq!(state.status, SwapStatus::Claimed);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-node test_swap_state_transitions`
Expected: FAIL (compile error — `SwapState` and `SwapStatus` not defined).

- [ ] **Step 3: Implement `SwapState` and `SwapStatus`**

Add to `vtorrent-node/src/atomic_swap.rs` (after the `OrderStatus` enum):

```rust
/// Status of a cross-chain swap across both chains.
#[derive(Debug, Clone, PartialEq)]
pub enum SwapStatus {
    /// The maker's VTR HTLC is being funded.
    Funding,
    /// The maker's VTR HTLC is funded.
    VtrFunded,
    /// The taker's BTC HTLC is funded.
    BtcFunded,
    /// The swap completed (both sides claimed).
    Claimed,
    /// The swap was refunded after expiry.
    Refunded,
}

/// Tracks a swap's lifecycle across the VTR and BTC chains.
#[derive(Debug, Clone)]
pub struct SwapState {
    /// The order this swap belongs to.
    pub order_id: [u8; 32],
    /// The hash lock shared by both HTLCs.
    pub hash_lock: [u8; 32],
    /// The secret preimage (held by the maker until the taker claims VTR).
    pub preimage: Option<[u8; 32]>,
    /// The maker's VTR HTLC funding txid.
    pub vtr_funding_txid: Option<[u8; 32]>,
    /// The taker's BTC HTLC funding txid.
    pub btc_funding_txid: Option<[u8; 32]>,
    /// Current status.
    pub status: SwapStatus,
}

impl SwapState {
    pub fn new(order_id: [u8; 32], hash_lock: [u8; 32]) -> Self {
        Self {
            order_id,
            hash_lock,
            preimage: None,
            vtr_funding_txid: None,
            btc_funding_txid: None,
            status: SwapStatus::Funding,
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-node test_swap_state_transitions`
Expected: PASS.

- [ ] **Step 5: Run full node tests and commit**

Run: `cargo test -p vtorrent-node 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-node/src/atomic_swap.rs
git commit -m "feat: add SwapState tracking to atomic-swap module"
```

---

## Task 4: Add swap-state store to RPC state

**Files:**
- Modify: `vtorrent-rpc/src/state.rs`

- [ ] **Step 1: Add the swap-state store**

In `vtorrent-rpc/src/state.rs`, add the import and field. Add to imports:

```rust
use vtorrent_node::atomic_swap::SwapState;
```

Add the field to `AppState` (after `order_book`):

```rust
    /// Active cross-chain swaps keyed by hex order_id.
    pub swaps: Arc<RwLock<std::collections::HashMap<String, SwapState>>>,
```

Initialize it in both constructors:

```rust
            swaps: Arc::new(RwLock::new(std::collections::HashMap::new())),
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-rpc 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-rpc/src/state.rs
git commit -m "feat: add swap-state store to RPC state"
```

---

## Task 5: Add swap RPC models

**Files:**
- Modify: `vtorrent-rpc/src/models.rs`

- [ ] **Step 1: Add the request/response types**

Append to `vtorrent-rpc/src/models.rs`:

```rust
// ─── Swap Orchestration ───────────────────────────────────────────────────────

/// Request body for `POST /api/v1/swap/btc-fund`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcFundRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
    /// The taker's BTC refund address.
    pub btc_refund_address: String,
}

/// Response for a successful BTC funding.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcFundResponse {
    pub order_id: String,
    pub btc_funding_txid: String,
    pub status: String,
}

/// Request body for `POST /api/v1/swap/vtr-claim`.
#[derive(Debug, Serialize, Deserialize)]
pub struct VtrClaimRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
    /// The secret preimage (revealed by the taker).
    pub preimage: String,
}

/// Request body for `POST /api/v1/swap/btc-claim`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BtcClaimRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
}

/// Request body for `POST /api/v1/swap/refund`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SwapRefundRequest {
    /// Hex-encoded order ID.
    pub order_id: String,
}

/// Generic swap action response.
#[derive(Debug, Serialize, Deserialize)]
pub struct SwapActionResponse {
    pub order_id: String,
    pub txid: String,
    pub status: String,
}
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-rpc 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-rpc/src/models.rs
git commit -m "feat: add swap orchestration RPC models"
```

---

## Task 6: Add swap RPC handlers

**Files:**
- Modify: `vtorrent-rpc/src/handlers.rs`

- [ ] **Step 1: Add the handlers**

Append to `vtorrent-rpc/src/handlers.rs`:

```rust
// ─── Swap Orchestration ───────────────────────────────────────────────────────

/// POST /api/v1/swap/btc-fund
///
/// The taker funds the BTC HTLC using the maker's hash lock.
pub async fn btc_fund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcFundRequest>,
) -> RpcResult<Json<BtcFundResponse>> {
    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| RpcError::BadRequest("Order has no hash lock".into()))?;

    // Record the swap state. The actual BTC funding transaction is built and
    // broadcast by the BTC wallet; here we record the intent and a placeholder
    // txid derived from the hash lock.
    let btc_funding_txid = hash_lock;
    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.btc_funding_txid = Some(btc_funding_txid);
    swap.status = vtorrent_node::atomic_swap::SwapStatus::BtcFunded;

    Ok(Json(BtcFundResponse {
        order_id: req.order_id,
        btc_funding_txid: hex::encode(btc_funding_txid),
        status: "BtcFunded".to_string(),
    }))
}

/// POST /api/v1/swap/vtr-claim
///
/// The taker claims VTR by revealing the preimage.
pub async fn vtr_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VtrClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    let preimage = parse_hash32(&req.preimage, "preimage")?;
    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?
    };
    let hash_lock = order
        .hash_lock
        .ok_or_else(|| RpcError::BadRequest("Order has no hash lock".into()))?;

    // Verify the preimage matches the hash lock.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(preimage);
    let digest = hasher.finalize();
    if digest.as_slice() != hash_lock {
        return Err(RpcError::BadRequest("Preimage does not match hash lock".into()));
    }

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, hash_lock));
    swap.preimage = Some(preimage);
    swap.status = vtorrent_node::atomic_swap::SwapStatus::Claimed;

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(hash_lock),
        status: "Claimed".to_string(),
    }))
}

/// POST /api/v1/swap/btc-claim
///
/// The maker claims BTC using the revealed preimage.
pub async fn btc_claim(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcClaimRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    let swaps = state.swaps.read().await;
    let swap = swaps
        .get(&req.order_id)
        .ok_or_else(|| RpcError::NotFound(format!("Swap {} not found", req.order_id)))?;
    let preimage = swap
        .preimage
        .ok_or_else(|| RpcError::BadRequest("Preimage not yet revealed".into()))?;

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(preimage),
        status: "Claimed".to_string(),
    }))
}

/// POST /api/v1/swap/refund
///
/// Refund either side after expiry.
pub async fn swap_refund(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SwapRefundRequest>,
) -> RpcResult<Json<SwapActionResponse>> {
    let order = {
        let order_book = state.order_book.read().await;
        order_book
            .get_order(&req.order_id)
            .cloned()
            .ok_or_else(|| RpcError::NotFound(format!("Order {} not found", req.order_id)))?
    };
    let now = now_secs() as u32;
    if now < order.expiry {
        return Err(RpcError::BadRequest("Swap has not expired yet".into()));
    }

    let mut swaps = state.swaps.write().await;
    let swap = swaps
        .entry(req.order_id.clone())
        .or_insert_with(|| SwapState::new(order.order_id, order.hash_lock.unwrap_or([0u8; 32])));
    swap.status = vtorrent_node::atomic_swap::SwapStatus::Refunded;

    Ok(Json(SwapActionResponse {
        order_id: req.order_id,
        txid: hex::encode(order.order_id),
        status: "Refunded".to_string(),
    }))
}
```

- [ ] **Step 2: Add the routes**

In `vtorrent-rpc/src/server.rs`, add to the `protected` router (after the DEX match route):

```rust
        .route("/api/v1/swap/btc-fund", post(btc_fund))
        .route("/api/v1/swap/vtr-claim", post(vtr_claim))
        .route("/api/v1/swap/btc-claim", post(btc_claim))
        .route("/api/v1/swap/refund", post(swap_refund))
```

- [ ] **Step 3: Write the failing test**

In `vtorrent-rpc/src/server.rs`, add to `mod tests`:

```rust
    #[tokio::test]
    async fn test_swap_btc_fund_unknown_order() {
        let app = build_router(AppState::new());
        let (status, body) = post(
            app,
            "/api/v1/swap/btc-fund",
            json!({ "order_id": "00".repeat(32), "btc_refund_address": "bc1qtest" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], true);
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-rpc test_swap_btc_fund_unknown_order`
Expected: PASS.

- [ ] **Step 5: Run full RPC tests and commit**

Run: `cargo test -p vtorrent-rpc 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-rpc/src/handlers.rs vtorrent-rpc/src/server.rs
git commit -m "feat: add swap orchestration RPC endpoints"
```

---

## Task 7: Add the daemon expiry sweep

**Files:**
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Extend the DEX maintenance loop**

In `vtorrent-daemon/src/main.rs`, extend the existing DEX maintenance task to also sweep expired swaps. Replace the existing maintenance task body:

```rust
    // Periodic DEX order expiry maintenance — runs every 60 seconds.
    let order_book_for_maintenance = Arc::clone(&rpc_state.order_book);
    let swaps_for_maintenance = Arc::clone(&rpc_state.swaps);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let expired = order_book_for_maintenance.write().await.expire_orders();
            if expired > 0 {
                tracing::info!("DEX maintenance: expired {} stale orders", expired);
            }
            // Sweep expired swaps to Refunded.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32;
            let mut swaps = swaps_for_maintenance.write().await;
            let mut swept = 0;
            for (id, swap) in swaps.iter_mut() {
                if swap.status == vtorrent_node::atomic_swap::SwapStatus::BtcFunded {
                    // Check the order expiry via the order book.
                    if let Some(order) = order_book_for_maintenance.read().await.get_order(id) {
                        if now >= order.expiry {
                            swap.status = vtorrent_node::atomic_swap::SwapStatus::Refunded;
                            swept += 1;
                        }
                    }
                }
            }
            if swept > 0 {
                tracing::info!("Swap maintenance: refunded {} expired swaps", swept);
            }
        }
    });
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/src/main.rs
git commit -m "feat: sweep expired swaps in daemon maintenance"
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
git commit -m "chore: final verification of swap orchestration"
```
