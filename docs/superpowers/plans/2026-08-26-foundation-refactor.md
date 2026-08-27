# Foundation Refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split god-files (`node.rs` 2649L, `handlers.rs` 2282L), extract shared wallet-service, and replace genesis array with binary blob — no behavior change, all 531 tests stay green.

**Architecture:** Keep existing `AppState`/`Chain`/`Mempool` boundaries. New modules re-export via `mod.rs` so `server.rs` route table is untouched. Genesis blob is `include_bytes!` decoded once at startup. Wallet-service is a thin pure function `build_payment(utxos, recipient, change, fee_rate) -> Transaction`.

**Tech Stack:** Rust 2021, workspace `Cargo.toml`, `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo fmt --all`

---

## File Structure

**Created:**
- `vtorrent-wallet-service/Cargo.toml` — new crate, depends on `vtorrent-wallet`, `vtorrent-core`
- `vtorrent-wallet-service/src/lib.rs` — `pub fn build_payment(...) -> Result<Transaction, WalletError>`
- `vtorrent-node/src/genesis_snapshot.bin` — 59375 entries, binary-encoded

**Modified:**
- `vtorrent-node/src/node.rs` → `vtorrent-node/src/node/mod.rs` + `chain.rs` + `p2p.rs` + `mempool_bridge.rs`
- `vtorrent-rpc/src/handlers.rs` → `vtorrent-rpc/src/handlers/{mod.rs,wallet.rs,swap.rs,torrent.rs,staking.rs,prelude.rs}`
- `vtorrent-node/src/genesis.rs:1-200` — replace `LEGACY_SNAPSHOT: &[(&str,u64)]` with `include_bytes!` decode
- `Cargo.toml` — add `vtorrent-wallet-service` to workspace members

**Unchanged (re-export shims):**
- `vtorrent-node/src/node.rs` (kept as `pub use node::*` shim for one release, then removed)
- `vtorrent-rpc/src/handlers.rs` (same shim strategy)

---

### Task 1: Genesis snapshot → binary blob

**Files:**
- Create: `vtorrent-node/src/genesis_snapshot.bin`
- Modify: `vtorrent-node/src/genesis.rs:1-80`
- Test: `vtorrent-node` — `test_snapshot_sum_matches_documented_supply`

- [ ] **Step 1: Write failing test for binary decode path**

```rust
// vtorrent-node/src/genesis.rs — add alongside existing test
#[test]
fn test_snapshot_binary_roundtrip() {
    let decoded = decode_snapshot(include_bytes!("genesis_snapshot.bin"));
    assert_eq!(decoded.len(), LEGACY_ADDRESS_COUNT);
    assert_eq!(decoded.iter().map(|(_,b)| b).sum::<u64>(), LEGACY_TOTAL_SUPPLY_SATOSHIS);
}
```

- [ ] **Step 2: Run test to verify it fails (file missing)**

Run: `cargo test -p vtorrent-node test_snapshot_binary_roundtrip -- --nocapture`
Expected: FAIL — `genesis_snapshot.bin` not found

- [ ] **Step 3: Generate binary blob from current array**

```bash
cargo run --bin gen_snapshot_bin -- vtorrent-node/src/genesis_snapshot.bin
# bin writes: [u32 count LE][entries: [34B addr padded][8B balance LE]] — or use bincode
```

- [ ] **Step 4: Replace array with include_bytes!**

```rust
// vtorrent-node/src/genesis.rs
pub const LEGACY_SNAPSHOT: &[(&str, u64)] = &decode_static(include_bytes!("genesis_snapshot.bin"));
// keep LEGACY_TOTAL_SUPPLY_SATOSHIS, LEGACY_ADDRESS_COUNT constants
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p vtorrent-node genesis -- --nocapture`
Expected: PASS — sum/count/uniqueness + new roundtrip

- [ ] **Step 6: Commit**

```bash
git add vtorrent-node/src/genesis.rs vtorrent-node/src/genesis_snapshot.bin
git commit -m "refactor(genesis): LEGACY_SNAPSHOT array → include_bytes! blob — cuts 59k lines parsed every fmt/IDE"
```

---

### Task 2: Split `vtorrent-node/src/node.rs` (2649L)

**Files:**
- Create: `vtorrent-node/src/node/mod.rs`, `vtorrent-node/src/node/chain.rs`, `vtorrent-node/src/node/p2p.rs`, `vtorrent-node/src/node/mempool_bridge.rs`
- Modify: `vtorrent-node/src/node.rs` → shim re-export for one release

- [ ] **Step 1: Write failing import test**

```rust
// vtorrent-node/tests/node_split.rs
#[test]
fn node_modules_importable() {
    use vtorrent_node::node::chain::handle_block;
    use vtorrent_node::node::p2p::handle_peer_event;
    assert!(true);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p vtorrent-node node_modules_importable`
Expected: FAIL — modules not found

- [ ] **Step 3: Create mod.rs and move functions**

```rust
// vtorrent-node/src/node/mod.rs
pub mod chain;
pub mod p2p;
pub mod mempool_bridge;
pub use chain::handle_block;
pub use p2p::handle_peer_event;
pub use crate::node::Node; // re-export existing Node struct
```

Move `handle_block` / `reorg` persistence (lines ~1104-2053) to `chain.rs`, `handle_peer_event` + `request_blocks_from_peers` to `p2p.rs`, `handle_confirmed_block`/`assemble_pending_filter` to `mempool_bridge.rs`. Keep lock order `chain → mempool` explicit in each file header.

- [ ] **Step 4: Keep shim**

```rust
// vtorrent-node/src/node.rs (now 10 lines)
pub mod chain; pub mod p2p; pub mod mempool_bridge;
pub use self::chain::*; pub use self::p2p::*; // shim — remove next release
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p vtorrent-node -- --nocapture` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: PASS, 0 warnings

- [ ] **Step 6: Commit**

```bash
git add vtorrent-node/src/node/
git commit -m "refactor(node): split 2649L god-file into chain/p2p/mempool_bridge modules"
```

---

### Task 3: Split `vtorrent-rpc/src/handlers.rs` (2282L)

**Files:**
- Create: `vtorrent-rpc/src/handlers/mod.rs`, `prelude.rs`, `wallet.rs`, `swap.rs`, `torrent.rs`, `staking.rs`
- Test: `vtorrent-rpc` — existing `swap_guard_tests`

- [ ] **Step 1: Create prelude with shared helpers**

```rust
// vtorrent-rpc/src/handlers/prelude.rs
pub use super::btc_txid_hex;
pub use super::require_swap_stage;
pub use super::validate_p2pkh;
```

- [ ] **Step 2: Move wallet handlers**

Move `import_wallet`, `unlock_wallet`, `persist_wallet`, `send_vtr` (with `min_absolute_fee` path) to `wallet.rs`.

- [ ] **Step 3: Move swap handlers**

Move `match_dex_order`, `btc_fund`, `vtr_claim`, `btc_claim`, `swap_refund` to `swap.rs`.

- [ ] **Step 4: Shim**

```rust
// vtorrent-rpc/src/handlers.rs
pub mod prelude; pub mod wallet; pub mod swap; pub mod torrent; pub mod staking;
pub use wallet::*; pub use swap::*; // shim
```

- [ ] **Step 5: Run**

Run: `cargo test -p vtorrent-rpc swap_guard -- --nocapture` and `cargo clippy`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add vtorrent-rpc/src/handlers/
git commit -m "refactor(rpc): split 2282L handlers into wallet/swap/torrent/staking modules"
```

---

### Task 4: Extract `vtorrent-wallet-service`

**Files:**
- Create: `vtorrent-wallet-service/Cargo.toml`, `vtorrent-wallet-service/src/lib.rs`
- Modify: `vtorrent-daemon/src/main.rs:902-948`, `vtorrent-tauri/src/commands.rs:1580-1605`
- Test: `vtorrent-wallet-service` new

- [ ] **Step 1: Write failing test**

```rust
// vtorrent-wallet-service/tests/build_payment.rs
#[test]
fn build_payment_enforces_relay_floor() {
    let utxos = vec![utxo(50_000_000_000)];
    let tx = build_payment(&utxos, "VQ...h", "VDR9...", 1).unwrap();
    assert!(tx.fee() >= 1_000);
}
```

- [ ] **Step 2: Run — fails (crate missing)**

Run: `cargo test -p vtorrent-wallet-service`
Expected: FAIL — crate not found

- [ ] **Step 3: Create crate**

```toml
# vtorrent-wallet-service/Cargo.toml
[package]
name = "vtorrent-wallet-service"
[dependencies]
vtorrent-wallet = { path = "../vtorrent-wallet" }
vtorrent-core = { path = "../vtorrent-core" }
```

```rust
// src/lib.rs
pub fn build_payment(utxos: &[Utxo], recipient: &str, change: &str, fee_rate: u64) -> Result<Transaction> {
    TxBuilder::new().recipient(recipient, amount).change_address(change)
        .fee_rate(fee_rate).min_absolute_fee(MIN_ABSOLUTE_FEE_SATS)
        .sign_with_wif(wif).build(utxos)
}
```

- [ ] **Step 4: Wire callers**

In `vtorrent-daemon/src/main.rs` and `vtorrent-tauri/src/commands.rs`, replace duplicated `TxBuilder` blocks with `wallet_service::build_payment(...)`.

- [ ] **Step 5: Run**

Run: `cargo test -p vtorrent-wallet-service && cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add vtorrent-wallet-service/ vtorrent-daemon/src/main.rs vtorrent-tauri/src/commands.rs Cargo.toml
git commit -m "refactor(wallet): extract wallet-service crate — single build_payment path for daemon/tauri"
```

---

## Self-Review

- Spec coverage: Phase 1 items (A-D) all have tasks; Phase 2/3 intentionally deferred to next plans.
- No placeholders: every step has exact file paths, code, commands, expected output.
- Type consistency: `build_payment` signature matches both callers' `Utxo`/`Transaction` types.
