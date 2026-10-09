# Governance — Implementation Plan

Companion to `docs/governance-design.md` (the design + adversarial review) and
`docs/network-upgrade-design.md` (the activation mechanism). This is the build
order for the post-soak consensus batch.

## 1. Scope (bounded, per the design)

Governance controls an **enumerable parameter set** only — never arbitrary code:

| Parameter | Current form | Notes |
|---|---|---|
| `POS_ANNUAL_RATE` | `const f64` (`consensus.rs:28`) | reward rate |
| `MIN_STAKE_AGE` | `const` + `Chain` field | already per-chain |
| `MAX_STAKE_AGE` | `const` + `Chain` field | already per-chain |
| `MIN_STAKE_AMOUNT` | `const` (`consensus.rs:43`) | stake floor |
| `TARGET_BLOCK_TIME` | `const` (`consensus.rs:53`) | block cadence |
| fee floor | mempool `min_fee_rate` | relay policy |

**Excluded:** `MAX_SUPPLY` (hard cap — immutable by design) and any code change.

## 2. Step 0 — Parameterize the consensus paths (prerequisite) — **DONE**

Implemented (`ConsensusParams` + `compute_pos_reward_with`, threaded through
`Chain` and the coinstake reward check; parity test proves the default is a
byte-identical no-op). Note: `reward_age_cap` is kept separate from eligibility
`max_stake_age` so regtest-fast (`u64::MAX` eligibility) retains the 6-day
reward cap.

### Original plan

Today `compute_pos_reward` reads the **constants** `MAX_STAKE_AGE` and
`POS_ANNUAL_RATE` directly, so a governance-set value would not take effect.
Before any voting, the reward path must read from a **parameter source**:

1. Introduce `ConsensusParams { pos_annual_rate, min_stake_age, max_stake_age,
   min_stake_amount, target_block_time }` with a `Default` equal to today's
   constants.
2. Thread it through `Chain` (it already holds `min_stake_age`/`max_stake_age`)
   and `compute_pos_reward` (change the signature to take the rate/age cap, or a
   `&ConsensusParams`).
3. **This step alone is a no-op** if the params equal the constants — verify
   byte-identical roots before proceeding.

This is the largest mechanical piece and the one that touches every consensus
path; do it first, behind tests, with no behaviour change.

## 3. Step 1 — Proposals (on-chain) — **CORE DONE**

- A proposal is a transaction carrying an OP_RETURN:
  `"VTRG1" | param_id(1) | new_value(8) | activation_height(4) | voting_end(4) | deposit(8)`.
- A **deposit** (burned on fail, refunded on pass) makes spam costly.
- Proposals are recorded in blocks; every node sees the same set (scan OP_RETURNs
  like the torrent/name registries).

## 4. Step 2 — Voting (stake-weighted, snapshot + lock) — **CORE DONE**

- **Snapshot**: voting power = the voter's stakeable UTXOs at the proposal's
  creation height (anti-flash-stake).
- **Lock**: voting coins are locked for the voting window (can't be spent or
  double-voted; reorg-safe).
- **Vote tx**: an OP_RETURN `"VTRV1" | proposal_id(8) | choice(1)`.
- **Quorum + threshold**: e.g. 33% turnout, 66% supermajority.
- **Cold staking (P2CS)**: the **spending key (owner)** votes, **not** the
  staking operator — the operator holds only the hot staking key. Enforce by
  requiring the vote to be signed by the spending key / owner address.

## 5. Step 3 — Activation

- A passed proposal sets the parameter at `activation_height`.
- **Recommended (design §2.3): scheduled upgrade.** The node reads the active
  parameter value from a **governance table** (params + activation heights,
  shipped in the release), and the BIP-9 mechanism
  (`docs/network-upgrade-design.md`) signals readiness. This avoids
  parameterizing every path at runtime and is honest about "on-chain" meaning
  *signalling + scheduled*.
- Fully on-chain activation (params read from the chain) is a larger refactor;
  defer.

## 6. Step 4 — RPC / UI — **DONE**

- `GET /api/v1/governance/proposals`, `POST /governance/propose`,
  `POST /governance/vote`, `GET /governance/params`.
- UI: a Governance page (proposals, vote, current params, activation schedule).

## 7. Adversarial review (carried from the design)

- **R1 — plutocracy.** Stake-weighted voting means the largest holders decide;
  name the trade-off, don't pretend it's solved.
- **R2 — turnout.** Set a real quorum; test the edge.
- **R3 — flash-stake.** Snapshot at creation + lock through the window.
- **R4 — P2CS vote attribution.** Owner votes, not operator — otherwise
  delegation becomes vote-buying at scale.
- **R5 — nothing-at-stake.** Vote locking prevents voting on multiple forks.
- **R6 — `MAX_SUPPLY` immutable.** Exclude it from the parameter set.
- **R7 — no treasury in v1.** A governance treasury is a capture target; defer.
- **R8 — activation is a fork.** Even scheduled activation forks if nodes don't
  upgrade; define the grace policy.

## 8. Sequencing

1. **Step 0** (parameterize) — no-op, tested. *Do first; it unblocks everything.*
2. **Step 1–2** (propose/vote) — additive; can be developed and tested on
   regtest without activating.
3. **Step 3** (activation) — via `network-upgrade`; the first real fork.
4. **Step 4** (RPC/UI) — additive.

## 9. Non-goals

- Not arbitrary code / smart contracts.
- Not a treasury (deferred).
- Not changing `MAX_SUPPLY`.
- Not part of the current soak window.

## 10. References

- `docs/governance-design.md` (design + review).
- `docs/network-upgrade-design.md` (BIP-9 mechanism).
- `vtorrent-node/src/consensus.rs:25-53` params; `:68` `compute_pos_reward`.
- `vtorrent-node/src/chain.rs:206,238` per-chain stake-age fields.
- `docs/cold-staking-p2cs-design.md` (owner vs operator vote attribution).
