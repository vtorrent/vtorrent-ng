# State Rent / UTXO Expiry — Design

Status: DRAFT (consensus change; contentious — post-soak, fresh-genesis decision)
Scope: `vtorrent-node` (UTXO set, validation), `vtorrent-wallet`, RPC/UI.
Motivation: the UTXO set grows **unboundedly** — every output persists until
spent, and dust, lost keys, and unspendable outputs persist forever. `Utxo`
(`chain.rs:63`) has no rent or expiry. Long-term, this is state bloat: every
node must hold and commit to every unspent output forever. This design explores
**state rent** honestly, including why it is contentious.

## 1. Problem (grounded)

- `Utxo { txid, vout, value, script_pubkey, height, timestamp }` (`chain.rs:63`)
  — no storage cost, no expiry.
- The set grows with every output; spent outputs leave, but **lost-key and
  unspendable outputs never do**. `total_supply` (`chain.rs:285`) is tracked
  separately, so coins can be "unspendable but counted."
- The commitment (`recompute_utxo_root`) and the store's UTXO table both grow
  with the set — the cost is borne by every node, forever, for free.

## 2. Options (from least to most invasive)

### 2.1 Do nothing (status quo)
- Accept unbounded growth. Fine for a small chain; a long-term liability.

### 2.2 UTXO expiry (spend-by height)
- Each UTXO carries an **expiry height** (e.g. created + N years). Unspent past
  expiry is pruned from the set and **burned** (removed from supply) or made
  reclaimable by the creator.
- Simple to reason about; but **users lose coins** if they wait too long — a
  serious UX and legal risk. Requires prominent warnings and a long horizon.

### 2.3 State rent (storage fee)
- UTXOs pay **rent** for the state they occupy; rent is deducted from the output
  value over time (demurrage) or paid explicitly. When value reaches zero, the
  UTXO is pruned.
- Aligns cost with consumption; but demurrage means balances silently shrink —
  hostile to a "store of value" narrative, and hard to explain.

### 2.4 Storage-fee market (Ethereum-style)
- A base storage cost per byte; creating a UTXO costs, and the fee funds the
  state. More complex; needs a fee market for state, not just blocks.

### 2.5 Reclaimable dust
- A narrow, safer variant: only **provably-unspendable** outputs (OP_RETURN,
  already excluded by `docs/op-return-utxo-exclusion-design.md`) and **dust below
  a threshold** are pruned; normal UTXOs are untouched. This captures most of the
  bloat with the least user harm.

## 3. Recommendation

- **Do 2.5 (reclaimable dust) first** — it's the least contentious and pairs with
  the OP_RETURN exclusion. It removes the outputs that can never be spent or are
  economically dead, without touching normal balances.
- **Defer 2.2–2.4** until the chain's growth is measured and the community has
  decided. State rent is a **governance-level** decision (it changes the social
  contract of "your coins are yours forever"), so it belongs behind
  `docs/governance-design.md`, not a unilateral change.

### 3.1 CORRECTION (2026-10-07): sub-dust exclusion is NOT a no-op

Grounding 2.5 in the code before implementing it found that excluding
**sub-dust** outputs is **not** the safe, no-op subset the design assumed:

- The **genesis coinbase is a value-0 output** (`genesis.rs:99-108`) and is
  included in `all_genesis_utxos` → `header.utxo_root` (`genesis.rs:154,172`).
  Excluding value-0 outputs would change the **genesis commitment**, so the
  chain's apply path (which uses `is_utxo_eligible`) would disagree with the
  genesis builder → a fork.
- The **coinstake marker** is a value-0, empty-script output
  (`staking.rs:506-507`); excluding it changes **every coinstake block's** root.

So sub-dust exclusion requires a **fresh genesis / activation**, exactly like the
other consensus changes — it is *not* a free add-on to the OP_RETURN exclusion.
(OP_RETURN exclusion itself remains a genuine no-op: the genesis coinbase script
is the genesis *message*, not `OP_RETURN`.)

**Revised recommendation:** do **not** implement sub-dust exclusion as a "safe
subset". Either (a) leave it out entirely, or (b) fold it into the
activation-gated consensus batch with a fresh genesis. Prefer (a) until the
chain's growth is measured.

## 4. Adversarial review

- **R1 — "your coins are yours forever" is a social contract.** Expiry/rent
  breaks it. This is the single biggest objection; it must be a governance
  decision, not an engineering one.
- **R2 — burning vs reclaiming.** If expired coins are burned, supply drops
  (deflationary, but users lose funds). If reclaimable, the creator can recover
  — safer, but the UTXO still had to be pruned. Define explicitly.
- **R3 — wallet must warn loudly.** Any expiry/rent must be surfaced with ample
  lead time (ties to `docs/notifications-design.md`); a silent loss is
  unacceptable.
- **R4 — consensus + activation.** Changes the UTXO set and commitment → soft
  fork (if only pruning unspendable) or hard fork (if touching normal UTXOs).
  Use `docs/network-upgrade-design.md`.
- **R5 — supply accounting.** Pruning a UTXO must update `total_supply`
  consistently (burn) or not (reclaim); the two are different and both must be
  tested against the commitment.
- **R6 — reorg safety.** Pruning must roll back with the chain; an expired UTXO
  restored by a reorg must reappear.
- **R7 — measure first.** Don't impose rent before quantifying the actual bloat;
  the OP_RETURN exclusion may already remove most of it.

## 5. Test plan

- 2.5: OP_RETURN + sub-dust outputs are pruned; normal UTXOs untouched; supply
  and commitment consistent; reorg restores a pruned UTXO.
- 2.2 (if pursued): expiry prunes and burns/reclaims; wallet warns; reorg-safe.
- Supply accounting matches the commitment under each variant.
- No normal-balance loss without an explicit, warned policy.

## 6. Non-goals

- Not imposing rent/expiry now (deferred to governance + measurement).
- Not changing `MAX_SUPPLY`.
- Not part of the current soak window.

## 7. References

- `vtorrent-node/src/chain.rs:63` `Utxo`, `:285` `total_supply`, `:329` cap check.
- `docs/op-return-utxo-exclusion-design.md` (the safe subset).
- `docs/governance-design.md` (rent/expiry is a governance decision).
- `docs/network-upgrade-design.md` (activation).
