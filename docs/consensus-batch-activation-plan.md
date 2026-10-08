# Consensus Batch — Activation Plan

Status: PLAN. Ties the consensus changes into a deployable sequence. Companion
to `docs/network-upgrade-design.md` (the mechanism) and `docs/roadmap.md`.

## 1. What is in the batch

| Change | Commit | Effect |
|---|---|---|
| OP_RETURN UTXO exclusion | `17c7b28` | unspendable outputs leave the UTXO set/commitment |
| Cold staking (P2CS) | `15ca950` | new stakeable script + coinstake re-lock rule |
| Governance | (design) | bounded consensus params |
| State rent | (design) | UTXO expiry/rent |

## 2. The no-op property — CORRECTED (2026-10-08)

**OP_RETURN exclusion — redesigned to a true no-op (verified).** The first
attempt excluded *all* OP_RETURN and **failed to replay at height 2** (reverted
`e52facf`): the genesis legacy-distribution outputs are `OP_RETURN <address>`
(`genesis.rs:88-93`) — claimable distribution markers, not data carriers.
`is_utxo_eligible` now excludes only **data-carrier** OP_RETURNs and retains the
genesis distribution (height 0, `LegacyClaim`). Verified by offline replay: the
batch binary reproduces the canonical tip hash (`51c64eed…` at 36065), 0 errors.

**P2CS alone IS a genuine no-op — verified.** With OP_RETURN reverted, the batch
binary replayed a node's store to the **identical tip hash** at height 35660 as
the canonical chain (`e12a953d…`), with 0 errors. P2CS changes `is_stakeable`
and the coinstake rule only for P2CS UTXOs, of which none exist pre-launch.

**Consequence:** P2CS can be adopted via a coordinated upgrade (no fresh
genesis). OP_RETURN exclusion needs a **fresh genesis** (or a redesigned
predicate that distinguishes the genesis distribution OP_RETURNs from
data-carrier OP_RETURNs) — do not ship it as a no-op.

## 3. Verify the no-op (required before activation)

Scan the chain for any output that the new rules would treat differently:

1. Any `OP_RETURN` output (would now be excluded).
2. Any `P2CS` output (would now be stakeable / subject to the re-lock rule).

If the scan finds **zero** (expected: the chain is P2PKH-only), the change is a
no-op historically and can be adopted without a fork. If it finds any, those
blocks' roots change → a **height activation** (or fresh genesis) is required.

**Implemented:** `vtorrent-cli check-consensus [--data-dir] [--regtest] [--regtest-fast-stake]`
scans the chain and reports OP_RETURN/P2CS counts (zero ⇒ no-op).

## 4. Activation options

1. **Unconditional adoption (recommended for the current batch).** Because the
   change is a no-op on the existing chain, ship it in a release and require a
   coordinated upgrade. No activation height, no fork — nodes on the new binary
   produce identical roots until an OP_RETURN/P2CS output first appears, at which
   point all upgraded nodes agree (and old nodes would diverge — so the upgrade
   must be coordinated, like any consensus release).
2. **BIP-9 soft fork** (`docs/network-upgrade-design.md`) if a mixed fleet must
   coexist: signal readiness, activate at a supermajority. More machinery; use
   when a hard cutover is unacceptable.
3. **Fresh genesis** only for changes that alter *existing* roots (e.g. if a
   future change touches zero-value outputs or reward math). Not needed here.

## 5. Rollout sequence

1. **Verify the no-op** (§3) on the production/soak chain.
2. **Release** the batch (version bump; release notes flag the consensus change
   and the coordinated-upgrade requirement).
3. **Coordinated upgrade** of all nodes (seeds first, then the fleet).
4. **Verify agreement**: all nodes report the same tip hash and `utxo_root`
   across a restart and a reorg.
5. **Soak** a fresh 7-day window on the batch binary (the previous sign-off
   validated the pre-batch rules).
6. **Then** proceed to governance / state rent (which *do* need activation).

## 6. Rollback

Because the batch is a no-op until an OP_RETURN/P2CS output appears, a rollback
is simply reverting the binary **before** such an output exists. After one
exists, rollback requires a reorg or a coordinated re-upgrade — so verify the
no-op and coordinate the upgrade to avoid ever being in that window.

## 7. Governance / state rent (later)

These are **not** no-ops: they change consensus parameters or prune normal UTXOs,
altering existing roots. They require the `network-upgrade` mechanism (BIP-9 or a
fresh genesis) and a governance decision. Do not bundle them with the no-op
batch.

## 8. References

- `docs/network-upgrade-design.md` (activation mechanism).
- `docs/op-return-utxo-exclusion-design.md`, `docs/cold-staking-p2cs-design.md`.
- `vtorrent-node/src/genesis.rs` (genesis scripts), `vtorrent-core/src/network.rs`
  (magic), `docs/soak-log.md` (the pre-batch sign-off).
