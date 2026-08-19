# Incentive Payment System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the VTR torrent incentive system pay out: exchange VTR addresses via a BEP-10 `ut_vtr` extension, record bandwidth, and emit payment events the daemon turns into real VTR transactions.

**Architecture:** Add a `ut_vtr` extension (advertised in the extension handshake, carrying a VTR address string). Wire `record_download`/`record_upload` into `run_peer_task` keyed by the peer's VTR address. Add a `PaymentDue` event type and a channel the torrent crate emits on settlement; the daemon consumes it and reuses the existing `send_vtr` flow.

**Tech Stack:** Rust (edition 2021), `tokio`, `serde_bencode`.

**Spec:** `docs/superpowers/specs/2026-08-19-incentive-payment-design.md`

---

## File Structure

**New:**
- `vtorrent-torrent/src/payment.rs` — `PaymentDue` event type + `PaymentSender` channel

**Modified:**
- `vtorrent-torrent/src/metadata.rs` — add `ut_vtr` handshake + address message helpers
- `vtorrent-torrent/src/engine.rs` — exchange addresses, record bandwidth, emit payments
- `vtorrent-torrent/src/lib.rs` — export `payment`
- `vtorrent-daemon/src/main.rs` — consume payment events and build VTR txs

---

## Task 1: `ut_vtr` address message helpers

**Files:**
- Modify: `vtorrent-torrent/src/metadata.rs`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `metadata.rs`:

```rust
    #[test]
    fn test_ut_vtr_handshake_advertises() {
        let bytes = build_ut_vtr_handshake(2);
        let value: Value = serde_bencode::from_bytes(&bytes).unwrap();
        if let Value::Dict(d) = value {
            if let Some(Value::Dict(m)) = d.get(b"m".as_slice()) {
                assert_eq!(m.get(b"ut_vtr".as_slice()), Some(&Value::Int(2)));
            } else {
                panic!("missing m dict");
            }
        } else {
            panic!("expected dict");
        }
    }

    #[test]
    fn test_ut_vtr_address_message_roundtrip() {
        let addr = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        let msg = build_ut_vtr_address(2, addr);
        assert_eq!(msg[0], 2); // extension id prefix
        let parsed = parse_ut_vtr_address(&msg[1..]).unwrap();
        assert_eq!(parsed, addr);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-torrent test_ut_vtr`
Expected: FAIL (compile error — `build_ut_vtr_handshake`/`build_ut_vtr_address`/`parse_ut_vtr_address` not defined).

- [ ] **Step 3: Implement the helpers**

Add to `metadata.rs` (after `build_extension_handshake`):

```rust
/// Build an extension handshake advertising `ut_vtr` at the given id.
pub fn build_ut_vtr_handshake(ut_vtr_id: u8) -> Vec<u8> {
    let mut m = std::collections::HashMap::new();
    m.insert(b"ut_vtr".to_vec(), Value::Int(ut_vtr_id as i64));
    let mut dict = std::collections::HashMap::new();
    dict.insert(b"m".to_vec(), Value::Dict(m));
    serde_bencode::to_bytes(&Value::Dict(dict)).unwrap_or_default()
}

/// Build a `ut_vtr` address message: `<ut_vtr_id><bencoded string>`.
pub fn build_ut_vtr_address(ut_vtr_id: u8, address: &str) -> Vec<u8> {
    let payload = serde_bencode::to_bytes(&Value::Bytes(address.as_bytes().to_vec()))
        .unwrap_or_default();
    let mut out = vec![ut_vtr_id];
    out.extend_from_slice(&payload);
    out
}

/// Parse a `ut_vtr` address message payload (the bencoded string after the id).
pub fn parse_ut_vtr_address(payload: &[u8]) -> Result<String> {
    let value: Value = serde_bencode::from_bytes(payload)
        .map_err(|e| TorrentError::PeerWireError(e.to_string()))?;
    match value {
        Value::Bytes(b) => Ok(String::from_utf8_lossy(&b).into_owned()),
        _ => Err(TorrentError::PeerWireError("ut_vtr not a string".into())),
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent test_ut_vtr`
Expected: PASS.

- [ ] **Step 5: Run full metadata tests and commit**

Run: `cargo test -p vtorrent-torrent metadata::`
Expected: all pass.

```bash
git add vtorrent-torrent/src/metadata.rs
git commit -m "feat: add ut_vtr address message helpers"
```

---

## Task 2: `PaymentDue` event type and channel

**Files:**
- Create: `vtorrent-torrent/src/payment.rs`
- Modify: `vtorrent-torrent/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-torrent/src/payment.rs`:

```rust
//! Incentive payment events emitted by the torrent engine.

use serde::{Deserialize, Serialize};

/// A payment that is due to a peer for bandwidth exchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentDue {
    /// The peer's VTR address.
    pub peer_address: String,
    /// Amount owed in satoshis.
    pub amount_satoshis: u64,
}

/// A channel for emitting payment events to the daemon.
#[derive(Clone)]
pub struct PaymentSender {
    tx: tokio::sync::mpsc::UnboundedSender<PaymentDue>,
}

impl PaymentSender {
    /// Create a new payment channel, returning (sender, receiver).
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<PaymentDue>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Emit a payment event (non-blocking; drops if the receiver is gone).
    pub fn emit(&self, payment: PaymentDue) {
        let _ = self.tx.send(payment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_payment_channel_roundtrip() {
        let (sender, mut receiver) = PaymentSender::channel();
        sender.emit(PaymentDue {
            peer_address: "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string(),
            amount_satoshis: 50_000_000,
        });
        let payment = receiver.recv().await.unwrap();
        assert_eq!(payment.peer_address, "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        assert_eq!(payment.amount_satoshis, 50_000_000);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-torrent payment::`
Expected: 1 test passes.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-torrent/src/lib.rs`, add `pub mod payment;` after `pub mod metadata;`.

```bash
git add vtorrent-torrent/src/payment.rs vtorrent-torrent/src/lib.rs
git commit -m "feat: add PaymentDue event type and channel"
```

---

## Task 3: Wire address exchange and bandwidth recording into the engine

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`

- [ ] **Step 1: Add a `payment_sender` to `PeerTaskContext`**

In `engine.rs`, add a field to `PeerTaskContext`:

```rust
struct PeerTaskContext {
    metainfo: Metainfo,
    peer_id: [u8; 20],
    scheduler: Arc<StdMutex<SchedulerState>>,
    download_dir: PathBuf,
    sessions: Arc<RwLock<SessionManager>>,
    session_id: String,
    cancel: CancellationToken,
}
```

> **Note:** The engine does not emit payment events directly. Bandwidth is recorded into the session's `incentive_accounts` (Step 3), and the daemon's settlement loop (Task 4) reads those accounts and emits `PaymentDue` events. This keeps the torrent crate decoupled from the wallet.

- [ ] **Step 2: Exchange `ut_vtr` addresses after connect**

In `run_peer_task`, after the handshake and before the download loop, add address exchange. Insert after the `let _ = conn.send(&PeerMessage::Interested).await;` line:

```rust
    // Exchange VTR addresses via the ut_vtr extension (BEP-10).
    let mut peer_vtr_address: Option<String> = None;
    {
        let handshake = crate::metadata::build_ut_vtr_handshake(1);
        let _ = conn
            .send(&PeerMessage::Extended {
                id: 0,
                payload: handshake,
            })
            .await;
        // Read the peer's extension handshake to learn its ut_vtr id.
        let mut ut_vtr_id = None;
        for _ in 0..10 {
            match conn.recv().await {
                Ok(PeerMessage::Extended { id: 0, payload }) => {
                    if let Ok(Value::Dict(d)) = serde_bencode::from_bytes::<Value>(&payload) {
                        if let Some(Value::Dict(m)) = d.get(b"m".as_slice()) {
                            if let Some(Value::Int(id)) = m.get(b"ut_vtr".as_slice()) {
                                ut_vtr_id = Some(*id as u8);
                            }
                        }
                    }
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        if let Some(id) = ut_vtr_id {
            // Send our address, then read the peer's address.
            let our_addr = {
                let guard = sessions.read().await;
                guard
                    .get_session(&session_id)
                    .map(|s| s.wallet_address.clone())
                    .unwrap_or_default()
            };
            let _ = conn
                .send(&PeerMessage::Extended {
                    id,
                    payload: crate::metadata::build_ut_vtr_address(id, &our_addr),
                })
                .await;
            for _ in 0..10 {
                match conn.recv().await {
                    Ok(PeerMessage::Extended { id: rid, payload }) if rid == id => {
                        if let Ok(addr) = crate::metadata::parse_ut_vtr_address(&payload) {
                            peer_vtr_address = Some(addr);
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        }
    }
```

- [ ] **Step 3: Record bandwidth on piece receipt**

In `run_peer_task`, in the `Ok(PeerMessage::Piece { index, begin, data })` arm, after updating `s.bytes_downloaded`, add bandwidth recording. Insert after the `s.bytes_downloaded = ...` line:

```rust
                                // Record bandwidth for incentive accounting.
                                let peer_key = peer_vtr_address
                                    .clone()
                                    .unwrap_or_else(|| conn.remote_peer_id.iter().map(|b| format!("{:02x}", b)).collect());
                                s.record_download(&peer_key, piece_data.len() as u64);
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-torrent 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-torrent/src/engine.rs
git commit -m "feat: exchange VTR addresses and record bandwidth in the engine"
```

---

## Task 4: Emit payment events on settlement

**Files:**
- Modify: `vtorrent-torrent/src/engine.rs`
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Emit payments in the daemon settlement loop**

In `vtorrent-daemon/src/main.rs`, in the incentive settlement task, after `account.settle(now)`, emit a payment event. Replace the settlement loop body:

```rust
            let mut guard = torrent_sessions_for_settlement.write().await;
            let mut settled = 0;
            for session in guard.sessions_mut() {
                for account in session.incentive_accounts.values_mut() {
                    if account.needs_settlement(now) {
                        let (earned, owed) = account.settle(now);
                        let _ = earned;
                        if owed > 0 && !account.peer_address.is_empty() {
                            // Emit a payment event for the owed amount.
                            let _ = payment_sender.emit(vtorrent_torrent::payment::PaymentDue {
                                peer_address: account.peer_address.clone(),
                                amount_satoshis: owed,
                            });
                        }
                        settled += 1;
                    }
                }
            }
```

- [ ] **Step 2: Create the payment channel and consume it**

In `vtorrent-daemon/src/main.rs`, before the settlement task, create the channel and a consumer task. Add before the settlement `tokio::spawn`:

```rust
    // Payment channel: the torrent engine emits PaymentDue events; this task
    // builds and broadcasts the actual VTR transactions.
    let (payment_sender, mut payment_receiver) = vtorrent_torrent::payment::PaymentSender::channel();
    let payment_rpc_state = Arc::clone(&rpc_state);
    tokio::spawn(async move {
        while let Some(payment) = payment_receiver.recv().await {
            // Build and broadcast a VTR payment using the wallet.
            let result = build_incentive_payment(&payment_rpc_state, &payment).await;
            match result {
                Ok(txid) => tracing::info!("Incentive payment {} sent to {}", txid, payment.peer_address),
                Err(e) => tracing::warn!("Incentive payment failed: {}", e),
            }
        }
    });
```

- [ ] **Step 3: Add the `build_incentive_payment` helper**

In `vtorrent-daemon/src/main.rs`, add a helper function (near the other free functions):

```rust
/// Build and broadcast a VTR payment for an incentive settlement.
async fn build_incentive_payment(
    state: &Arc<vtorrent_rpc::state::AppState>,
    payment: &vtorrent_torrent::payment::PaymentDue,
) -> anyhow::Result<String> {
    use vtorrent_wallet::tx_builder::TxBuilder;

    let wif = state
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("wallet not unlocked"))?;
    let change_address = state
        .wallet_change_address
        .read()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("change address not set"))?;

    let utxos: Vec<vtorrent_node::chain::Utxo> = {
        let chain = state.chain.lock().await;
        chain.get_utxos_for_address(&change_address)
    };
    if utxos.is_empty() {
        return Err(anyhow::anyhow!("no UTXOs available"));
    }

    let tx = TxBuilder::new()
        .recipient(&payment.peer_address, payment.amount_satoshis)
        .change_address(&change_address)
        .fee_rate(10)
        .sign_with_wif(&wif)
        .build(&utxos)
        .map_err(|e| anyhow::anyhow!("tx build failed: {}", e))?;

    let txid = hex::encode(tx.txid());
    {
        let mut mempool = state.mempool.lock().await;
        mempool
            .add_transaction(tx.clone())
            .map_err(|e| anyhow::anyhow!("mempool rejected: {}", e))?;
    }
    if let Some(ref sender) = state.tx_submit {
        let _ = sender.try_send(tx);
    }
    Ok(txid)
}
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/src/main.rs
git commit -m "feat: emit and settle incentive payments"
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
git commit -m "chore: final verification of incentive payments"
```
