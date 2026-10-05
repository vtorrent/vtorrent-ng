# On-Chain Governance — Design

Status: DRAFT (consensus change; post-soak, fresh-genesis decision)
Scope: `vtorrent-node` (proposals/votes/activation), `vtorrent-wallet`, RPC/UI.
Motivation: consensus parameters are **hardcoded constants**
(`consensus.rs:25-53`: `POS_ANNUAL_RATE`, `MIN/MAX_STAKE_AGE`,
`MIN_STAKE_AMOUNT`, `TARGET_BLOCK_TIME`, `MAX_SUPPLY`) and there is **no
governance surface** — changing any of them means a hard fork with no
legitimacy mechanism. This design adds **stake-weighted, on-chain governance**
for a defined parameter set, with the hard problems named.

## 1. What governance would control (bounded)

- **Parameter set** (the safe, enumerable subset): `POS_ANNUAL_RATE`,
  `MIN_STAKE_AGE`, `MAX_STAKE_AGE`, `MIN_STAKE_AMOUNT`, `TARGET_BLOCK_TIME`,
  fee floor. **Not** `MAX_SUPPLY` (hard cap — immutable by design) and **not**
  arbitrary code.
- **Treasury** (optional): a protocol-defined share of block rewards to a
  treasury address, spendable by governance. Decide whether to have one at all
  (§4).

## 2. Design

### 2.1 Proposals

- A proposal is a transaction (or a dedicated message) that commits to:
  `{ param, new_value, activation_height, voting_window, deposit }`.
- A **deposit** (burned or refunded on pass) makes spam costly.
- Proposals are recorded on-chain (in blocks) so every node sees the same set.

### 2.2 Voting

- **Stake-weighted**: voting power = stake at a **snapshot height** taken at
  proposal creation, so stake can't be moved in after seeing the proposal
  (anti-flash-stake).
- **Vote locking**: voting coins are locked for the voting window, so they can't
  be spent *and* staked elsewhere simultaneously (anti-double-vote / nothing-at-stake).
- **Quorum + threshold**: require a minimum turnout (e.g. 33% of staked supply)
  and a supermajority (e.g. 66%) to pass.
- **Delegation interaction**: for P2CS cold-staked coins, the **spending key
  (owner)** votes, not the staking operator — the operator holds only the
  staking key. Define this explicitly (§5 R4).

### 2.3 Activation

- A passed proposal changes the parameter at `activation_height`. Because
  consensus params are compiled constants today, this requires either:
  1. **Node upgrade** (params read from a governance table, shipped in the
     release) — governance is *signaling + scheduled*, nodes must upgrade; or
  2. **Param-in-header** (nodes read the active params from the chain) — fully
     on-chain, but every consensus path must read the parameterized value, a
     large, risky refactor.
- Recommend **(1) for the first iteration**: governance is binding *in intent*,
  activation is a scheduled upgrade. It avoids parameterizing every consensus
  path and is honest about what "on-chain" means.

## 3. Adversarial review

- **R1 — plutocracy / vote-buying.** Stake-weighted voting means the largest
  holders decide. Mitigations (quadratic, caps, delegation) all have flaws;
  name the trade-off and don't pretend it's solved.
- **R2 — low turnout.** Without quorum, a small active minority decides. Set a
  real quorum and test the edge.
- **R3 — flash-stake / snapshot.** Voting power must be snapshotted at proposal
  creation and coins locked through the window, or an attacker borrows stake,
  votes, and returns it.
- **R4 — cold-staking vote attribution.** With P2CS, the operator holds the
  staking key and the owner holds the spending key. The **owner** must vote (the
  operator is a service provider, not the owner). If the operator could vote the
  delegated stake, delegation becomes vote-buying at scale. Enforce owner-votes.
- **R5 — nothing-at-stake.** Vote locking (R3) prevents voting on multiple forks
  with the same stake; without it, governance is unsafe under reorg.
- **R6 — `MAX_SUPPLY` must be immutable.** A governance system that can raise the
  cap is a permanent inflation risk. Exclude it from the parameter set.
- **R7 — treasury is a honeypot.** A governance-controlled treasury invites
  capture and theft. If included, constrain spending (multisig, timelock,
  per-epoch cap) and be explicit that it's a trust surface.
- **R8 — activation is a hard fork.** Even "scheduled upgrade" activation forks
  if some nodes don't upgrade; define the fork policy (activation height + grace).
- **R9 — scope.** Large and consensus-touching; post-soak, fresh genesis, its own
  soak. Do not rush it.

## 4. Treasury — recommend deferring

- A treasury needs a funding source (a cut of rewards) and spending rules; both
  are contentious and a capture target. **Recommend no treasury in v1** —
  governance over parameters only. Revisit with a concrete, constrained design.

## 5. Test plan

- Proposal lifecycle: create (with deposit) → vote → pass/fail → activation.
- Snapshot: stake moved after creation doesn't add voting power.
- Lock: voting coins can't be spent or double-voted; reorg-safe.
- Quorum/threshold edges (exactly at quorum; just below).
- Cold-staking: the owner (spending key) votes, not the operator.
- `MAX_SUPPLY` is not a votable parameter.
- Activation at height changes the parameter deterministically across nodes.

## 6. Non-goals

- Not arbitrary code execution / smart contracts.
- Not a treasury (deferred, §4).
- Not changing `MAX_SUPPLY`.
- Not part of the current soak window.

## 7. References

- `vtorrent-node/src/consensus.rs:25-53` hardcoded consensus params.
- `docs/cold-staking-p2cs-design.md` (owner vs operator keys → vote attribution).
- `docs/staking-pools-design.md` (delegation).
