# Block Explorer — Design

Status: DRAFT (no consensus change; read-only service over the existing store)
Scope: `vtorrent-store` (reuse tables), a read-only explorer service, optional
`vtorrent-ui` page.
Motivation: `docs/explorer-faucet-policy.md` explicitly **defers** the explorer
to post-launch and commits to "reuse the existing store tables rather than
adding a second index." This design makes that concrete and keeps the deferral
honest.

## 1. What exists

- The RPC already serves every primitive: block by height/hash
  (`handlers/blockchain.rs:90,109`), tx by id (`:131`), mempool (`:221`), peers,
  SPV status. The policy doc's position is that these are sufficient until a
  dedicated explorer ships.
- The store (`vtorrent-store`) holds blocks, UTXOs, and derived state in redb.
- The **chain keeps a full index in memory** (`height_index`, `tx_index`,
  `parent_map`, …) and, since pruning, bodies only for the recent window — older
  bodies come from the store.

## 2. Design

### 2.1 Read-only service, no second index

- A small read-only service (or a set of RPC routes) that answers explorer
  queries **from the store**, not from a new index:
  - block by height/hash (body from the store; header/index from memory or store),
  - tx by id (via `tx_index` → block → store body),
  - address history / UTXOs (scan the store's UTXO table + tx index),
  - chain stats (height, supply, staking, peers).
- This is exactly the "reuse the store tables" commitment. No new database.

### 2.2 Address index (the one real gap)

- Address→txs is the only query the current store doesn't answer cheaply (the
  in-memory `get_recent_transactions` scans a bounded window; `resolve_output`
  is display-only). Options:
  1. **Scan-based** (no new index): bounded by the store's UTXO table for
     balances, and a windowed scan for history. Cheap to build, limited history.
  2. **Derived address index** in the store (address → txids), built on apply and
     rolled back on reorg. More work, full history.
- Recommend starting with (1) and adding (2) only if history depth demands it —
  keeps the "no second index" spirit as long as possible.

### 2.3 Serving

- **Self-hosted first**: the explorer is a mode of the node (or a sibling binary)
  reading the same store, so operators can run one without extra infrastructure.
- **Public instance later**: a hosted read-only deployment is a separate ops
  decision (and a DoS surface — rate-limit, cache).

### 2.4 UI (optional)

- A minimal explorer page in `vtorrent-ui` (or a standalone static site) with
  block/tx/address views. Reuse the existing RPC client.

## 3. Adversarial review

- **R1 — do not add a second index.** The policy doc's whole point. Reuse the
  store; if an address index is truly needed, derive it *in* the store with
  reorg-aware apply/rollback, not a parallel database.
- **R2 — pruning interaction.** Bodies older than the window live only in the
  store; the explorer must read bodies from the store (the same
  `BlockBodySource` path), never assume memory. Test an old-block lookup.
- **R3 — reorg correctness.** Any derived index must roll back with the chain,
  or the explorer will show orphaned blocks/txs. Reuse the journal/rollback path.
- **R4 — DoS / cost.** A public explorer invites unbounded scans (address history
  over the whole chain). Rate-limit, cache, and bound history depth; a public
  instance is an ops decision, not a default.
- **R5 — consistency.** Explorer reads must be consistent with the node's tip;
  read under the chain lock or from a snapshot, and label confirmations
  accurately.
- **R6 — no consensus change.** Read-only.

## 4. Test plan

- Block/tx lookup by height/hash/id, including a **pruned** body (store path).
- Address balance from the UTXO table; windowed history.
- Reorg: an orphaned block/tx disappears from the explorer after rollback.
- Rate limiting / bounded history on the address endpoint.
- Confirmations are accurate and consistent with the tip.

## 5. Non-goals

- Not a hosted public service (ops decision; deferred).
- Not a second index (explicitly rejected, R1).
- Not part of the current soak window.

## 6. References

- `docs/explorer-faucet-policy.md` (deferral + "reuse store tables").
- `vtorrent-rpc/src/handlers/blockchain.rs:90,109,131,221` existing primitives.
- `vtorrent-store/src/store.rs` (blocks/UTXO tables).
- `docs/block-body-pruning-design.md` (bodies from the store).
