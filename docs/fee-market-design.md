# Fee Market & Mempool Policy — Design

Status: DRAFT (no consensus change; mempool + RPC + wallet)
Scope: `vtorrent-node` (mempool, fee estimation), `vtorrent-wallet` (selection),
RPC/UI.
Motivation: the mempool has fee-rate priority, eviction, and RBF
(`mempool.rs`), but fee **estimation is crude** (`recommended_fee_rate` returns
the minimum unless the pool is >50% full, then p75) and there is **no
target-based estimate, no CPFP, and no fee policy documentation**. This makes
fees predictable and the mempool well-behaved under load.

## 1. What exists

- `MempoolEntry` with `fee_sats`, `size_bytes`, `fee_rate()` (`mempool.rs:34,51`).
- Eviction of the lowest fee rate when full; `MIN_RBF_FEE_BUMP`; RBF rules
  (signals + bump + absolute fee) — documented in the module header.
- `min_fee_rate` (`:652`), `median_fee_rate` (`:662`), `recommended_fee_rate`
  (`:675`, crude), `total_fees` (`:657`).
- `GET /api/v1/fee/estimate` returns recommended/min/median + count
  (`handlers/blockchain.rs:240`).

## 2. Design

### 2.1 Target-based estimation

- Replace the crude heuristic with **confirmation-target estimates**: for
  targets of 1/2/3/6 blocks, compute the fee rate at which a tx would have
  confirmed within that target, from recent block history (the fee rates of txs
  included per block).
- Return a small table: `{ target_blocks: fee_rate }`, plus min/median for
  context. This is what wallets actually need.
- Keep it **monotonic** (target 1 ≥ target 2 ≥ …) and **bounded** (never below
  `min_fee_rate`).

### 2.2 Mempool policy (documented)

- **Admission**: `fee_rate ≥ min_fee_rate`; reject below.
- **Eviction**: when full, evict the lowest fee rate (already done); make the
  policy explicit and test it.
- **Expiry**: drop txs older than a bound (e.g. 2 weeks) so stale txs don't
  linger forever.
- **RBF**: keep the existing rules; document the bump requirement.
- **Size**: `max_mempool` (config) + byte limit; state the eviction order.

### 2.3 CPFP (child-pays-for-parent)

- Allow a wallet to accelerate a stuck tx by spending its output with a high-fee
  child; the miner's package fee rate (`(parent_fee + child_fee)/(parent_size +
  child_size)`) is what matters.
- The mempool should **track packages** (or at least not reject the child for
  having a low own fee rate when the package rate is adequate).
- Wallet: a "speed up" action that builds the CPFP child (or an RBF replacement
  when the original signals RBF).

### 2.4 Wallet fee selection

- Default to the **target-2-block** estimate; let the user pick a target
  (fast/standard/economy) with the estimated cost shown.
- Show the **absolute fee** and the **fee rate**; warn on unusually high fees
  (fat-finger protection).

## 3. Adversarial review

- **R1 — estimation must be monotonic and bounded.** A non-monotonic estimate
  (target 1 cheaper than target 2) is a bug; clamp to `min_fee_rate` and enforce
  ordering. Test.
- **R2 — CPFP needs package awareness.** Rejecting a low-fee child that pays for
  its parent defeats CPFP. Track package fee rates, or at least admit children
  whose package rate clears the bar.
- **R3 — eviction must not drop a package's parent.** Evicting a parent while
  keeping its child orphans the child; evict packages together or by package
  rate.
- **R4 — RBF vs CPFP.** Prefer RBF when the original signals it; CPFP otherwise.
  Don't offer both blindly.
- **R5 — fee sniping / reorg.** A tx confirmed then reorged returns to the
  mempool; ensure re-admission respects the current fee market.
- **R6 — no consensus change.** Mempool/policy/estimation only; block validity
  is unchanged.
- **R7 — DoS.** Estimation over recent blocks must be bounded; don't scan the
  whole chain per request.

## 4. Test plan

- Target estimates are monotonic and ≥ `min_fee_rate`; a quiet mempool returns
  the minimum.
- Eviction removes the lowest fee rate; a package is evicted together.
- Expiry drops stale txs.
- CPFP: a low-fee parent + high-fee child is admitted by package rate.
- RBF: a replacement below the bump is rejected; at/above is accepted.
- Wallet default target and fat-finger warning.

## 5. Non-goals

- Not changing block validity or the fee *rule* (fees are voluntary).
- Not a full package-relay (BIP-331) implementation (a follow-on).
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/mempool.rs:34,51,652,657,662,675` fee metadata + estimators.
- `vtorrent-rpc/src/handlers/blockchain.rs:240` `get_fee_estimate`.
- `vtorrent-wallet/src/tx_builder.rs` (fee selection).
