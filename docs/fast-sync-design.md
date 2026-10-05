# Fast Sync / Snapshot Bootstrap — Design

Status: DRAFT (no consensus change; sync + store + snapshot artifact)
Scope: `vtorrent-store`, `vtorrent-snapshot`, `vtorrent-node`, `vtorrent-p2p`.
Motivation: a new node replays **every block from genesis**
(`load_into_chain`, `store.rs:441`) — slow onboarding that grows with the chain.
Modern chains onboard by downloading a **verified state snapshot** at a recent
height, then syncing forward. vTorrent already commits to the UTXO set per block
(`utxo_root`), so a snapshot can be **verified against that commitment** — the
elegant part.

## 1. What exists

- `vtorrent-snapshot`: legacy UTXO-set extraction (`snapshot_reader/writer`,
  `utxo_set.rs`, `block_parser.rs`) — the tooling to write/read a UTXO set.
- Genesis embeds a legacy snapshot (59,375 addresses).
- Per-block **UTXO commitment** (`utxo_root` in the header; `recompute_utxo_root`).
- `load_into_chain` replays all blocks from genesis.

## 2. Design

### 2.1 Snapshot artifact

- A snapshot at height `H` contains:
  - the **UTXO set** at H (txid, vout, value, script, height, timestamp),
  - the **chain state** at H: `height`, `tip_hash`, `cumulative_work`,
    `total_supply`, `total_staked`,
  - the **header** at H (so the snapshot commits to `utxo_root`).
- Format: reuse `vtorrent-snapshot`'s UTXO serialization; add the chain-state
  header.

### 2.2 Verification (the key property)

- The snapshot's UTXO set must hash to the **`utxo_root` in the header at H**.
  A node recomputes `compute_utxo_root_sorted(snapshot.utxos)` and compares to
  `header.utxo_root` — **trust-minimized**: a malicious snapshot can't forge a
  state that matches the committed root.
- The header chain from genesis to H must still be **validated** (or a
  **checkpoint** trusted, `docs/light-client-design.md`). Recommend: validate
  headers (cheap) + verify the snapshot against H's commitment.

### 2.3 Bootstrap flow

1. Sync the **header chain** to H (cheap; headers only).
2. Download the **snapshot** at H (from a peer, a mirror, or a bundled file).
3. **Verify** the snapshot against `header[H].utxo_root`.
4. Load the UTXO set + chain state into the store; set the tip to H.
5. **Sync forward** from H (normal block sync).

### 2.4 Serving snapshots

- A node can **serve** a snapshot at a recent height (bounded work: serialize its
  UTXO set). Rate-limit; a public mirror is an ops decision.
- **Bundled snapshot**: ship a recent snapshot with the release for offline
  bootstrap (like the genesis snapshot today).

### 2.5 Reorg safety

- Pick `H` **deep enough** (e.g. tip − 1000) that a reorg below H is
  implausible; if one occurs, the node must re-snapshot. Document the depth.

## 3. Adversarial review

- **R1 — verify against the commitment, always.** A snapshot that isn't checked
  against `header[H].utxo_root` is a trust hole (a peer could hand a state that
  steals/omits coins). The verification is the whole point.
- **R2 — the header chain is still trusted/validated.** Snapshot verification
  doesn't validate the *chain*; headers to H must be validated or a checkpoint
  trusted. State the trust model.
- **R3 — reorg below H.** A snapshot at H can't be rolled back below H; choose H
  deep enough and define the recovery (re-snapshot).
- **R4 — supply/staked consistency.** The snapshot's `total_supply`/`total_staked`
  must match what the chain would compute at H (they're committed indirectly via
  the UTXO set + headers); verify or recompute.
- **R5 — DoS on serving.** Serializing a large UTXO set per request is expensive;
  rate-limit and cache the serialized snapshot.
- **R6 — no consensus change.** Sync optimization; the chain rules are unchanged.
  The snapshot format is a new artifact, not a consensus rule.
- **R7 — determinism.** The snapshot's UTXO set must serialize deterministically
  (sorted) so its root is reproducible.

## 4. Test plan

- Snapshot at H verifies against `header[H].utxo_root`; a tampered snapshot
  (dropped/added UTXO) fails.
- Bootstrap: header sync → snapshot verify → load → forward sync reaches the tip.
- Reorg below H is handled (re-snapshot) or documented as out of scope.
- Supply/staked in the snapshot match a replayed chain at H.
- Serving is rate-limited; serialization is deterministic.
- No consensus change.

## 5. Non-goals

- Not a consensus change (no new rules).
- Not a hosted snapshot service (ops decision).
- Not part of the current soak window.

## 6. References

- `vtorrent-store/src/store.rs:441` `load_into_chain` (replay from genesis).
- `vtorrent-snapshot/src/` (UTXO set read/write).
- `vtorrent-node/src/chain.rs` `recompute_utxo_root`; `genesis.rs` (legacy snapshot).
- `docs/light-client-design.md` (checkpoint trust), `docs/block-body-pruning-design.md`.
