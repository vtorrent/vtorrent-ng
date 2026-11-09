# BTC Wallet Reorg Undo Journal — Design

**Date:** 2026-11-09
**Status:** Approved — scope limited to live-process reorgs
**Author:** vTorrent Dev
**Related:** `vtorrent-btc/src/sync.rs:469`, `vtorrent-btc/src/wallet.rs` (reorg path, `require_retained_wallet_history`), `vtorrent-btc/src/utxo.rs`, `docs/mainnet-readiness.md` §2

## 1. Overview

The BTC wallet's chain state is a fold over blocks: each scanned transaction
removes spent outputs and adds created ones (`record_matching_outputs`,
`sync.rs:469-496`). A reorg currently discards the whole fold and rebuilds it
from the retained header floor, because there is no way to undo just the
invalidated blocks. On a reorg deeper than the wallet's birthday the rebuild is
impossible, so the wallet fails closed and an operator must raise retention.

This design adds a **per-block undo journal** so the fold can be rewound to the
fork height, making reorg recovery seconds instead of a 100,000-block rescan and
removing the routine need for operator intervention — without ever silently
dropping a possible output.

## 2. Goals / Non-Goals

**Goals**

- Rewind chain-derived wallet state to an arbitrary fork height within the
  journal's coverage, then continue scanning from the fork.
- Scope is **live-process reorgs only**: the journal is discarded whenever the
  wallet clears its chain cache (which happens on every restart), so startup
  recovery is unchanged.
- Remove the operator step for reorgs that the journal covers.
- Keep the existing guarantee: a wallet that may hold funds below what it can
  reconstruct stays blocked, with its balance hidden and the critical alert up.
- Change nothing on the wire, in consensus hashing, or in the chain store.

**Non-Goals**

- No change to bincode encoding, `txid`/`hash`/`sighash`, or genesis.
- No new recovery of history the node never retained.
- No attempt to reconstruct spend history below the wallet's birthday.
- No cross-device sync of journal or UTXO state.
- No persisted header lineage, and no change to restart behaviour. A journal that
  would let a restart skip its full rescan is explicitly out of scope: that needs
  its own lineage-persistence design.

**Anti-goals** (would make this a failure even if it works)

- A silent fallback to a full rescan that could hide an output created before
  the floor.
- Journal growth unbounded with chain height.
- Any path where a corrupt journal produces a *smaller* balance without an alert.

## 3. Constraints

- Wallet-local data only; no consensus or peer-visible change.
- Must survive restart and must be pruned in step with header retention, or it
  becomes a second unbounded store.
- Must be validated against the real reorg tests
  (`vtorrent-btc/src/sync/swap_tests.rs`) as well as unit tests.
- Must not weaken `require_retained_wallet_history`; it may only be reached less
  often.

## 4. Approaches Considered

**A. Per-block undo journal (chosen).** For each scanned height, store the
wallet-relevant outputs created and removed. Rollback replays the journal
backwards from the tip to the fork. Small data, second-scale recovery, reuses
the existing fold.

**B. Periodic snapshots.** Full UTXO copy every N blocks; roll back to the
nearest snapshot and rescan the remainder. Simpler, but larger files and still
up to N blocks of rescan.

**C. Per-height commitments.** A UTXO set or hash per height. Precise and
auditable, storage-heavy, substantially more code.

A wins: least data, fastest recovery, and no second source of truth.

## 5. The Reorg Rule

On detecting a reorg with fork height `F`, and with `floor` the retained header
floor and `birthday` the wallet's recorded first height:

| Case | Condition | Action |
|---|---|---|
| 1 | Journal covers `F` | Replay undo back to `F`, rescan `F+1..tip`, balance hidden until the scan completes |
| 2 | Journal does not cover `F`, and `birthday >= floor` | Full rescan from `floor` (provably lossless: nothing below the floor predates the wallet) |
| 3 | `birthday < floor` | Fail closed: hide balances, keep `rescan_required`, raise `BtcWalletHistoryUnavailable` |

Case 3 is the only alerting case, so the alert keeps a single meaning: *this
wallet may hold funds you cannot currently see*.

Case 1 hides the balance by setting `rescan_on_sync` but deliberately does **not**
set `rescan_required`, so it clears when the post-fork scan completes and the
critical alert stays case-3-only. The wallet is never spendable mid-rollback.

## 6. Journal Format

Stored **inside** the UTXO set file, so one rename commits set and journal
together. A journal that is ahead of the persisted set would replay `removed`
outputs that were never removed on disk — a phantom balance, which is an
anti-goal. No separate file, no second commit point.

```
UtxoSet {
  ...
  last_scanned_tip: Option<[u8; 32]>,   // hash of the tip the cursor reached
  undo: Vec<UndoEntry>,                 // ascending, sparse, capped
}
UndoEntry {
  height: u32,
  created: Vec<Utxo>,                       // outputs this block added
  removed: Vec<RemovedOutput>,              // full record + the state to restore
}
RemovedOutput {
  utxo: Utxo,                               // txid, vout, value, address, height
  prior: PriorState,                        // Free | Reserved(Utxo) | SpentReserved
}
```

Using the full `Utxo` is what makes rollback exact: restoring an output needs its
value, address, and height, not just its outpoint. Storing the prior reservation
state is what keeps swap inputs correct — an input the abandoned branch spent
returns to *reserved* (not selectable for a second swap) rather than to free.

Entries are **sparse**: only heights that touched the wallet are stored, because
an untouched block is a no-op for the fold. Coverage is therefore a height range
plus internal ordering, not one entry per block.

**Rollback** applies entries from the highest covered height down to `F+1` in
reverse: re-add `removed` (inserting directly, bypassing `add`'s reserved-outpoint
filter), drop `created`, and restore each outpoint's prior reservation state.
`last_scanned_height` becomes `F + 1` and `last_scanned_tip` is cleared.

**Pruning.** Entries below `floor` are dropped whenever the header chain prunes,
and entries are capped by count as a safety valve so
`--btc-header-retention 0` cannot produce an unbounded journal. If pruning
removes a range a later reorg needs, case 2 or 3 applies — the rule degrades,
never lies.

**Corruption.** A journal that fails to parse, is not ascending, or has a gap in
its recorded range is discarded as a whole and treated as not-covering; the
wallet then takes case 2 or 3. It is never partially applied. `last_scanned_tip`
is used to detect a lineage mismatch on load, not to judge coverage.

## 7. Interaction With Existing Behaviour

- `require_retained_wallet_history` keeps its current semantics; the journal
  only makes case 1 and case 2 avoid it.
- `seen_funds` continues to gate birthday adoption.
- `BtcWalletHistoryUnavailable` and `vtorrent_btc_wallet_history_blocked` are
  unchanged; they now fire only in case 3.
- Journal, cursor, and `last_scanned_tip` live in one file and one save, so a
  crash cannot leave them inconsistent.
- The journal is owned by `BtcWallet`. Only the main scan path appends entries;
  swap verification and observation build `BtcSync` over a scratch `UtxoSet` and
  must never journal. The scan takes an optional journal sink and the scratch
  call sites pass none.
- `reset_chain_cache` clears the journal with the rest of the chain state, which
  is what makes the live-process-only scope explicit in code.

## 8. Test Matrix

Unit (`vtorrent-btc`):

1. Append and replay a single block; state matches pre-block.
2. Replay N blocks backwards; intermediate states match a forward rebuild.
3. Restoring a removed output re-adds value, address, and height.
4. Reservation and spent-reservation bookkeeping survive a rollback.
5. Pruned journal + `birthday >= floor` takes case 2 and rescans losslessly.
6. Pruned journal + `birthday < floor` takes case 3, hides balance, sets the
   flag, and persists `rescan_required`.
7. Corrupt, unordered, or gapped journal is discarded whole; no partial application.
8. `reset_chain_cache` clears the journal, so a restart cannot replay stale entries.
9. Journal and cursor persist in one save; a crash between blocks never leaves the
   journal ahead of the set.

Integration (`sync/swap_tests.rs`, existing harness):

10. Reorg one block below the tip rolls back via journal and re-derives the same
    UTXO set as a full rescan.
11. Reorg plus a spent-on-abandoned-branch output: the output returns via the
    journal (this is the case the earlier fork-only rollback got wrong).
12. A swap funding input spent on the abandoned branch returns to *reserved*, not
    selectable, after rollback.
13. Balance is hidden from rollback until the post-fork scan completes, and
    `BtcWalletHistoryUnavailable` does not fire for a covered reorg.

Alert: `promtool` test already covers case 3 firing and resolving.

## 9. Rollout

Wallet-local, no wire or consensus change, so it does not require a coordinated
upgrade. It does change the BTC reorg path, so per the provenance policy it
needs its own fleet evidence before it can be part of a release commit: unit and
harness tests first, then a candidate fleet where a reorg is induced or observed.
Because scope is live-process only, restart behaviour is unchanged and does not
need re-soaking; the reorg path itself does.

## 10. Open Items For The Builder

- Exact cap on journal entries (count vs bytes) and its interaction with
  `--btc-header-retention 0` (unbounded headers must not imply unbounded
  journal).
- Whether to expose journal coverage in a metric for operators.
