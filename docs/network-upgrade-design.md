# Network Upgrade & Fork Coordination — Design

Status: DRAFT (operational; enables the consensus changes)
Scope: `vtorrent-p2p` (version signaling, min-version), `vtorrent-node`
(activation heights), release/ops.
Motivation: every consensus change we have designed (P2CS cold staking,
OP_RETURN exclusion, governance) needs a **coordinated activation** mechanism.
Today there is a P2P version handshake (`PROTOCOL_VERSION = 3`,
`is_v2_peer`, `encode_for_peer`) but **no activation height, no min-version
enforcement, and no documented fork policy**. This is the backbone those changes
depend on.

## 1. What exists

- Version handshake: `VersionMsg` carries `version` + `user_agent`
  (`message.rs:53,65`); `PROTOCOL_VERSION = 3` (`:102`); `is_v2_peer` (`:131`);
  `encode_for_peer` version-gates wire encoding (`:142`).
- Per-network magic (`network.rs:18,43,72`): mainnet `VTRX`, testnet `VTRT`,
  legacy.
- No activation-height, no min-version enforcement, no fork policy.

## 2. Design

### 2.1 Version signaling

- Keep `PROTOCOL_VERSION` for **P2P wire** changes, and add a **deployment
  bitfield** (BIP-9 style) to the version/`sendaddrv2`-like message for
  **consensus** deployments, so nodes can signal readiness without a wire break.
- `user_agent` stays informational; never gate consensus on it.

### 2.2 Activation (BIP-9 for soft forks)

- A named deployment: `{ name, bit, start_height, timeout_height }`.
- A soft fork **activates** when a supermajority (e.g. 90% of blocks in a
  retarget window) signal the bit before `timeout_height`; otherwise it
  **fails** (no activation). This is the standard, well-understood mechanism.
- Nodes enforce the new rule from the activation height; before that, old rules.

### 2.3 Hard forks (fresh genesis)

- For changes that can't be soft-forked (new script semantics that old nodes
  would reject, or a fresh genesis), use a **new network magic / chain id** so
  the two chains can't cross-talk, and require an explicit operator upgrade.
- Pre-mainnet, prefer **fresh genesis** (no legacy chain to protect). Post-launch,
  hard forks need replay protection (new magic) and a coordinated height.

### 2.4 Min-version enforcement

- For **P2P-only** changes (Dandelion, PEX hardening, mobile scopes), reject or
  down-rank peers below a required `PROTOCOL_VERSION` after a grace period.
- Never enforce min-version for **consensus** rules — consensus is enforced by
  the rule at the activation height, not by peer version.

### 2.5 Which of our designs need what

| Change | Mechanism |
|---|---|
| OP_RETURN UTXO exclusion | soft fork (BIP-9) or fresh genesis |
| P2CS cold staking | soft fork (new standard script) or fresh genesis |
| Governance | needs this machinery to activate param changes |
| Dandelion++ / PEX hardening | P2P version-gated, no consensus |
| Mobile scopes / notifications | no activation (RPC/UI) |

## 3. Adversarial review

- **R1 — soft-fork signaling needs threshold + timeout.** Without a supermajority
  threshold and a timeout, activation is ambiguous and can stall or split. Use
  BIP-9 semantics and test the edges.
- **R2 — hard fork needs replay protection.** A fresh genesis with a new magic is
  clean; a same-magic hard fork risks replay across chains. Prefer fresh genesis
  pre-mainnet.
- **R3 — min-version enforcement can partition.** A too-aggressive min-version
  drops honest old peers; use a grace period and only for P2P-level changes.
- **R4 — don't conflate P2P version with consensus.** A peer's version must never
  decide consensus; the rule at the activation height does.
- **R5 — deterministic activation.** Every node must compute the same activation
  height from the same chain; test across nodes and across a reorg at the
  boundary.
- **R6 — document the fork policy.** Activation height, grace period, rollback
  plan, and communication — the ops runbook (`docs/oncall-runbook.md`) should
  reference it.
- **R7 — no consensus change by itself.** This is machinery; it changes no rules
  until a deployment uses it.

## 4. Test plan

- Deployment signaling: bit set/cleared per block; activation at the threshold;
  failure at timeout.
- Activation height is deterministic across nodes and stable across a reorg at
  the boundary.
- Min-version: a below-version peer is down-ranked after the grace period, not
  before.
- Fresh-genesis hard fork: new magic isolates the chains (no cross-talk).
- Consensus is enforced by the rule, not the peer version (a high-version peer
  sending a pre-activation block is still validated by the old rule).

## 5. Non-goals

- Not a specific deployment (this is the mechanism).
- Not changing any consensus rule itself.
- Not part of the current soak window.

## 6. References

- `vtorrent-p2p/src/message.rs:53,65,102,131,142` version handshake.
- `vtorrent-core/src/network.rs:18,43,72` per-network magic.
- `docs/cold-staking-p2cs-design.md`, `docs/op-return-utxo-exclusion-design.md`,
  `docs/governance-design.md` (the changes that need activation).
- `docs/oncall-runbook.md` (fork policy belongs there too).
