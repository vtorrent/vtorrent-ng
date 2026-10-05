# Seeding Policy & Bandwidth Management — Design

Status: DRAFT (no consensus change; torrent engine + config + UI)
Scope: `vtorrent-torrent` (session/engine), `vtorrent-daemon` (config), UI.
Motivation: sessions track `Downloading`/`Seeding` and speeds
(`session.rs:15,73,75`), but there is **no bandwidth limit, no seeding policy,
and no per-torrent priority**. A node that seeds torrents (to earn VTR) can
saturate the user's link, and there's no way to say "seed until ratio 2, then
stop" or "cap upload at 1 MB/s." This is table stakes for a torrent client.

## 1. What exists

- `SessionState::{Downloading, Seeding, …}` (`session.rs:15`), `download_speed`
  / `upload_speed` (`:73,75`).
- `SchedulerState` / `PieceTracker` (`scheduler.rs`) for piece/peer state.
- No rate limits, no ratio/seed-time policy, no queue/priority.

## 2. Design

### 2.1 Bandwidth limits

- **Global** upload/download caps (bytes/s), plus **per-torrent** overrides.
- Implement a **token-bucket** at the peer read/write path so limits are enforced
  across all connections, not per-socket.
- **Scheduling**: when capped, prioritize by torrent priority (below) and by
  incentive value (serve peers who pay — ties to
  `docs/incentive-verification-design.md`).

### 2.2 Seeding policy

- Per-torrent policy: **ratio target** (stop seeding at upload/download = R),
  **seed-time target** (stop after T), **seed-forever**, or **stop-on-complete**.
- **Global defaults** with per-torrent overrides.
- **Incentive interaction**: seeding earns VTR (bandwidth accounting), so
  "seed-forever" is the profit-maximizing default for a node operator — but the
  user may want to cap it. Make the trade-off explicit.

### 2.3 Queue & priority

- A **download queue** with a max number of active torrents; the rest are queued
  (metadata/peer discovery only).
- Per-torrent **priority** (high/normal/low) affecting piece requests and
  bandwidth share.

### 2.4 Scheduler fairness

- Choke/unchoke should be **fair** (round-robin / tit-for-tat) so a node isn't
  seen as a leech; the current scheduler tracks peers but fairness policy should
  be explicit and tested.

## 3. Adversarial review

- **R1 — enforce limits globally, not per-socket.** A per-connection cap is
  defeated by opening more connections. Use a shared token bucket.
- **R2 — incentive vs user intent.** Seeding earns VTR, so the node is biased to
  seed forever; the user's cap must win. Surface the earnings foregone.
- **R3 — ratio accounting must be exact.** Upload/download byte counters drive the
  policy and the incentive; a bug over/under-seeds or mis-pays. Test.
- **R4 — fairness prevents leeching.** Without tit-for-tat, peers choke us and
  throughput collapses; make the policy explicit.
- **R5 — queue starvation.** A low-priority torrent must still make progress
  (anti-starvation), or users think it's broken.
- **R6 — no consensus change.** Torrent engine + config + UI only.

## 4. Test plan

- Global cap is respected across N connections (aggregate ≤ cap).
- Ratio/seed-time policies stop seeding at the target; seed-forever continues.
- Priority affects bandwidth share; low-priority still progresses (anti-starvation).
- Byte counters are exact and feed both policy and incentive.
- Choke/unchoke fairness under many peers.
- No consensus/chain change.

## 5. Non-goals

- Not a QoS/SQM network shaper.
- Not changing the incentive pricing.
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/session.rs:15,73,75` states + speeds.
- `vtorrent-torrent/src/scheduler.rs` `PieceTracker`/`SchedulerState`.
- `vtorrent-daemon/src/config.rs` (config knobs).
- `docs/incentive-verification-design.md` (seeding earnings).
