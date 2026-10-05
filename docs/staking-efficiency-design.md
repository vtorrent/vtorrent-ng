# Staking Efficiency & Coin Management — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet` (coin selection/consolidation), `vtorrent-rpc`, UI.
Motivation: staking weight is per-UTXO and gated by `MIN_STAKE_AMOUNT`, but the
wallet accumulates many small UTXOs (torrent payments, swap outputs, claims,
rewards). Fragmented stake is less efficient and some coins can't stake at all.
There is no tooling to see or fix this.

## 1. Problem (grounded)

- `is_stakeable` (`consensus.rs:149`) requires `value >= MIN_STAKE_AMOUNT` and a
  P2PKH script — so **dust below the minimum can never stake**.
- `compute_pos_reward(stake_amount, coin_age_seconds)` (`consensus.rs:70`) means
  reward scales with stake size and coin age (capped at `MAX_STAKE_AGE`). Many
  small UTXOs each stake separately with small weight; a few large UTXOs stake
  more effectively.
- The wallet has no view of *which* UTXOs are eligible, their coin age, or which
  are dust — and no way to consolidate.
- Consolidation **resets coin age** (a new output starts fresh), so timing
  matters: consolidate right after a stake, not just before one.

## 2. Design

### 2.1 "Stake health" view

- List the wallet's UTXOs with: value, coin age, **eligible / immature / dust**
  status, and estimated next-eligible time.
- Headline: total staked, effective weight, count of dust UTXOs, and the
  fraction of balance that is actually staking.

### 2.2 Consolidation

- **Consolidate** action: send many small UTXOs to a fresh wallet address (a
  self-send), producing fewer, larger UTXOs.
- **Timing guidance**: recommend consolidating **just after a successful
  stake** (coin age was just reset anyway) and warn that consolidating resets
  coin age for the consolidated coins.
- **Dust sweep**: specifically target UTXOs below `MIN_STAKE_AMOUNT` (they can't
  stake) and merge them into a stakeable output.
- Coin selection: prefer larger/older UTXOs for staking (the staking loop already
  iterates eligible UTXOs; expose the ordering).

### 2.3 Auto-compound

- Rewards are already re-locked by the coinstake (stake + reward), so the
  staking UTXO compounds automatically. The gap is **wallet-level** UTXOs
  (payments, claims) that sit idle.
- Optional **auto-consolidate** policy: when dust/idle UTXOs exceed a threshold,
  propose (or, opt-in, perform) a consolidation — never silently, since it costs
  a fee and resets age.

### 2.4 RPC

- `get_stake_health` (UTXO eligibility, ages, dust), `consolidate_utxos`
  (build the self-send), reusing the wallet-service payment builder.

## 3. Adversarial review

- **R1 — coin-age reset is the trap.** Consolidating resets age; doing it right
  before a stake loses rewards. The UI must warn and recommend post-stake
  timing. Test the guidance.
- **R2 — fees vs benefit.** Consolidation costs a fee; only worth it above a
  dust threshold. Show the cost/benefit and never auto-consolidate silently.
- **R3 — privacy.** Consolidation links UTXOs on-chain (common-input-ownership).
  Warn; make it opt-in and per-user.
- **R4 — `MIN_STAKE_AMOUNT` edge.** Merging dust must produce an output ≥ the
  minimum, or the consolidation is pointless. Validate.
- **R5 — no consensus change.** Wallet/RPC/UI only; the chain is unchanged.
- **R6 — don't fight the staking loop.** The staking engine picks eligible UTXOs;
  consolidation must not race it (do it while not mid-stake, or accept a missed
  tick).

## 4. Test plan

- Stake-health classification: eligible/immature/dust correct for fixtures.
- Consolidation builds a valid self-send; output ≥ MIN_STAKE_AMOUNT; fee sane.
- Dust sweep merges sub-minimum UTXOs into a stakeable one.
- Timing guidance fires (post-stake recommendation).
- No consensus/chain change; existing staking tests unaffected.

## 5. Non-goals

- Not staking pools / delegation (cold staking covers the security side; pools
  are a separate design).
- Not changing reward math or `MIN_STAKE_AMOUNT`.
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/consensus.rs:70` `compute_pos_reward`, `:149` `is_stakeable`,
  `MIN_STAKE_AMOUNT`/`MAX_STAKE_AGE`.
- `vtorrent-node/src/staking.rs` (eligible-UTXO iteration).
- `vtorrent-wallet/src/tx_builder.rs`; `vtorrent-wallet-service` payment builder.
- `docs/earnings-view-design.md` (rewards visibility).
