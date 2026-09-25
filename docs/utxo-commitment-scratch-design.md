# UTXO Commitment — Reusable Scratch (design)

Status: DRAFT (docs-only, no fleet deployment)
Scope: `vtorrent-node` chain apply path + staking block production
Soak constraint: no deploy until an operator window reset.

## 1. Background

The staker's RSS grows ~1070 kB/h (vs ~400 kB/h for non-stakers) even after
block-body pruning (`docs/block-body-pruning-design.md`). A dhat profile
(soak-log 2026-09-25) shows why: the UTXO commitment is rebuilt **from scratch,
allocating fresh buffers, on every block** in two places:

- **Chain apply** — `apply_block_journaled` computes the post-apply root with
  `compute_utxo_root_ordered(chain.utxo_set.values())`
  (`chain/chain_reorg.rs:206`), which allocates a leaf `Vec` and a Merkle tree.
  This runs on **every node, every block** → the non-staker ~400 kB/h.
- **Staking** — `build_from_kernel_with_proof` builds a UTXO inclusion tree
  **and** `compute_post_apply_root` walks the whole UTXO set again
  (`staking.rs:292,331`) → the extra ~670 kB/h for the staker.

Measured staking churn: **~637 MB per 35 blocks (~11 MB/block)**. The N-sized
Vecs are mmap-backed and returned, but the tree's many small upper levels and
the per-build scratch fragment the heap; the retained fraction is ~0.1–0.2%.
`perf/staking-churn` already cut this 32% by reusing pre-apply leaves and
pre-sizing tree levels, but ~4 N-sized allocations per build remain.

## 2. Why not an incremental tree (the tempting fix)

The commitment is a **sorted, positional Merkle tree**:

```
leaf(k) = hash_utxo(utxo_k)              (block.rs:329)
root    = compute_merkle_root_from_txids(leaves sorted by (txid, vout))
```

- Inserting/removing a UTXO **shifts the positions** of every later leaf, so
  every affected Merkle path changes — an index-addressed tree cannot be
  updated in O(log N). Maintaining the exact root is inherently O(N) per change.
- A Sparse Merkle Tree (key-addressed, O(log N) updates) would **change the
  root value** → a consensus change requiring a fresh genesis (same class as the
  OP_RETURN-UTXO item). Out of scope here.

So the costs are: **O(N) hashing is acceptable** (≈2–3 ms at ~80k UTXOs,
`consensus_hotpath.rs`); the problem is **allocation churn**, not CPU. The fix
is to keep the algorithm identical and stop allocating.

## 3. Design — reuse buffers, same root

Give the two call sites persistent, reusable scratch buffers so the per-block
commitment allocates nothing once capacity is warm.

### 3.1 `Chain` (apply path)

Add to `Chain`:

```rust
struct UtxoCommitment {
    /// Reused leaf buffer, sorted by (txid, vout).
    leaves: Vec<[u8; 32]>,
    /// Reused Merkle tree levels (level 0 == leaves).
    levels: Vec<Vec<[u8; 32]>>,
}
```

- `fn utxo_root_in_place(&mut self) -> [u8; 32]`: clears and fills `leaves`
  from `self.utxo_set` (BTreeMap order is already (txid,vout)-sorted), builds
  the levels into reused `levels` buffers, returns the root. No allocation
  after the first few blocks.
- `apply_block_journaled` (`&mut Chain`) uses it instead of
  `compute_utxo_root_ordered`.
- `chain.rs:439` (claim/bootstrap post-apply root) uses it too — note it builds
  a `post` set that isn't `utxo_set`; either compute into the same scratch from
  an iterator, or keep the one-off path (it is genesis/claim-only, not per
  block).

### 3.2 Staking engine

Add reusable scratch to `StakingEngine` and make
`build_from_kernel_with_proof` take `&mut self` (the loop already holds the
engine behind the node; use `self.staking.as_mut()`):

- Reuse one `leaves` buffer for the pre-apply tree and derive the post-apply
  root from it (the `perf/staking-churn` change), and reuse the tree-level
  buffers.
- The inclusion proof is read out of the reused tree; keep it as an owned
  proof (siblings are small and must outlive the borrow).

To avoid an API ripple (public `build_stake_block*` take `&self`), prefer an
interior-mutability scratch (`RefCell<...>` or `Mutex<...>`) so the public
signatures are unchanged. Decide during implementation; `RefCell` is fine
single-threaded per engine, otherwise `Mutex`.

### 3.3 Merkle builder

Add a reusable builder alongside `MerkleTree` (vtorrent-spv):

```rust
pub struct MerkleScratch { levels: Vec<Vec<[u8; 32]>> }
impl MerkleScratch {
    pub fn build(&mut self, leaves: &[[u8; 32]]) -> [u8; 32]; // root
    pub fn proof(&self, index: usize) -> Option<MerkleProof>;
}
```

`MerkleTree::build` becomes `MerkleScratch::new().build(..)` for non-hot
callers. Levels grow to the largest UTXO count once, then never realloc.

## 4. Consensus safety

- **Identical algorithm**: same leaf preimage (`hash_utxo`), same sort key,
  same Merkle combination and odd-node rule → byte-identical root. No header,
  wire, or store-schema change; no fresh genesis.
- Rollback/reorg: `rollback_journal` restores the UTXO set; the root is
  recomputed on the next apply from the same set → unchanged behaviour.
- The scratch is derived state only; it is never read for a decision other than
  producing the same root.

## 5. Test plan

- **Determinism**: for random UTXO sets and random change sequences,
  `utxo_root_in_place` == `compute_utxo_root_sorted` (byte-equal).
- **Reuse**: build the root twice in a row over shrinking/growing sets; assert
  no capacity regrowth beyond the max and the root stays correct (and unchanged
  buffers don't leak stale leaves — clear each build).
- **Apply**: `chain.add_block` still accepts PoS blocks (producer root ==
  journal root) over many blocks.
- **Staking proof**: existing `test_build_stake_block_produces_verifiable_proof`
  and SPV verification still pass.
- **Measured**: dhat probe — staking-path churn should drop from ~430 MB/35
  builds to near the leaf-buffer size × builds; RSS rate toward the non-staker
  baseline.

## 6. Risks / open questions

- **Borrow checking**: `apply_block_journaled(&mut Chain)` needs both
  `&mut scratch` and `&utxo_set`; the scratch is a separate field, so split
  borrows are fine. The `post`-set claim path needs care.
- **Capacity retention**: buffers grow to the high-water UTXO count and stay —
  bounded and desirable (that is the point), but note it in the budget.
- **`RefCell` vs `Mutex`** for the engine scratch: pick based on whether one
  engine is ever shared across threads (currently one node task).
- **Does it actually fix the rate?** The churn is proven to be allocation-driven;
  this removes the allocations. Expected large drop, but confirm with a probe
  before claiming the window passes.

## 7. Relationship to other items

- Supersedes nothing; complements `block-body-pruning` (bodies) — together they
  bound the two largest in-memory consumers.
- Orthogonal to the OP_RETURN-UTXO exclusion (consensus change, post-soak).

## 8. Adversarial review (2026-09-25)

**R1 — Which allocations actually fragment? (the load-bearing question).**
The N-sized Vecs are ≥128 KiB → glibc mmap → returned on free; they do *not*
fragment the heap. What fragments is the **many small allocations**: the Merkle
tree's upper levels (level k has N/2^k entries; below ~128 KiB ≈ level 4 they
are heap allocations) and per-build scratch (`removed` `HashSet`, `added`
`BTreeMap`, `seen` `HashSet`, `non_conflicting` `Vec`, cloned `script_pubkey`s).
`perf/staking-churn` confirmed this: pre-sizing the level `next` removed a
293 MB `finish_grow` churn point and the rate fell 19%. So the fix must reuse
**all tree levels and the small per-build sets**, not just the big leaf Vec —
`MerkleScratch` (§3.3) must own and reuse every level, and §3.2 must reuse
`removed`/`added`/`seen`.

**R2 — `RefCell` breaks `Send`/`Sync`.** `StakingEngine` lives inside `Node`,
which is shared across async tasks (`Arc`), so it must stay `Send + Sync`.
`RefCell` is neither. Use `Mutex<Scratch>` (std) — locked once per build, not on
the hot per-tick path. Verify `StakingEngine: Send + Sync` still holds after.

**R3 — Stale-buffer bugs.** Reused buffers must be fully cleared each build
(`Vec::clear` keeps capacity). A shrink/grow test (§5) is mandatory; a stale
leaf would silently change the root and split consensus.

**R4 — CPU is unchanged.** This removes allocations, not the O(N) hashing
(~2–3 ms/block at 80k UTXOs). Fine within the 60 s budget; state it so nobody
expects a speedup.

**R5 — Do the N-sized allocations *and* the chain path both need it?** The
chain's `compute_utxo_root_ordered` per block (every node) is the non-staker
~400 kB/h. Its tree levels have the same small-upper-level allocation pattern,
so `MerkleScratch` on the chain path matters as much as on the staking path.

**R6 — `Chain` split borrows.** `apply_block_journaled(&mut Chain)` needs
`&mut self.utxo_commitment` plus `&self.utxo_set`. Field-split borrows are fine
in Rust, but the root is computed *after* mutations, so order matters: mutate
`utxo_set`, then borrow scratch + set. Verify no borrow conflict; if it appears,
compute from an owned iteration.

**R7 — Non-PoS blocks compute the root needlessly.** `apply_block_journaled`
computes it unconditionally (`chain_reorg.rs:206`) but only PoS headers are
verified against it. Skipping it for non-PoS blocks (genesis/claim/faucet) is a
free extra win on regtest; keep the journal field defaulted for those. Low risk.

**R8 — High-water capacity is permanent.** The scratch holds the peak UTXO
count's worth of buffers (~6 MB at 59k) for the process life. That is the point
(no churn) but it is a new fixed floor; note it in the budget.

**R9 — Does it fix the rate?** Unproven until measured. The evidence (R1)
supports it, but the design's success criterion is a dhat probe showing
staking-path *small-allocation* churn collapse and the RSS rate approaching the
non-staker baseline. Gate any "the window passes" claim on that measurement.

## 9. References

- `vtorrent-node/src/block.rs:329` `hash_utxo`, `:361`
  `compute_utxo_root_sorted`, `:371` `compute_utxo_root_ordered`.
- `vtorrent-node/src/chain/chain_reorg.rs:206` per-block root.
- `vtorrent-node/src/staking.rs:292,331` staking trees; `:61`
  `compute_post_apply_root`.
- `vtorrent-spv/src/merkle.rs:37` `MerkleTree::build`, `:78` `proof`.
- `vtorrent-spv/src/spv_chain.rs:330` proof verification.
- `docs/soak-log.md` 2026-09-25 dhat profile; `perf/staking-churn` branch.
