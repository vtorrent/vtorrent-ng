# BTC Wallet Reorg Undo Journal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the BTC wallet rewind chain-derived state to a reorg fork height instead of rescanning the whole retained window.

**Architecture:** A sparse per-height undo journal lives inside `UtxoSet` and is serialized with it, so one save commits set, cursor, and journal together. The scan records, per touched height, the outputs it added and the outputs it removed along with each removed outpoint's prior reservation state. On a reorg the wallet replays the journal backwards to the fork and resumes scanning; if the journal does not cover the fork, the existing full-rescan / fail-closed behaviour is unchanged.

**Tech Stack:** Rust, `serde`/`serde_json` for wallet persistence, existing `vtorrent-btc` test harness (simulated Bitcoin peers in `sync/swap_tests.rs`).

**Spec:** `docs/superpowers/specs/2026-11-09-btc-undo-journal-design.md`

## Global Constraints

- Scope is **live-process reorgs only**. `reset_chain_cache*` clears the journal, so a restart never replays stale entries.
- No change to bincode encoding, `txid`, block `hash`, `sighash`, genesis, the chain store, or any wire format.
- Journal, cursor, and `last_scanned_tip` persist in the **same file and the same save**.
- Because the journal is embedded, "corrupt journal" and "corrupt UTXO file" are the same event: a file that fails to parse is handled by the existing load-error path, and a file that parses but whose entries are unordered or gapped is treated as not covering. Missing journal fields are legal (`serde(default)`) so pre-change files load and simply take the full-reset path.
- Journal entries are **sparse**: only heights that touched the wallet.
- `BtcWalletHistoryUnavailable` fires only when `birthday < floor`; a covered reorg must not fire it.
- Balance stays hidden from rollback until the post-fork scan completes.

## Review Focus

- A reorg whose abandoned branch **spent** a wallet output: the output must return with its value, address, and height, not just its outpoint.
- A swap funding input spent on the abandoned branch: it must return to **reserved**, never to selectable.
- A journal that is unordered, gapped, or unparsable: discarded whole, never partially applied.
- A reorg deeper than journal coverage with `birthday >= floor`: lossless full rescan, and no alert.
- A crash between blocks: journal and cursor must never disagree, because both are saved together.

---

### Task 1: Undo journal data model in `UtxoSet`

**Files:**
- Modify: `vtorrent-btc/src/utxo.rs` (struct `UtxoSet`, new types, new methods, tests module)
- Test: `vtorrent-btc/src/utxo.rs` (`mod tests`)

**Interfaces:**
- Consumes: existing `Utxo`, `UtxoSet::{add, remove, reserve_utxo, reserved_utxo, note_spend, was_reserved_spent}`.
- Produces:
  - `pub enum PriorState { Free, Reserved(Utxo), SpentReserved }`
  - `pub struct RemovedOutput { pub utxo: Utxo, pub prior: PriorState }`
  - `pub struct UndoEntry { pub height: u32, pub created: Vec<Utxo>, pub removed: Vec<RemovedOutput> }`
  - `UtxoSet::last_scanned_tip(&self) -> Option<[u8; 32]>`
  - `UtxoSet::set_last_scanned_tip(&mut self, tip: Option<[u8; 32]>)`
  - `UtxoSet::push_undo(&mut self, entry: UndoEntry)`
  - `UtxoSet::undo_entries(&self) -> &[UndoEntry]`
  - `UtxoSet::clear_undo(&mut self)`
  - `UtxoSet::prune_undo(&mut self, floor: u32, max_entries: usize)`
  - `UtxoSet::undo_above(&mut self, fork: u32) -> bool` — applies and returns coverage
  - `UtxoSet::outpoint(&self, txid: &str, vout: u32) -> Option<Utxo>`

- [ ] **Step 1: Write the failing tests**

In `mod tests` add, using the existing `utxo(txid, vout, value)` helper:

```rust
#[test]
fn undo_entry_restores_outputs_removed_above_the_fork() {
    let mut set = UtxoSet::new();
    set.add(Utxo { txid: "aa".into(), vout: 0, value: 500, address: "bc1q".into(), height: 5 });
    set.push_undo(UndoEntry {
        height: 6,
        created: vec![Utxo { txid: "bb".into(), vout: 0, value: 700, address: "bc1q".into(), height: 6 }],
        removed: vec![RemovedOutput { utxo: utxo("aa", 0, 500), prior: PriorState::Free }],
    });
    set.add(Utxo { txid: "bb".into(), vout: 0, value: 700, address: "bc1q".into(), height: 6 });
    set.remove("aa", 0);

    assert!(set.undo_above(5));
    let ids: Vec<_> = set.list().iter().map(|u| u.txid.clone()).collect();
    assert_eq!(ids, vec!["aa".to_string()], "created above fork dropped, removed restored");
    assert_eq!(set.list()[0].value, 500);
    assert_eq!(set.list()[0].height, 5);
}

#[test]
fn undo_restores_reserved_state_instead_of_freeing_the_input() {
    let mut set = UtxoSet::new();
    let reserved = utxo("cc", 0, 900);
    set.push_undo(UndoEntry {
        height: 7,
        created: vec![],
        removed: vec![RemovedOutput { utxo: reserved.clone(), prior: PriorState::Reserved(reserved.clone()) }],
    });
    set.note_spend("cc", 0);

    assert!(set.undo_above(6));
    assert!(set.reserved_utxo("cc", 0).is_some(), "input returns reserved, not selectable");
    assert!(!set.list().iter().any(|u| u.txid == "cc"));
}

#[test]
fn undo_above_reports_no_coverage_when_the_fork_is_below_the_journal() {
    let mut set = UtxoSet::new();
    set.push_undo(UndoEntry { height: 10, created: vec![], removed: vec![] });
    assert!(!set.undo_above(5));
    assert_eq!(set.undo_entries().len(), 1, "no partial application");
}

#[test]
fn undo_above_reports_no_coverage_for_gapped_or_unordered_journal() {
    let mut set = UtxoSet::new();
    set.push_undo(UndoEntry { height: 12, created: vec![], removed: vec![] });
    set.push_undo(UndoEntry { height: 10, created: vec![], removed: vec![] });
    assert!(!set.undo_above(9));
}

#[test]
fn prune_undo_drops_below_floor_and_enforces_cap() {
    let mut set = UtxoSet::new();
    for height in 1..=5u32 {
        set.push_undo(UndoEntry { height, created: vec![], removed: vec![] });
    }
    set.prune_undo(3, 10);
    assert_eq!(set.undo_entries().first().unwrap().height, 3);
    set.prune_undo(0, 2);
    assert_eq!(set.undo_entries().len(), 2);
    assert_eq!(set.undo_entries().last().unwrap().height, 5);
}

#[test]
fn journal_and_tip_round_trip_through_persistence() {
    let path = std::env::temp_dir().join(format!("btc-undo-rt-{}.json", std::process::id()));
    let mut set = UtxoSet::new();
    set.set_last_scanned_tip(Some([7u8; 32]));
    set.push_undo(UndoEntry { height: 4, created: vec![], removed: vec![] });
    set.save(&path).unwrap();

    let loaded = UtxoSet::load(&path).unwrap();
    assert_eq!(loaded.last_scanned_tip(), Some([7u8; 32]));
    assert_eq!(loaded.undo_entries().len(), 1);
    std::fs::remove_file(path).unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vtorrent-btc --lib utxo::tests::undo -- --nocapture`
Expected: compile error — `UndoEntry`, `PriorState`, `RemovedOutput`, and the new methods do not exist.

- [ ] **Step 3: Implement the types and methods in `vtorrent-btc/src/utxo.rs`**

Add `#[serde(default)] last_scanned_tip: Option<[u8; 32]>` and `#[serde(default)] undo: Vec<UndoEntry>` to `UtxoSet`. `undo_above` walks entries in reverse: reject unless entries are strictly ascending and every height is `> fork` (coverage requires the lowest entry height `<= fork + 1`); otherwise return `false` before mutating anything. When applying, insert `removed` outputs directly into `utxos` (bypassing `add`'s reserved filter), then restore `PriorState`: `Free` pushes the record into `utxos` only, `Reserved(u)` calls `reserve_utxo(u)`, `SpentReserved` calls `note_spend`. Drop `created`. Finally truncate `undo` to entries `<= fork` and clear `last_scanned_tip`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vtorrent-btc --lib utxo::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-btc/src/utxo.rs
git commit -m "feat(btc): add an undo journal to the wallet UTXO set"
```

---

### Task 2: Record undo entries during scans

**Files:**
- Modify: `vtorrent-btc/src/sync.rs` (`record_matching_outputs`, ~line 469)
- Test: `vtorrent-btc/src/sync/swap_tests.rs`

**Interfaces:**
- Consumes: `UtxoSet::{push_undo, outpoint, undo_entries, reserved_utxo, was_reserved_spent}`, `UndoEntry`, `RemovedOutput`, `PriorState` from Task 1.
- Produces: no new public API; a side effect on the shared `UtxoSet`.

- [ ] **Step 1: Write the failing test**

Append to `sync/swap_tests.rs`, reusing `chain`, `serve`, `contract`:

```rust
#[tokio::test]
async fn scanning_records_a_sparse_undo_entry_for_touched_heights() {
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let (blocks, _) = chain(&contract(), "wallet_unspent");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(serve(listener, blocks, "wallet_valid"));
    let mut peer = BtcPeer::connect_with_network(addr, bitcoin::Network::Regtest).await.unwrap();
    wallet.sync(&mut peer).await.unwrap();
    wallet.scan_utxos_bip158_multi(std::slice::from_mut(&mut peer), 0).await.unwrap();

    let entries = wallet.undo_entries_for_tests();
    assert!(!entries.is_empty(), "the funding block must be journalled");
    assert!(entries.iter().all(|e| e.height == 1), "only touched heights are stored");
    assert_eq!(entries[0].created.len(), 1);
    drop(peer);
    server.await.unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtorrent-btc --lib scanning_records_a_sparse_undo_entry_for_touched_heights`
Expected: FAIL — `undo_entries_for_tests` does not exist.

- [ ] **Step 3: Implement recording**

In `record_matching_outputs`, before each `utxos.note_spend(...)`, capture the prior state:

```rust
let prior = if let Some(reserved) = utxos.reserved_utxo(&prev_txid, prev_vout) {
    PriorState::Reserved(reserved.clone())
} else if utxos.was_reserved_spent(&prev_txid, prev_vout) {
    PriorState::SpentReserved
} else {
    PriorState::Free
};
let removed = utxos.outpoint(&prev_txid, prev_vout).map(|utxo| RemovedOutput { utxo, prior });
```

Accumulate this function's `created` and `removed` into the entry for `height` via `push_undo`, merging with an existing entry at the same height (find the last entry; if its height equals `height`, extend it, else push a new one). Only push when either vector is non-empty. Add `pub(crate) fn undo_entries_for_tests(&self) -> Vec<UndoEntry>` on `BtcWallet` returning a clone of the journal.

- [ ] **Step 4: Prune the journal after each scan**

In `vtorrent-btc/src/wallet.rs`, after a scan completes and the cursor is saved, call `utxos.prune_undo(self.retained_floor(), MAX_UNDO_ENTRIES)` where `MAX_UNDO_ENTRIES: usize = 200_000` is a module constant. The cap is independent of `--btc-header-retention`, so unbounded headers never imply an unbounded journal.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p vtorrent-btc --lib scanning_records_a_sparse_undo_entry_for_touched_heights`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-btc/src/sync.rs vtorrent-btc/src/sync/swap_tests.rs vtorrent-btc/src/wallet.rs
git commit -m "feat(btc): journal outputs added and removed while scanning"
```

---

### Task 3: Persist the scanned tip alongside the cursor

**Files:**
- Modify: `vtorrent-btc/src/wallet.rs` (`set_last_scanned_height`, ~line 655)
- Modify: `vtorrent-rpc/src/btc_sync.rs` (call site ~line 277)
- Test: `vtorrent-btc/src/wallet.rs` (`mod tests`)

**Interfaces:**
- Consumes: `UtxoSet::set_last_scanned_tip`, `BtcWallet::best_height()`.
- Produces: `pub fn set_scan_progress(&mut self, next_height: u32, tip: Option<[u8; 32]>)`; `pub fn last_scanned_tip(&self) -> Option<[u8; 32]>`; `pub fn best_hash(&self) -> Option<[u8; 32]>` (delegates to `headers.best_hash()`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn scan_progress_persists_the_tip_with_the_cursor() {
    let path = std::env::temp_dir().join(format!("btc-progress-{}.json", std::process::id()));
    let mut w = BtcWallet::with_persistence([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
    w.set_scan_progress(42, Some([9u8; 32]));

    let loaded = BtcWallet::with_persistence([7u8; 64], bitcoin::Network::Regtest, path.clone()).unwrap();
    assert_eq!(loaded.last_scanned_height(), 42);
    assert_eq!(loaded.last_scanned_tip(), Some([9u8; 32]));
    std::fs::remove_file(path).unwrap();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p vtorrent-btc --lib scan_progress_persists_the_tip_with_the_cursor`
Expected: FAIL — `set_scan_progress` does not exist.

- [ ] **Step 3: Implement**

`set_scan_progress` sets `self.last_scanned_height`, delegates the existing completion/persistence logic to `set_last_scanned_height`, then writes `set_last_scanned_tip(tip)` into the same `UtxoSet` and saves once. Update `vtorrent-rpc/src/btc_sync.rs` to capture `let tip = wallet.best_hash();` before the scan and call `wallet.set_scan_progress(done, tip)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p vtorrent-btc --lib scan_progress_persists_the_tip_with_the_cursor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-btc/src/wallet.rs vtorrent-rpc/src/btc_sync.rs
git commit -m "feat(btc): persist the scanned tip with the cursor"
```

---

### Task 4: Use the journal on a reorg, keep the old path otherwise

**Files:**
- Modify: `vtorrent-btc/src/wallet.rs` (reorg branch in `sync`, ~line 560; `reset_chain_cache_from`, ~line 173)
- Test: `vtorrent-btc/src/sync/swap_tests.rs`

**Interfaces:**
- Consumes: `UtxoSet::undo_above`, `UtxoSet::clear_undo`, `reset_chain_cache_from(Some(fork))`.
- Produces: no new public API.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn a_covered_reorg_replays_the_journal_instead_of_rescanning_everything() {
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let (blocks, _) = chain(&contract(), "wallet_unspent");
    wallet.add_header(&serialize(&blocks[0].header), 0).unwrap();
    for (height, block) in blocks.iter().enumerate().skip(1) {
        wallet.add_header(&serialize(&block.header), height as u32).unwrap();
    }
    // Seed a journalled entry so the fork is covered.
    wallet.push_undo_for_tests(UndoEntry { height: 6, created: vec![], removed: vec![] });

    let reorged = wallet.reset_for_reorg_for_tests(6);
    assert!(reorged, "a covered fork must take the journal path");
    assert_eq!(wallet.last_scanned_height(), 7);
    assert_eq!(wallet.undo_entries_for_tests().len(), 0);
}

#[tokio::test]
async fn a_covered_reorg_restores_an_output_spent_on_the_abandoned_branch() {
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let owned = Utxo {
        txid: "ab".repeat(32),
        vout: 0,
        value: 100_000,
        address: contract().recipient,
        height: 5,
    };
    wallet.add_utxo(owned.clone());
    wallet.push_undo_for_tests(UndoEntry {
        height: 6,
        created: vec![],
        removed: vec![RemovedOutput { utxo: owned.clone(), prior: PriorState::Free }],
    });
    wallet.remove_utxo_for_tests(&owned.txid, owned.vout);

    assert!(wallet.reset_for_reorg_for_tests(5), "fork at 5 is covered");
    let restored = wallet.utxo_for_tests(&owned.txid, owned.vout).expect("output must return");
    assert_eq!(restored.value, 100_000);
    assert_eq!(restored.height, 5);
}

#[tokio::test]
async fn an_uncovered_reorg_falls_back_to_the_full_reset() {
    let mut wallet = crate::wallet::BtcWallet::with_network([1; 64], bitcoin::Network::Regtest);
    let reorged = wallet.reset_for_reorg_for_tests(6);
    assert!(!reorged, "no journal means the existing full-reset path");
    assert_eq!(wallet.last_scanned_height(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vtorrent-btc --lib reorg_`
Expected: FAIL — the test helpers do not exist.

- [ ] **Step 3: Implement**

In the reorg branch replace the unconditional `reset_chain_cache_from(Some(fork))` with:

```rust
if !self.reset_chain_cache_from_journal(fork) {
    self.reset_chain_cache_from(Some(fork));
}
```

`reset_chain_cache_from_journal(fork) -> bool` takes the UTXO lock, calls `undo_above(fork)`; on `true` it sets `last_scanned_height = fork + 1`, clears `completed_scan`, clears filter headers, sets `rescan_on_sync` (balance hidden) but **not** `rescan_required`, saves, and returns `true`. On `false` it returns `false` without mutating. `reset_chain_cache_from(None)` must call `utxos.clear_undo()` and `set_last_scanned_tip(None)`. Add `pub(crate)` test helpers `push_undo_for_tests`, `reset_for_reorg_for_tests`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vtorrent-btc --lib reorg_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-btc/src/wallet.rs vtorrent-btc/src/sync/swap_tests.rs
git commit -m "feat(btc): replay the undo journal on a covered reorg"
```

---

### Task 5: Preserve the existing guarantees

**Files:**
- Test: `vtorrent-btc/src/wallet.rs` (`mod tests`), `vtorrent-btc/src/sync/swap_tests.rs`
- Modify: `vtorrent-btc/src/wallet.rs` only if a test exposes a gap.

**Interfaces:**
- Consumes: everything from Tasks 1–4.
- Produces: no new API.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn clearing_the_chain_cache_clears_the_journal() {
    let mut w = BtcWallet::new([7u8; 64]);
    w.push_undo_for_tests(UndoEntry { height: 3, created: vec![], removed: vec![] });
    w.clear_chain_cache_for_tests();
    assert!(w.undo_entries_for_tests().is_empty(), "restart path must not replay stale entries");
    assert_eq!(w.last_scanned_tip(), None);
}

#[test]
fn a_covered_reorg_does_not_raise_the_history_alert() {
    let mut w = BtcWallet::new([7u8; 64]);
    w.push_undo_for_tests(UndoEntry { height: 3, created: vec![], removed: vec![] });
    assert!(w.reset_for_reorg_for_tests(3));
    assert!(!w.history_unavailable(), "a covered reorg is recoverable, not an operator incident");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vtorrent-btc --lib clearing_the_chain_cache_clears_the_journal a_covered_reorg_does_not_raise_the_history_alert`
Expected: FAIL — helpers missing.

- [ ] **Step 3: Implement any gap the tests expose**

Expected gaps: `clear_chain_cache_for_tests` helper, and confirming `history_unavailable` is false because `rescan_required` is untouched on the journal path. Do not weaken `require_retained_wallet_history`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vtorrent-btc --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-btc/src/wallet.rs
git commit -m "test(btc): pin journal reset and alert behaviour"
```

---

### Task 6: Document and verify the whole change

**Files:**
- Modify: `docs/mainnet-readiness.md` (§2 BTC retained-history note)
- Modify: `docs/soak-log.md` (new dated entry)
- Test: full workspace suite

- [ ] **Step 1: Update the readiness note**

State that a reorg covered by the journal is recovered in seconds with balance hidden until the scan completes, that an uncovered reorg with `birthday >= floor` still rescans losslessly, and that only `birthday < floor` alerts. Keep the existing scope sentence: live-process only, restart behaviour unchanged.

- [ ] **Step 2: Add the soak-log entry**

Record: the design, the four review findings that shaped it (restart wipes cache, atomicity, coverage semantics, reservation fidelity), and the test matrix outcome.

- [ ] **Step 3: Run the full suite**

Run: `VTORRENT_TOTP_STATE_DIR=$(mktemp -d) cargo test --workspace --all-features`
Expected: 0 failed; note the group/pass/ignored counts in the log entry.

- [ ] **Step 4: Run lints and formatting**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add docs/mainnet-readiness.md docs/soak-log.md docs/superpowers/specs/2026-11-09-btc-undo-journal-design.md docs/superpowers/plans/2026-11-09-btc-undo-journal.md
git commit -m "docs: record the undo journal and its verification"
```

---

## Execution Notes

- The journal changes the BTC reorg path, so per the provenance policy it needs fleet evidence before it can be part of a release commit: unit and harness tests first, then a candidate fleet where a reorg is induced or observed.
- Do not push or deploy as part of these tasks; committing locally is the deliverable.
