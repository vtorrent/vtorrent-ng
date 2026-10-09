# Governance Step 3 — Live Integration Design Decision

Status: DECISION NEEDED. Step 3 (live apply-path wiring) is **not** mechanical:
grounding it in the code surfaced a blocker that requires a design choice before
any implementation. Companion to `docs/governance-implementation-plan.md`.

## 1. What Step 3 requires

1. **Apply a passed parameter** to the live `chain.params` at its activation
   height — changes reward/kernel behaviour → **consensus**.
2. **Vote-locking**: reject spends of coins locked for a vote → changes
   validation → **consensus**.
3. **Snapshot voting power at proposal creation** — the anti-flash-stake
   mechanism from the design.

## 2. The blocker: no historical UTXO state

The chain keeps **only the current `utxo_set`** (`chain.rs`) — there is no
archive of past UTXO sets. So "voting power = stake at the proposal's creation
height" **cannot be computed** without either:

- **(A) A historical UTXO archive** — retain per-height UTXO snapshots (or a
  pruned/committed archive). Expensive (state bloat); the opposite of the
  memory work we just did.
- **(B) Lock-at-vote with a minimum lock age** — count voting power from coins
  **locked at vote time**, and require the lock to have existed for ≥ some age
  before the vote counts. This defeats flash-stake (you can't borrow coins and
  vote immediately) **without** historical state. Simpler; the lock is enforced
  by the consensus rule (part 2).
- **(C) Snapshot the scalar `total_staked` at creation** (cheap) and count votes
  by stake locked at vote time, accepting that a voter who acquires coins after
  the proposal can vote. Weakest anti-flash-stake.

**Recommendation: (B).** It needs no archive, gives real anti-flash-stake, and
folds naturally into the vote-locking rule (part 2). It does mean the design's
"snapshot at creation" wording changes to "lock at vote with a minimum age".

## 3. Consensus impact (all of Step 3)

- Applying params and enforcing locks both change block validation → **consensus
  changes** → they need the `network-upgrade` (BIP-9) activation and a fresh
  soak. They are **not** no-ops (unlike OP_RETURN/P2CS).
- Therefore Step 3 must be **batched with the governance activation**, not
  shipped alongside the no-op batch.

## 4. What can be done safely now (no consensus)

- The **deterministic state machine** (done: `GovernanceState`) and the
  **read-only RPC/UI** (done) — these don't touch validation.
- A **pure vote-lock helper** (`is_spend_allowed(locked, spend)`) with tests —
  additive, no wiring.
- **Nothing that changes `chain.params` or validation** until the activation
  decision.

## 5. Decision needed

1. **Anti-flash-stake model**: (A) archive, (B) lock-at-vote + min age
   (recommended), or (C) scalar snapshot.
2. **Activation**: batch Step 3 with the governance BIP-9 deployment (it is a
   real consensus change, not a no-op).
3. **Lock parameters**: minimum lock age and lock duration.

Once decided, Step 3 is a bounded implementation: enforce the lock in
`apply_transaction_journaled`, apply passed params at activation, and rebuild
`GovernanceState` on replay.

## 6. References

- `docs/governance-design.md`, `docs/governance-implementation-plan.md`,
  `docs/network-upgrade-design.md`.
- `vtorrent-node/src/governance.rs` (state machine), `chain.rs` (no historical
  UTXO state), `chain/chain_reorg.rs` (validation hooks).
