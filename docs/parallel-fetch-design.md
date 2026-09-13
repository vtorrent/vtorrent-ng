# Parallel Fetch Design — vTorrent-NG Header/Block Sync

Status: DRAFT (docs-only, no fleet deployment)
Scope: `vtorrent-node` sync hot path + `vtorrent-p2p` peer scheduling
Soak constraint: **no deploy to the soak fleet** until after 2026-09-20 sign-off.
This doc is reviewable now; code lands post-soak under a feature flag.

## 1. Background

Current sync (`vtorrent-node/src/node/p2p.rs:76` `request_blocks_from_peers`,
`handler.rs:856` `handle_getheaders` / `handler.rs:926` `handle_headers`) is
strictly sequential:

- One `getheaders` **broadcast** per `sync_ticker` tick (`mod.rs:570`).
- Adaptive interval: `SYNC_INTERVAL_SECS=30`, `FAST_SYNC_INTERVAL_SECS=3`
  when `network_best_height > our_height` (`mod.rs:94`).
- One serving peer returns at most `HEADERS_PER_BATCH=2000` headers
  (`mod.rs:115`, `handler.rs:892` capped_range).
- Receiver validates `count <= 2000`, queues `getdata` for missing block hashes,
  and if `count == 2000` immediately issues the next `getheaders` to the
  **same peer** (`handler.rs:984`).

Throughput ceiling (single-peer):

- 2000 headers / 3s ≈ 667 hdrs/s. For 500k-block IBD that is ~12.5 min of
  header round-trips, plus block `getdata`/validation which is emulated as
  extra sequential hops. The node never exploits the 2+ peers it maintains
  (`TARGET_OUTBOUND` via `maintain_peers`, now randomized via
  `CANDIDATE_POOL_FACTOR=4`).

Operational observation (soak fleet, `main@c9d00a6` → `7db5da3`):

- Tip agreement and re-org recovery are clean; bandwidth, not correctness,
  is the limiter. `min_over_time(peer_count)=0` transient dips self-heal
  after the ping-timeout fix, so sharding must be resilient to transient
  0-peer windows.

## 2. Goals / Non-Goals

Goals:

- Saturate available peers during IBD and catch-up: fetch distinct header
  ranges / block batches in parallel.
- Preserve re-org safety, `chain → mempool` lock order, and the 18-test
  process-recovery / soak replay guarantees.
- Measurable: wall-clock IBD reduction with `PEX_INTERVAL`, `PING_INTERVAL`,
  ping-timeout, ban-manager thresholds unchanged.

Non-goals (this doc):

- Changing consensus, block validation, or store layout.
- Replacing header-first sync with header-tree or compact-block IBD.
- Wire-format change (reuse `getheaders` / `headers` / `getdata` / `block`).

## 3. Constraints

- No wire change: peers may run `c9d00a6` or `7db5da3`; parallel fetch must
  degrade to sequential when `peer_count==1`.
- Re-orgs are rare (none in 13-day soak) but must not be corrupted by
  out-of-order assembly. `chain.add_block` (`handler.rs:239`) is strictly
  in-order; out-of-order arrivals must queue.
- Orphan budget is bounded: `MAX_ORPHAN_BLOCKS` / `MAX_ORPHAN_BYTES`
  (`handler.rs:54` `queue_orphan`) must still hold under parallel inflight
  pressure.
- Failure to fetch a range from one peer must not stall others:
  per-range timeout + retry on a different peer.

## 4. Current Pipeline (measured)

```
sync_ticker(3s/30s) → broadcast getheaders(locator from tip, step-doubling)
    → one peer returns headers[0..2000]
    → handler validates → getdata(missing hashes) chunked to 500
    → if 2000 → getheaders(last_hash) to same peer
    → blocks arrive → handle_block → chain.add_block → orphan queue if gap
```

Bottlenecks:

1. Single outstanding `getheaders` at a time.
2. `getdata` broadcast (not per-peer assignment) in `handle_headers:977`
   — every `want` batch is eligible to be served by any peer, but the
   request is still sent to the single headers peer; other peers idle.
3. Replay after restart re-validates the entire chain (`chain.db` load at
   startup) — parallel fetch does not worsen this, but must not increase
   orphan pressure during catch-up.

## 5. Proposal — Two Phases

### Phase 1 — Parallel Block Fetch (no header sharding)

Low-risk first step. Header fetch stays sequential (one `getheaders` peer);
block fetches (`getdata`) are sharded across all connected peers.

- `handle_headers` change: instead of `send_to(same_peer, getdata)` for
  every chunk, assign chunks round-robin (or least-inflight) over
  `peer_manager.connected_peers()`.
- Keep the implicit `getheaders` continuation on the header peer only.
- Benefit: with `TARGET_OUTBOUND=8` and 500-item `getdata` chunks, a
  2000-header batch issues 4 parallel `getdata` RPCs. Expected IBD block-fetch
  time ~1/peers.

No new state machine; `orphan_blocks` already absorbs out-of-order arrivals.

### Phase 2 — Parallel Header Fetch (range sharding)

Activates only when `behind` and `peer_count >= 2`.

- Partition the outstanding header range into disjoint sub-ranges.
- Assign each range a distinct `getheaders` to a distinct peer, using the
  same locator logic but seeding the start hash per shard.

Shard construction (detail in §6). Headers phase still caps at 2000 per peer
response; the scheduler tracks per-shard `from_height → to_height` and merges
into a single ordered window before `getdata` dispatch.

## 6. Design Detail

### 6.1 Sync Scheduler

New struct `SyncScheduler` (owned by `Node`, no extra lock):

```rust
struct HeaderShard { peer: SocketAddr, from: u32, to: u32, started: Instant }
struct SyncScheduler {
    shards: Vec<HeaderShard>,
    pending_headers: BTreeMap<u32, Vec<HeaderEntry>>, // keyed by from height
    next_height: u32, // smallest height not yet dispatched to getdata
    inflight_getdata: HashMap<SocketAddr, usize>, // for least-inflight assignment
}
```

Tick logic (replaces `request_blocks_from_peers` broadcast):

1. `our_height = chain.best_height()`, `net_height = peer_manager.network_best_height()`.
2. If `our_height >= net_height`, clear shards, return (stay at 30s interval).
3. Determine peers: `connected_peers()`. If `len==1`, fall back to existing
   broadcast path.
4. Phase 1: issue single `getheaders` as today to the peer with the highest
   `network_best_height` (or current headers peer); on `headers` arrival,
   shard `want` blocks across peers via `assign_getdata()`.
5. Phase 2 (flag-gated): compute `remaining = net_height - our_height`,
   `shard_size = (remaining / peers).clamp(MAX_HEADERS_MIN_SHARD, 2000)`.
   Issue one `getheaders` per shard with locator seeded at `shard.from`.

Flag gate: `config.parallel_fetch` or `parallel_fetch_headers` (default off
until soak passes and bench-gate approves). Phase 1 can ship with flag off for
header sharding.

### 6.2 Header Range Assignment

Reuse `capped_range(from, to, 2000)` for server-side capping; shards are
purely client-side dispatch hints. Each shard's locator is built as today but
starting from `shard.from` via `chain.get_block_at_height(shard.from - 1)`
to obtain the prev hash. Hash-stop stays `[0;32]` for IBD; for catch-up
(< 2000 behind) shards collapse to one.

### 6.3 Block `getdata` Assignment

```rust
fn assign_getdata(items: Vec<InvItem>, peers: &[SocketAddr],
                  inflight: &HashMap<SocketAddr, usize>) -> Vec<(SocketAddr, Vec<InvItem>)>
```

- Order peers by `inflight` ascending, then round-robin chunk assignment
  (`chunk_getdata(items)` → 500-item chunks).
- `send_to(peer, getdata(chunk))` per assignment; keep `inflight` counters
  for back-pressure (cap: 2 outstanding `getdata` per peer).

### 6.4 Assembly and Commit Ordering

- `handle_headers` inserts `decoded_headers` into `pending_headers` keyed by
  `shard.from`; duplicate shards (re-try) are deduped by hash.
- `try_commit()` walks `next_height` upward, emitting consecutive headers
  to `getdata` assignment only when the prefix is contiguous.
- Blocks themselves commit via existing `handle_block` + `chain.add_block`;
  out-of-order blocks go to `orphan_blocks` today, no new queue needed.
  The scheduler does not reorder `handle_block` commit; it only ensures
  `getdata` requests are for contiguous prefixes when possible to reduce
  orphan pressure.

### 6.5 Failure Model

- Per-shard timeout: 10s (header) / 30s (block). On timeout, re-queue shard to
  another peer; after 2 retries mark peer slow and exclude from next tick.
- Misbehaviour: oversized `headers` (>2000) or `inv` (>1000) already bans via
  `Misbehaviour::OversizedMessage`; shard assignment does not relax those caps.
- Peer loss: `handle_peer_event(PeerDisconnected)` removes its shards and
  re-dispatches them on next tick.
- Orphan budget: `queue_orphan` evicts oldest (FIFO via `orphan_order`) when
  `len >= MAX_ORPHAN_BLOCKS` or `bytes > MAX_ORPHAN_BYTES`; parallel fetch
  respects this by throttling `getdata` concurrency (2 per peer).

### 6.6 Persistence and Recovery

No new persistence. Chain remains the sole truth (`chain.db` + `peers.dat`).
Scheduler state is ephemeral and rebuilt each tick from `chain.best_height()`.
Startup replay is unchanged; the scheduler starts idle until first
`sync_ticker` tick after `health:healthy`.

## 7. Testing

- Unit: `capped_range`, `chunk_getdata`, `assign_getdata` (deterministic),
  shard deduplication, timeout re-queue.
- Node integration: extend `vtorrent-node/src/node/handler.rs` tests:
  `handle_headers` sequential baseline still passes; new
  `handle_headers_parallel_assigns_across_peers` using virtual peers
  (`PeerManager::register_virtual_peer` as in
  `handle_block_queues_unknown_parent_without_banning_peer`).
- Process-recovery: run `cargo test -p vtorrent-daemon --release --test
  process_recovery` with flag on — IBD killed mid-shard must recover to
  the same tip as sequential (no store corruption).
- Fuzz: oversized locator (64 cap) and oversized headers (2000 cap) still
  trigger `Misbehaviour`.

## 8. Rollout

1. Land behind `parallel_fetch` flag (default off) — docs-only first PR.
2. Internal IBD bench: `cargo bench --bench consensus_hotpath` + ad-hoc
   50k-header synthetic IBD on local `regtest` cluster, compare wall-clock
   sequential vs parallel (target: >2× with 3 peers).
3. Canary on follower (e.g. node3) with flag on, soak observation, then
   full-fleet flag enable after green CI and `cargo audit` / `clippy -D warnings`.
4. No image push until soak sign-off; local `vtorrent/node:<short>` build
   via `docker/Dockerfile.release-artifact` as per `docker/testnet/README.md`.

## 9. Metrics

- `vtorrent_sync_headers_inflight` (gauge per peer)
- `vtorrent_sync_getdata_inflight` (gauge per peer)
- `vtorrent_sync_shards` (histogram of shard size)
- Reuse existing `vtorrent_block_height` + `up` + `peer_count` for sign-off
  comparison; flag off must be indistinguishable from today.

## 10. Open Questions

- Shard size tuning: `MAX_HEADERS_MIN_SHARD` (propose 500) vs fixed 2000 —
  needs bench data.
- Least-inflight vs. round-robin for `getdata` — simplest that shows gain
  is round-robin; upgrade to weighted by `peer.ping_nonces` latency if needed.
- Phase 2 header sharding pays off mainly for deep IBD (>20k behind);
  for steady-state (60s blocks, 3-peer testnet) Phase 1 alone may be sufficient
  — ship Phase 1 first, gate Phase 2 on IBD bench gate (>25% regression guard
  `scripts/bench-gate.sh`).

## 11. Alternatives Considered

- Single-peer `getblocks`+`inv` legacy path: rejected, 2–3× slower than
  header pipelining in local tests.
- Full header-tree sync: heavier, not needed for 20M-supply chain; defer.
- Replacing broadcast `getheaders` with unicast to highest-height peer
  without parallelism: low gain, keeps idle-peer problem.

## 12. References

- Current sync: `vtorrent-node/src/node/p2p.rs:76`, `handler.rs:856`,
  `handler.rs:926`, `mod.rs:570` sync_ticker / `SYNC_INTERVAL_SECS` / `HEADERS_PER_BATCH`.
- Safety: `handler.rs:54` orphan bounds, `handler.rs:209` block acceptance,
  `mod.rs` lock order `chain → mempool`.
- Ops: `docker/testnet/README.md` image discipline, `docs/soak-log.md`
  interruption recording, `scripts/bench-gate.sh` >25% gate.
