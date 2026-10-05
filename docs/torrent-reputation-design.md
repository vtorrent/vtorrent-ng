# Torrent Peer Reputation — Design

Status: DRAFT (no consensus change; peer layer + accounting)
Scope: `vtorrent-torrent` (peer identity, scoring), `vtorrent-p2p` (ban/selection),
RPC/UI.
Motivation: the incentive design (`docs/incentive-verification-design.md`) makes
payments *verifiable* but names **Sybil** as out of scope. This is that piece:
a reputation layer so honest peers are preferred and dishonest ones are
de-prioritized — grounded in **verified** signals, not self-reports.

## 1. Problem (grounded)

- Bans are **IP-based** (`ban_manager.rs:201` `ban_ip`) — trivially evaded by IP
  churn, and shared IPs (NAT) punish the innocent.
- Torrent `peer_id` (`peer_wire.rs:10`) is **self-declared** and not
  cryptographic — anyone can impersonate anyone.
- DHT `NodeId` is random and unbound to anything.
- So there is **no persistent, verifiable peer identity**, and therefore no
  reputation. A peer can misbehave, re-key, and return.

## 2. Design

### 2.1 Identity: bind to a VTR key

- A peer's reputation is bound to a **VTR address** (already the payment
  identity) or a dedicated pubkey, proven by a **signed challenge** at session
  start (sign a nonce; verify the pubkey hashes to the address). This makes
  identity cryptographic and stable across IP changes.
- Reuse the existing WIF/address machinery; the challenge is one BEP-10 message.

### 2.2 Signals (only verified ones count)

| Signal | Source | Weight |
|---|---|---|
| Bytes served (verified) | signed receipts (`incentive-verification-design.md`) | high |
| Disputes lost | receipt mismatch / withheld payment | negative |
| Swap completed | `SwapStatus` completion | medium |
| Availability / uptime | successful sessions over time | low |
| Self-reported claims | — | **zero** (ignored) |

### 2.3 Score

- A bounded, **time-decaying** score per identity, with separate dimensions
  (bandwidth-served, honesty, availability) rather than one opaque number.
- **Local/subjective by default**: each node scores peers from *its own*
  experience. Optionally, peers may publish **signed attestations** ("I served
  X GB to Y"), which a node may weigh — but never as ground truth (R3).
- Decay ensures old good behaviour doesn't grant permanent trust and old
  misbehaviour can be outgrown.

### 2.4 Use

- **Peer selection**: prefer high-reputation peers for new sessions.
- **Payment terms**: verified/high-rep peers can be paid promptly; unknown peers
  may face a delay or a lower cap until they build reputation.
- **De-prioritize / ban**: low-reputation identities are de-prioritized and,
  past a threshold, refused — replacing/augmenting IP bans.

### 2.5 Sybil resistance

- Identity must **cost something**: require a small **VTR bond** (or weight
  reputation by stake) so farming N identities costs N× the bond. Without a
  cost, reputation is free to forge and worthless.
- Combine with the receipt web: a peer that has served many *distinct* honest
  peers is harder to fake than one that only talks to itself.

## 3. Adversarial review

- **R1 — only verified signals.** Reputation must never accrue from
  self-reported bytes or self-declared peer_id. Tie it to signed receipts and
  on-chain events.
- **R2 — Sybil needs a cost.** Without a bond/stake weight, an attacker mints
  identities freely and the score is meaningless. Make the cost explicit and
  tune it.
- **R3 — global reputation is gameable.** A single global score invites
  brigading/defamation and needs consensus to maintain. Prefer **local,
  subjective** scoring + optional signed attestations the receiver weights
  itself.
- **R4 — privacy.** Reputation/attestations leak who transacted with whom. Keep
  the local store private; make attestation publication opt-in and coarse.
- **R5 — cold start.** New honest peers must not be starved; give unknown peers
  a neutral starting score and a path to earn.
- **R6 — identity binding must be verified.** A signed challenge at session
  start, or identity is spoofable (the current peer_id problem).
- **R7 — no consensus change.** Peer-layer only; the chain sees only payments.

## 4. Test plan

- Challenge/response binds a session to a VTR key; a forged peer_id is rejected.
- Score accrues only from verified receipts; self-reported bytes add nothing.
- Decay: a good score decays without activity; a bad score recovers over time.
- Sybil cost: N identities require N bonds (assert the gate).
- Selection prefers high-rep peers; low-rep peers are de-prioritized.
- Local store is private (no automatic gossip).

## 5. Non-goals

- Not a global consensus reputation system (explicitly rejected, R3).
- Not changing payment pricing.
- Not part of the current soak window.

## 6. References

- `vtorrent-p2p/src/ban_manager.rs:201` `ban_ip` (IP-based, the thing to augment).
- `vtorrent-torrent/src/peer_wire.rs:10` `peer_id` (self-declared).
- `vtorrent-torrent/src/dht.rs` `NodeId`.
- `docs/incentive-verification-design.md` (verified receipts = the signal source).
- `vtorrent-node/src/atomic_swap.rs` `SwapStatus` (swap-completion signal).
