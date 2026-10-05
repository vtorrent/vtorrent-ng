# Staking Pools & Delegation — Design

Status: DRAFT (delegation = no consensus change; pooled = consensus + trust)
Scope: `vtorrent-node` (staking), `vtorrent-wallet`, RPC/UI.
Motivation: the reward math makes **larger stake more efficient**, so users want
to combine stake — but naive pools reintroduce custody risk. This design offers
a **trustless delegation** model first and a **pooled** model second, with the
trade-offs explicit.

## 1. Why pools (grounded)

- `compute_pos_reward(stake_amount, coin_age)` (`consensus.rs:70`) pays
  `stake_amount × 0.05 × min(age, MAX_STAKE_AGE)/year`.
- Kernel probability is `value / total_staked` (`check_stake_kernel_v2`).
- Expected reward rate for a UTXO of value `v` is therefore
  `(v/total) × v × rate × age` — **quadratic in `v`** for fixed age.
- So splitting stake across UTXOs *reduces* total yield, and dust below
  `MIN_STAKE_AMOUNT` earns nothing. This is the economic pull toward combining
  stake (and toward consolidation — `docs/staking-efficiency-design.md`).

## 2. Model A — Delegated cold staking (trustless, recommended first)

- A delegator creates a **P2CS** UTXO (`docs/cold-staking-p2cs-design.md`) and
  hands the **staking key** to an operator; the **spending key stays cold**.
- The operator stakes the delegator's coins. Because a P2CS coinstake must pay
  the stake **back to the same P2CS script**, the **reward re-locks to the
  delegator's own coins** — no redistribution, no custody, no trust.
- **No combined weight**: each delegator's UTXO stakes independently, so this
  does *not* capture the quadratic pooling benefit. Its value is **convenience
  and uptime** (the operator runs a reliable node), not higher yield.
- **Operator fee**: the operator must be paid. Two options:
  1. **Consensus fee output** — allow a P2CS coinstake to include an operator
     fee output (a cut of the reward). Automatic, but a consensus change.
  2. **Periodic subscription** — the delegator pays the operator periodically
     (like the torrent incentive). No consensus change; requires the delegator
     to pay.

## 3. Model B — Pooled staking (higher yield, trust-minimized)

- Delegators deposit into a **pool script** (multisig or a pool contract) so the
  pool stakes as **one large UTXO**, capturing the quadratic benefit.
- The coinstake reward re-locks to the pool script (cold-staking rule).
- **Reward distribution** is the hard part: proportional shares must be
  withdrawable. Without smart contracts, this needs either:
  - a **coinstake fee/split output** (consensus), or
  - an **operator-managed accounting** with periodic payouts (trust + audit).
- **Trust**: the pool script's spending authority must be constrained (multisig,
  timelock) or the operator can abscond. Reputation
  (`docs/torrent-reputation-design.md`) + a bond mitigate but do not eliminate.

## 4. Recommendation

- **Ship Model A (delegated cold staking) first** — it is trustless, needs no
  reward redistribution, and is the natural extension of P2CS. Its honest pitch
  is "stake without handing over your keys," not "higher yield."
- **Model B later**, only with a trust-minimized script (multisig/timelock) and a
  clear audit story. Do not ship a custodial pool under a "trustless" banner.

## 5. Adversarial review

- **R1 — don't call a custodial pool trustless.** Model A is trustless; Model B
  is not (unless the script constrains spending). Label them honestly.
- **R2 — the quadratic reward is the real incentive; state it.** Users will pool
  because of it; hiding it invites worse (custodial) alternatives.
- **R3 — operator fee must be enforceable.** A P2CS coinstake can't pay the
  operator without a consensus rule; otherwise the fee is a handshake. Pick §2
  option 1 (consensus) or 2 (subscription) explicitly.
- **R4 — delegation ≠ pooling.** Model A does not combine weight; don't imply it
  improves yield.
- **R5 — pool withdrawal safety.** Model B's withdrawal must not be gated on the
  operator's cooperation; use a script-enforced rule or don't ship it.
- **R6 — reorg/rollback.** Any pool accounting must roll back with the chain.
- **R7 — consensus surface.** Model A with a subscription fee is no-consensus;
  Model A with a fee output and all of Model B are consensus changes → fresh
  genesis, post-soak.

## 6. Test plan

- Model A: a delegator's P2CS UTXO staked by an operator re-locks the reward to
  the delegator's own script; the operator cannot spend it.
- Operator fee (subscription): periodic payment succeeds; (fee output): the
  coinstake split validates.
- Model B: pool stakes as one UTXO; a delegator withdraws without operator
  cooperation (script-enforced); accounting rolls back on reorg.
- Yield check: combined stake yields more than split (asserts the quadratic
  property) — a test that documents the economics.

## 7. Non-goals

- Not a custodial staking service.
- Not changing the reward math.
- Not part of the current soak window.

## 8. References

- `vtorrent-node/src/consensus.rs:70` `compute_pos_reward`, `:174`
  `check_stake_kernel_v2`, `:149` `is_stakeable`.
- `docs/cold-staking-p2cs-design.md` (the trustless primitive).
- `docs/staking-efficiency-design.md` (consolidation), `docs/torrent-reputation-design.md`.
