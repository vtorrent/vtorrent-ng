# Reindex / Rescan — Design

Status: DRAFT (no consensus change; store + CLI/RPC)
Scope: `vtorrent-store`, `vtorrent-cli`, `vtorrent-rpc`, `vtorrent-daemon`.
Motivation: the store has the machinery to rebuild derived state
(`clear_derived_state` `store.rs:787`, `truncate_above` `:768`,
`rebuild_from_blocks` `:608`) but there is **no operator-facing command** to
reindex or rescan. When derived state is suspected corrupt (a wrong UTXO root, a
bad tx index, a wallet that can't find its txs), recovery means writing code.
This exposes safe, bounded reindex/rescan operations.

## 1. What exists

- `clear_derived_state` (`store.rs:787`) — drop derived tables.
- `truncate_above(keep)` (`:768`) — roll the store back to a height.
- `rebuild_from_blocks` (`:608`) — replay a block list to rebuild state.
- The daemon's event-bridge reconciliation rebuilds a lagging store.
- No CLI/RPC command surfaces these.

## 2. Design

### 2.1 Reindex (rebuild derived state from blocks)

- `vtorrent-cli reindex [--from-height H] [--to-height H]`: replay the stored
  blocks and rebuild the derived state (UTXO set, tx index, headers, commitment).
- **Bounded**: `--from-height` limits the work; default rebuilds from a recent
  checkpoint (not genesis) unless `--full` is given.
- **Safe**: stop the daemon (or take the store lock) first; the operation is
  idempotent and verifies the resulting tip/UTXO root.

### 2.2 Rescan (wallet)

- `vtorrent-cli rescan [--from-height H]`: rescan the chain for wallet-relevant
  txs (rebuild the wallet's tx history / balances). Needed after an import or an
  HD gap-limit scan (`docs/hd-discovery-design.md`).
- Distinct from reindex: rescan is **wallet-scoped**; reindex is **chain-scoped**.

### 2.3 RPC

- `POST /api/v1/admin/reindex` and `/admin/rescan` — **admin-scoped** (not the
  read token), require the wallet unlocked + explicit confirmation, and run
  **off the request thread** with progress reporting.
- Refuse while the node is syncing or if another reindex is running.

### 2.4 Safety

- **Backup first**: recommend/require a store backup before a full reindex.
- **Progress + resumability**: report progress; a reindex interrupted mid-way
  leaves the store in a known (truncated) state that can be resumed.
- **Verify after**: recompute the UTXO root and compare to the tip header; fail
  loudly on mismatch.

## 3. Adversarial review

- **R1 — reindex is destructive to derived state.** It drops and rebuilds; a bug
  can corrupt the store. Require a backup, run on a stopped/locked store, and
  verify the result. Never run concurrently with the daemon writing.
- **R2 — full reindex is expensive.** Replaying from genesis is slow; default to
  a recent checkpoint and make `--full` explicit (same cost class as
  `load_into_chain`).
- **R3 — rescan ≠ reindex.** Keep them distinct; a rescan must not touch chain
  state, and a reindex must not touch wallet keys.
- **R4 — admin auth.** Reindex/rescan are privileged; require the admin scope +
  confirmation, not the read token (`docs/mobile-companion-design.md` scopes).
- **R5 — concurrency.** Refuse if the node is syncing or another operation is
  running; take the store lock.
- **R6 — no consensus change.** Store/CLI/RPC only; the chain rules are unchanged.

## 4. Test plan

- Reindex from a checkpoint rebuilds the UTXO set + tx index; the tip and
  `utxo_root` match the pre-reindex values.
- Full reindex from genesis reproduces the tip.
- Rescan rebuilds wallet history/balances without touching chain state.
- Interrupted reindex leaves a resumable state; resuming completes correctly.
- Refuses while syncing or if another reindex runs; requires admin auth.
- No consensus/chain change.

## 5. Non-goals

- Not a general database migration tool.
- Not automatic (operator-initiated).
- Not part of the current soak window.

## 6. References

- `vtorrent-store/src/store.rs:608,768,787` rebuild/truncate/clear.
- `vtorrent-cli/src/main.rs` (command surface).
- `vtorrent-daemon/src/main.rs` (reconciliation).
- `docs/node-diagnostics-design.md` (detect the need), `docs/hd-discovery-design.md`
  (rescan), `docs/mobile-companion-design.md` (scopes).
