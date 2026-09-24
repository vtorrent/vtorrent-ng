# In-Memory Block-Body Pruning — Design

Status: DRAFT (docs-only, no fleet deployment)
Scope: `vtorrent-node` `Chain` in-memory representation; store fallback in
`vtorrent-daemon` / `vtorrent-tauri` / `vtorrent-rpc` wiring.
Soak constraint: **no deploy to the soak fleet** until after sign-off
(2026-10-01T06:58:52Z). This doc is reviewable now; code lands post-soak.

## 1. Background

The soak fleet's node1 RSS grows **~477 kB/h, chain-proportional** — over a
70-minute steady window at 59 blocks/h that is **~8 kB per block** with no
sign of a plateau (`docs/memory-observability-design.md` §7.7). The glibc
mmap/trim tunables deployed 2026-09-24 returned the startup-replay transient
(163 → 133 MiB) but did not change the rate; the budget was raised 180 → 220
MiB as a stopgap so the current window can be observed.

Root cause: `Chain` retains **every `Block` body for the life of the process**,
in `Chain::blocks` (`vtorrent-node/src/chain.rs:131`). For a long-running node
this is unbounded and is a **mainnet blocker**. The store already persists every
block (`BlockStore::append_block` / `get_block` / `get_block_at_height`,
`vtorrent-store/src/store.rs:190,305,316`), so the in-memory bodies are a cache,
not the source of truth.

This design bounds that cache: keep recent bodies in memory, keep the full
**index** and every **header**, and fall back to the store for older bodies.

## 2. What the in-memory `Chain` holds per block today

`Chain` (`vtorrent-node/src/chain.rs:129`):

| Field | Grows with | Per-block cost | Needed for consensus? |
|---|---|---|---|
| `blocks: HashMap<[u8;32], Block>` | all blocks + forks | **~0.5–1 kB body + allocator churn** | no (see §3) |
| `height_index: Vec<[u8;32]>` | main chain | 32 B | yes (fork choice) |
| `tx_index: HashMap<[u8;32], ([u8;32], usize)>` | main-chain txs | ~40 B/tx | lookup only |
| `utxo_set: BTreeMap<.., Utxo>` | unspent outputs | ~100 B/UTXO | **yes** |
| `journals: VecDeque<BlockJournal>` | **bounded** `max_reorg_depth=100` | — | yes (rollback) |
| `cumulative_work: HashMap<[u8;32], u64>` | all blocks | ~40 B | yes (fork choice) |
| `parent_map: HashMap<[u8;32], [u8;32]>` | all blocks | ~40 B | yes (reorg path) |
| `block_heights: HashMap<[u8;32], u32>` | all blocks | ~40 B | yes |

`Block` = `BlockHeader` (120 B: version, prev, merkle_root, **utxo_root**,
timestamp, bits, nonce, **stake_modifier**) + `Vec<Transaction>`
(`vtorrent-node/src/block.rs:279,238`).

The observed ~8 kB/block is far above the ~1 kB of body data: each `Block`
carries many small `Vec` allocations (one per tx, per input, per output,
script_sig, script_pubkey), and the allocator overhead + fragmentation on that
churn dominates. **Pruning bodies removes both the data and the allocation
churn** — the rate should improve by more than the naive byte ratio.

## 3. What actually needs full bodies (evidence)

**Consensus reads nothing that requires an old body.**

- Applying a block needs only the **parent's `stake_modifier`** — a header
  field: `apply_block_journaled` does `chain.blocks.get(&prev_block_hash)
  .map(|b| b.header.stake_modifier)` (`chain/chain_reorg.rs:166`). A kept header
  map satisfies this.
- Input validation and rewards use the **`utxo_set`** (`Utxo` carries script +
  value), not block bodies (`chain_reorg.rs:228` `apply_transaction_journaled`).
- Reorg rollback reads `blocks.get(&journal.block_hash)`
  (`chain_reorg.rs:57`) and `reorganize_to` reads fork bodies to apply
  (`chain_reorg.rs:559`). Both are bounded by the journals deque, i.e.
  `max_reorg_depth = 100`. **Any body older than the last 100 blocks is never
  read for consensus.**
- `add_block`'s happy path reads only the adjacent parent body
  (`chain.rs:846`); `add_block`'s fork path reads the new block itself.

**Unbounded-old reads are all RPC/display and already tolerate absence:**

| Caller | Path | Uses |
|---|---|---|
| `get_block(hash)` `chain.rs:401` | explorer `/block/:hash` | full block |
| `get_block_at_height(h)` `chain.rs:406` | explorer, swap recovery, DEX | full block |
| `get_transaction(txid)` `chain.rs:431` | wallet, swap, history | tx body |
| `resolve_output` `chain.rs:714` | `get_recent_transactions`, `tx_fee` | output script/value |
| `genesis_block()` `chain.rs:574` | bootstrap | genesis only |

`resolve_output` callers use `filter_map` / `if let Some` (`chain.rs:660,704`),
so a body miss degrades gracefully (fee/sent shows 0). Genesis must be pinned.

## 4. Design

**Keep, for every known block:** header + the existing index fields. Add
`headers: HashMap<[u8;32], BlockHeader>` (all blocks + forks) so consensus
never needs a body. `height_index`, `parent_map`, `cumulative_work`,
`block_heights`, `tx_index`, `utxo_set`, `journals` are unchanged.

**Keep full bodies** only for the most recent `K` main-chain blocks:
`blocks: HashMap<[u8;32], Block>` with `K = block_body_cache` (default **1000**,
configurable). This is ≥ 10× the 100-block reorg/rollback bound.

**Prune point.** In `Chain::add_block`, after a main-chain block is accepted and
`height_index.push(hash)` (`chain.rs:820`), if
`height_index.len() > K`, take `victim = height_index[len - 1 - K]`, and if
`victim` is not referenced by any journal, remove it from `blocks` — its header
stays in `headers`. Because `replay_range` drives `add_block`
(`store.rs:649`), this also bounds memory during startup replay.

**Lookup fallback.** `Chain` returns a miss (as today) for a pruned body; the
layer that owns the store fills it:

- Define a node-side, object-safe trait (node must not depend on the store —
  `vtorrent-store` already depends on `vtorrent-node`):
  ```rust
  pub trait BlockBodySource: Send + Sync {
      fn block_body(&self, hash: &[u8; 32]) -> Option<Block>;
      fn block_body_at_height(&self, height: u32) -> Option<Block>;
  }
  ```
- Hold `Option<Arc<dyn BlockBodySource>>` in `Chain`, set once at startup by the
  daemon/tauri (which own the `Arc<BlockStore>`); `BlockStore` implements the
  trait. `get_block` / `get_block_at_height` try `blocks`, else the source.
- `get_transaction` / `resolve_output` fall back the same way; `Chain` gains an
  owned-returning variant because the fallback yields `Block` by value (today
  they return `&Transaction` / `&TxOutput`).

Options considered:

- **A — trait on `Chain` (recommended).** One place for all lookups; no store
  dependency inside `vtorrent-node`; wiring is a single
  `chain.set_body_source(store.clone())`. Costs an API tweak (owned vs borrowed).
- **B — fallback at the ~7 RPC call sites.** No `Chain` change, but the same
  fallback is re-implemented across rpc/daemon/tauri and easy to miss.
- **C — small LRU in `AppState`.** Helps only RPC, not swap-recovery-internal
  lookups; rejected.

## 5. Consensus safety

- Headers retained → hash, `prev_block_hash`, `merkle_root`, `utxo_root`,
  `timestamp`, `bits`, `nonce`, `stake_modifier` all available; nothing in
  validation changes.
- No consensus path reads a body older than `max_reorg_depth`; `K=1000` is a
  10× margin.
- **Local memory optimization only** — no wire format, no consensus rule, no
  genesis change, no store-schema change. The store already holds every body.
- Reorg/rollback at the maximum accepted depth (100) must still pass after
  pruning — covered by tests (§8).

## 6. Interaction with the staking-tree churn

`attempt_stake` rebuilds a full-UTXO merkle tree every attempt, including the
59,375 unspendable genesis OP_RETURN outputs
(`vtorrent-node/src/staking.rs:275-302`,
`chain/chain_reorg.rs:377`). That is the *exercise* that aggravates the
allocator, not the *rate* driver. Two separate post-soak items:

1. Cache the UTXO merkle tree between blocks (rebuild only when the UTXO set
   changes) — pure optimization, no consensus impact.
2. Exclude unspendable OP_RETURN outputs from the UTXO set — **consensus
   change** (moves `utxo_root`, needs fresh genesis), so it belongs with the
   post-soak consensus batch, not this design.

## 7. Configuration & instrumentation

- `--block-body-cache <N>` (default 1000; `0` = headers-only mode for testing).
- New gauge `vtorrent_chain_bodies_in_memory` and a `blocks_pruned_total`
  counter (`vtorrent-rpc/src/metrics.rs`).
- `debug` log per prune batch (height range), not per block.

## 8. Test plan

- **Unit**: with `K=8`, add 50 blocks → bodies for heights `0..tip-8` gone,
  headers present; `get_block` falls back to a stub `BlockBodySource`.
- **Reorg**: build a 100-deep fork (max depth), reorg after pruning → rollback
  + apply succeed, tip/UTXO/`utxo_root` unchanged vs an unpruned chain.
- **Parity/property**: two chains over the same random block sequence, one with
  `K=∞`, one with `K=8` → identical `best_hash`, `best_height`, `utxo_root`,
  and `get_utxo` results at every step.
- **Replay**: `load_into_chain` from a store with pruning on → correct tip;
  peak RSS bounded (no full-body retention).
- **Regression**: `cargo test -p vtorrent-node` (`chain_tests.rs`, reorg tests)
  pass with pruning enabled and with `K=0`.

## 9. Risks / open questions

- **RPC explorer of old blocks now hits disk.** Add a modest body LRU (or rely
  on the redb page cache) if `/block/:height` latency regresses.
- **`get_recent_transactions` scans up to `MAX_SCAN_BLOCKS = 200_000`**
  (`chain.rs:643`) and resolves spent inputs from bodies — with pruning it will
  miss bodies. Its fee/`sent` fields degrade to 0 for old txs. The code already
  flags the proper fix (an address→txids index); decide whether to bound the
  scan window harder or accept degradation.
- **Swap recovery** looks up by txid (`swap_recovery.rs`, `swap_reconciliation.rs`);
  ensure it goes through the source-backed fallback (active swaps are recent, so
  low risk, but verify).
- **Owned-vs-borrowed API churn** for `get_block` / `get_transaction` /
  `resolve_output` is the main mechanical cost.
- **`K` sizing**: 1000 covers the 100-deep reorg bound with margin; larger `K`
  only shifts the plateau, it does not change the asymptote.

## 10. Non-goals

- No consensus change, no genesis/Snapshot change, no store-schema change.
- Does not shrink the `utxo_set` or the per-block index maps (those are needed
  for fork choice) — only bodies are pruned.
- Does not implement the staking-tree cache or OP_RETURN exclusion (§6).
- No fleet deploy until after sign-off.

## 11. References

- `vtorrent-node/src/chain.rs:129` `Chain`, `:401` `get_block`, `:406`
  `get_block_at_height`, `:431` `get_transaction`, `:574` `genesis_block`,
  `:643` `MAX_SCAN_BLOCKS`, `:714` `resolve_output`, `:820` main-chain accept.
- `vtorrent-node/src/chain/chain_reorg.rs:57` rollback body read, `:166` parent
  `stake_modifier`, `:559` reorg apply body read.
- `vtorrent-node/src/block.rs:279` `Block`, `:238` `BlockHeader`.
- `vtorrent-store/src/store.rs:190` `append_block`, `:305` `get_block`, `:316`
  `get_block_at_height`, `:441` `load_into_chain`, `:649` `replay_range`.
- `docs/memory-observability-design.md` §7.7 (rate is chain-proportional).
