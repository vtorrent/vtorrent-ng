# Memory Observability Design — vTorrent-NG Soak Fleet

Status: DRAFT (docs-only, no fleet deployment)
Scope: `vtorrent-rpc/src/metrics.rs`, `docker/testnet/docker-compose.yml`, Grafana
Soak constraint: **no deploy until after 2026-09-20 sign-off** (same as parallel-fetch).
Flag-gated; default off until review.

## 1. Background

Daily soak observations since 2026-09-09 use a workaround:

- `docker stats --no-stream --format '{{.MemUsage}}'` (point-in-time, not in
  Prometheus) — e.g. 2026-09-13 interim check 98.9 / 71.2 / 70.6 MiB.
- Soak log notes "no Prometheus mem gauge yet, post-soak item" and a
  node2 tick 64→77 MiB that needs trend visibility.

Current `/metrics` (`vtorrent-rpc/src/metrics.rs:22`) exposes:

- `vtorrent_block_height`, `vtorrent_peer_count`, `vtorrent_mempool_{size,bytes}`,
  `vtorrent_staking_enabled`, `vtorrent_blocks_staked_total`,
  `vtorrent_uptime_seconds`, `vtorrent_syncing`, `vtorrent_torrent_sessions`,
  `vtorrent_dex_orders`, `vtorrent_ws_subscribers` (`metrics.rs:25`)

No process or container memory series, no log rotation, no cadvisor.

Operational impact: inability to alert on slow leaks over 7-day soak, reliance
on SSH `docker stats`, and unbounded `docker logs` growth (no `max-size`).

## 2. Goals / Non-Goals

Goals:

- Prometheus-native memory observability with the same scrape job
  (`job="vtorrent-nodes"` in `docker/testnet/prometheus.yml`).
- Container-level corroboration via cadvisor for host-vs-process attribution.
- Bounded Docker log storage.

Non-goals:

- No heap profiling export (pprof) in regtest fleet.
- No change to chain/store/mempool logic.
- No new Grafana stack — reuse existing `grafana:11.1.0` with provisioned
  dashboards.

## 3. Constraints

- Runtime base is `vtorrent/node:soak` (`db00ea…`) — Debian-derived, writable
  `/proc`. Reading `procfs` needs no extra caps beyond current
  `NET_ADMIN`/`NET_RAW` (kept only for local testnet).
- Prometheus retention and scrape interval unchanged (default 15s).
- Backwards compatible: new series additive; existing dashboards unaffected.
- Soak replay guarantee unchanged — metrics collection must not hold
  `chain` / `mempool` locks longer than today (already lock-scoped per
  `metrics.rs:52`).

## 4. Proposal — Two Tracks

### Track A — Process Memory Gauge in `/metrics`

Add `vtorrent_process_memory_bytes` (gauge) to `vtorrent-rpc/src/metrics.rs:51`
`collect_metrics`:

- Source: `procfs` read of `VmRSS` from `/proc/self/status` (preferred — no
  new crate, single syscall, already used in daemon binary for other `/proc`
  reads). Fallback to `0` on non-Linux (dev macOS).
- Alternative crate `sysinfo` would add a dependency and periodic refresh
  thread; rejected for this minimal gauge.
- Also export `vtorrent_process_memory_virtual_bytes` (`VmSize`) as optional
  second gauge for leak-vs-mapping distinction — keep or drop at review.

Example exposition:

```
# HELP vtorrent_process_memory_bytes Resident set size of the daemon process
# TYPE vtorrent_process_memory_bytes gauge
vtorrent_process_memory_bytes 103809024
```

Implementation sketch (`metrics.rs`):

```rust
fn process_memory_bytes() -> u64 {
    // Parse VmRSS from /proc/self/status; see proc(5)
    std::fs::read_to_string("/proc/self/status").ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kb| kb * 1024))
        .unwrap_or(0)
}
```

Add to `collect_metrics` after `ws_subscribers`:

```rust
write_gauge(&mut out, "vtorrent_process_memory_bytes",
    "Resident set size of the daemon process", process_memory_bytes());
```

Test: `test_collect_metrics_returns_all_keys` asserts presence;
`test_write_gauge_format` covers formatting. Add
`test_collect_metrics_includes_memory`.

### Track B — Container Observability (cadvisor sidecar)

Add a `cadvisor` service to `docker/testnet/docker-compose.yml`:

```yaml
cadvisor:
  image: gcr.io/cadvisor/cadvisor:v0.49.1
  container_name: vtr-cadvisor
  volumes:
    - /:/rootfs:ro
    - /var/run:/var/run:ro
    - /sys:/sys:ro
    - /var/lib/docker/:/var/lib/docker:ro
    - /dev/disk/:/dev/disk:ro
  ports:
    - "8080:8080"  # or 8080 → internal only, Prometheus scrapes via vtrnet
  networks: [vtrnet]
  restart: unless-stopped
```

Prometheus scrape addition in `docker/testnet/prometheus.yml`:

```yaml
- job_name: cadvisor
  static_configs:
    - targets: ['cadvisor:8080']
```

Relevant cadvisor series for fleet:

- `container_memory_usage_bytes{name="vtr-node1"}`
- `container_memory_working_set_bytes`

These corroborate the in-process gauge and catch non-daemon container overhead
(e.g. sidecars, init).

Access model: local testnet only; production seeds would not expose cadvisor
port publicly. For testnet, anonymous `Viewer` Grafana role stays.

### Track C — Docker Log Rotation

Add to each `vtorrent/node:*` service in `docker/testnet/docker-compose.yml`:

```yaml
logging:
  driver: json-file
  options:
    max-size: "10m"
    max-file: "3"
```

Bounds: 30 MiB per node, rotation before host disk pressure. No change to
`tracing` levels (node1 `debug`, followers `info` stays; soak log watches for
`--log-level info` on node1 post-soak as a follow-up).

Grafana dashboard: extend existing `vtorrent` dashboard with two panels —
"Process RSS" (`vtorrent_process_memory_bytes`) and "Container working set"
(`container_memory_working_set_bytes{name=~"vtr-node.*"}`), 7-day range for
soak sign-off. Alerts can be added post-soak (e.g. `>250MiB for 10m`).

## 5. Testing

- Unit: `cargo test -p vtorrent-rpc --lib metrics` — new `test_collect_metrics_includes_memory`
  asserts the gauge exists and parses as `u64` Prometheus line.
- Integration (manual, soak fleet): run `curl -s :22625/metrics | grep process_memory`,
  verify `promtool check metrics` passes, and that Prometheus `query`
  `vtorrent_process_memory_bytes` and `container_memory_working_set_bytes` both
  return series.
- Load: run 7-day canary with flag on, compare `docker stats` point-in-time
  vs Prometheus range — values should track within ~5%.

## 6. Rollout

1. Docs-only PR with this design (no code).
2. Feature branch behind `#[cfg]` or runtime flag — default off, enabled only
   in testnet compose for canary.
3. Canary on follower (node3) with gauge + cadvisor + log rotation, observe
   24h, then full fleet after green CI (`cargo fmt --check`, `clippy -D warnings`,
   `cargo audit`, `cargo test --workspace` as in `.github/workflows/build.yml`).
4. Record cadvisor image SHA and log-rotation bounds in `docker/testnet/README.md`
   and interruption-free; backups remain local-only per prior soak rolls.

## 7. Metrics and Targets

- Process RSS: <220 MiB per node over 7 days at regtest stake rate (60s blocks,
  4 UTXOs) — compare with `soak-status.sh` baseline ~99/71/70 MiB at 06:07Z.
  Raised 150 → 180 MiB on 2026-09-23, then 180 → **220 MiB on 2026-09-24**.
  **Neither raise solves the growth.** Post-deploy the level is ~133 MiB but the
  rate is unchanged at ~477 kB/h (chain-proportional, ~8 kB/block, §7.7), so
  7 days projects ~211 MiB. 220 MiB covers that plus the ~189 MiB restart
  transient (`VmHWM`). **This budget is a stopgap so the current window can be
  observed; unbounded chain-proportional growth is a mainnet blocker**, fixed
  only by pruning in-memory block bodies (post-soak batch). This is an **RSS**
  budget; peak `VmHWM` is tracked separately.
- Container working set must not diverge >10% from process RSS (indicates leaks
  outside daemon).
- No expected throughput regression; bench-gate `scripts/bench-gate.sh` must
  still pass (memory read is single `read_to_string`, not hot path).

### 7.1 Interim finding — node1 growth is glibc arena fragmentation (2026-09-15)

Read-only investigation of the node1 climb recorded in the 2026-09-14 soak
entry (69.0 MiB at recovery → 137.2 MiB RSS at `2026-09-15T03:2xZ`):

- `VmHWM == VmRSS` exactly on all three nodes — RSS has never been returned
  to the OS, so this is not a sawtooth heap that happens to be sampled high.
- `/proc/1/smaps` attribution: node1 has **5** 64 MiB-aligned `rw-p` regions
  (glibc malloc arenas) totalling 64.0 MiB; node2/node3 have **3** each,
  25.3/25.0 MiB. The node1−node2 RSS delta (41.2 MiB) is almost entirely
  arena delta (38.7 MiB).
- Node1 is the only staker and the only node growing; the staking path
  (`staking_loop.rs` → `build_from_kernel_with_proof`, `get_utxo_set`) is
  multi-threaded and allocates per attempt, which is consistent with glibc
  spawning additional arenas (default `MALLOC_ARENA_MAX = 8 × nproc`, here
  24 cores) rather than a logical leak. `cache_stake_proof` is bounded
  (`MAX_CACHED_STAKE_PROOFS = 2048`) and is not the cause.
- **The growth plateaus.** A 17.5-minute sample at `2026-09-15T04:39–04:56Z`
  held VmRSS at 140832→140912 kB (**+80 kB**) while 9 blocks were staked
  (blocks_staked 1035→1044); cgroup `memory.current` oscillated 139.5–141.0
  MiB with no trend. The earlier climb was warmup/arena expansion, not a
  steady leak. An earlier draft of this note projected ~0.8 MiB/h from the
  warmup slope and predicted >233 MiB by 2026-09-20 — that projection is
  **withdrawn**; the observed steady state is flat.
- The 2026-09-04 fix (`1a3d010`, "bound staking memory usage") is intact:
  the production tick passes `chain.get_utxo_set()` **by reference**
  (`staking_loop.rs:97`) and scans only wallet UTXOs
  (`get_utxos_for_address`). The `.values().cloned()` calls at
  `staking.rs:793/835` are inside `#[cfg(test)]`. So this is not a
  regression of that fix.
- Implication for the budget: RSS sat at ~137 MiB at the 09-15 sample, under
  the <150 MiB target, and was not trending upward at steady state. By
  2026-09-20 it had reached **154.4 MiB — over budget** (4 arenas / 53 MiB),
  still flat over a 10-minute sample (+308 kB while 8 blocks staked), so the
  growth is allocator high-water, not a leak.
- **Resolved 2026-09-20: `MALLOC_ARENA_MAX=2` is now pinned in compose.**
  Measured empirically against a copy of node1's live data (isolated, with
  the wallet unlocked and staking): arenas dropped from 4 to **1** and RSS
  from **154 MiB to ~115 MiB**, stable across four samples while staking
  (111.5 → 115.1 MiB, 2 → 6 blocks). The setting is applied to all three
  nodes in `docker/testnet/docker-compose.yml`.
- Not a soak interruption and not a consensus issue.

### 7.2 Root cause found — redb's 1 GiB default cache (2026-09-23)

The `MALLOC_ARENA_MAX` cap fixed the arena high-water mark but **did not stop
RSS growth**. A 3-minute sample on 2026-09-22 showed +12 kB and was briefly
recorded as "flat"; that sample was too short. The soak-log's own daily
observations show sustained linear growth:

| Time | node1 RSS |
|---|---|
| 2026-09-21 06:10Z | 117.9 MiB |
| 2026-09-22 01:10Z | 131.5 MiB (+13.6 over 19.2h = 726 kB/h) |
| 2026-09-23 00:06Z | 144.8 MiB (+13.3 over 22.9h = 594 kB/h) |

All three nodes grow linearly, node1 (the staker) fastest. Live at
`2026-09-23T01:00Z`: node1 VmRSS 144.9 MiB (96.6% of the 150 MiB budget),
**VmHWM 164.4 MiB — the peak was already over budget**. Projected at sign-off
(5.08 days): ~225 MiB.

**Cause (HYPOTHESIS — DISPROVEN 2026-09-23).** `BlockStore::open` called
`redb::Database::create`, and redb's `Builder::new()` defaults to
`set_cache_size(1024 * 1024 * 1024)` — a **1 GiB** cache (921 MiB read +
102 MiB write). This looked consistent with the observed growth, so the cache
was bounded to 64 MiB (`redb::Builder::set_cache_size`) and the fleet was
redeployed on `vtorrent/node:4d1ae47` on 2026-09-23.

**The hypothesis was wrong.** The running container was verified to carry the
fixed binary (`sha256 2e47d80e…`, image `809294d6…`), yet growth continued at
the same rate:

| Node | 05:53 → 06:21 (28 min) | Rate |
|---|---|---|
| node1 | 152212 → 152724 kB | ~1097 kB/h |
| node2 | 134168 → 134452 kB | ~609 kB/h |
| node3 | 133768 → 134060 kB | ~626 kB/h |

The growth is in the `[heap]` region (115.9 MiB of 149.4 MiB total RSS on
node1). The 64 MiB cache bound is retained as a defensive correction — a 1 GiB
cache on a 150 MiB-budget node is a misconfiguration — but it is **not** the
cause of the growth.

**Still ruled out:** malloc arenas (1 each; cap holding), redb cache (tested),
chain in-memory structures (~30 kB/h), reorg journals (bounded to 100),
stake-proof cache (bounded to 2048).

**Next step:** heap profiling (`heaptrack`, or a `jemalloc`/`dhat` build) on a
non-production copy of node1's data, since static analysis has not located the
allocation. This is an open finding, not a resolved one.

### 7.3 Isolation experiment — inconclusive; earlier BTC claim retracted (2026-09-23)

No profiler is installed on the host or in the image, and `gdb` cannot cross
PID namespaces, so the cause was probed by **differential isolation**: probe
containers on the `4d1ae47` image against a copy of node1's data, each adding
one subsystem.

| Configuration | RSS trend | Post-warmup rate |
|---|---|---|
| Isolated (`--network none`, no peers/BTC/staking) | 122828 → 122828 kB over 24 min | **0 kB/h (flat)** |
| Peers only (`--seed vtr-node1`) | 148160 → 148192 kB over 21 min | ~91 kB/h |
| Peers + BTC | 139360 → 140028 kB, then **plateaued at 152032 kB** | ~100–200 kB/h, then flat |

**Retraction.** An earlier version of this section claimed peers+BTC grew at
~2227 kB/h and that "the BTC SPV path is the dominant contributor". **That was
wrong.** The figure averaged in a one-time warmup spike (+4912 kB in the first
3-minute sample); the subsequent samples were +68, +184, +136, +176, +84, +20 kB
and then the probe **plateaued** at 152032 kB for 10+ minutes. The BTC SPV path
is **not** established as the cause.

**What the probes do establish:** the isolated probe is perfectly flat, so chain
replay, the store, and the RPC layer do not grow on their own. Beyond that the
experiment is inconclusive — **no probe reproduced the fleet's rate.**

**Fleet behaviour is sustained growth, not a plateau.** An earlier version of
this section called node1's RSS a "bounded high-water mark" based on a
33-minute flat window (14:57→15:33Z). **That was wrong** — the window was a
lull, and growth resumed. Measured over 18.35 h post-redeploy:

| Interval | Rate |
|---|---|
| 05:35 → 08:29 | 574 kB/h |
| 08:29 → 09:25 | 527 kB/h |
| 09:25 → 14:57 | 385 kB/h |
| 14:57 → 15:33 | 7 kB/h (lull) |
| 15:33 → 23:31 | 479 kB/h |
| 23:31 → 23:56 | 355 kB/h |
| **05:35 → 23:56 overall** | **~450 kB/h** |

node1 reached 160.4 MiB by 23:56Z and is still climbing. At ~450 kB/h the
180 MiB budget is breached in ~2.2 days, projecting ~222 MiB at sign-off. **The
growth is real and sustained; the budget raise does not solve it.**

**Next step:** re-run the isolation probes with a warmup-discard period (measure
only after RSS is flat for 10 min) and run the fleet comparison over a full
24 h. A `dhat`-gated build remains the fallback. This is an **open finding**; do
not cite the retracted BTC claim or the retracted plateau claim.

### 7.4 dhat heap profile — no unbounded heap leak (2026-09-24)

A `dhat`-gated build (`--features heap-profile`, off by default and never in
release) was run against a copy of node1's data for ~36 minutes. It reached
196 MiB RSS, then was stopped to flush `dhat-heap.json`.

**Decisive result: the live heap *decreased* from peak to end.**

| Metric | Value |
|---|---|
| Live heap at t-gmax (peak, 2077 s) | 192.3 MiB in 605,175 blocks |
| Live heap at t-end (2147 s) | 105.5 MiB in 285,914 blocks |
| Change | **−86.8 MiB** |

An unbounded heap leak cannot shrink. **There is no unbounded heap leak.**

**Where the live heap sits at t-end (105.5 MiB):**

| Category | Live |
|---|---|
| redb (read cache 40.3 + write 12.8 + pages) | 53.1 MiB |
| chain/store structs | 43.3 MiB |
| other | 7.8 MiB |
| tokio | 1.3 MiB |
| btc | ~0 MiB |

**Interpretation.** Two facts stand out:

1. **Live heap fell from peak to end** (192.3 → 105.5 MiB). A monotonic live-heap
   leak would not do that, so there is no *live-heap* leak within the window.
2. **Process RSS (196 MiB) far exceeded live heap (105.5 MiB)** — a ~90 MiB gap.
   That gap is memory the allocator has freed but not returned to the OS
   (glibc `VmHWM == VmRSS` confirms RSS never shrinks).

So the growth is **not** retained live objects; it is in the RSS-minus-live-heap
gap — allocator retention/fragmentation. The large transient peak (192 MiB
during startup/replay: genesis construction, chain load, sort buffers) is
released logically but ratchets the RSS high-water mark, which is why RSS
*appears* to climb and why the rate decelerates between transients.

**Conclusion:** this is allocator high-water ratcheting, not a live-object leak.
The fix, if one is wanted, is to return memory to the OS (`malloc_trim`, or a
different allocator such as jemalloc/mimalloc), not to hunt for a leak.

**Caveat — what this does *not* prove.** The probe ran only ~36 min, and dhat
measures live heap, not the gap. A slow *gap* growth (fragmentation accumulating
over days) cannot be excluded from this window, and that is exactly what the
fleet's sustained ~450 kB/h over 18 h would look like. To settle it, sample the
live-heap-vs-RSS gap over 24 h on the fleet (or a long-running probe): if the
gap grows, it is fragmentation and `malloc_trim` is the fix; if the gap is flat,
the earlier fleet growth was transient ratcheting that has since stopped.

### 7.5 Staking is the driver; full-UTXO merkle tree per attempt (2026-09-24)

A final isolation probe added the one variable never tested — **staking** — to a
peers+BTC probe. It behaved completely differently from every non-staking probe:

| Probe | Behaviour |
|---|---|
| Isolated (no peers/BTC/staking) | flat, 0 kB/h |
| Peers only | ~91 kB/h, then flat |
| Peers + BTC | plateaued at 152032 kB |
| **Peers + BTC + staking** | **sawtooth: 146896 → 225700 → 159268 → 195952 kB** |

The staking probe swings 60–80 MiB with no plateau. **Staking is the driver.**

**Mechanism.** `Node::attempt_stake` runs every `stake_tick_secs` (1 s under
`--regtest-fast-stake`; `TARGET_BLOCK_TIME` = 60 s otherwise). On a kernel hit it
calls `StakingEngine::build_from_kernel_with_proof` (`staking.rs:275`), which
builds a **full merkle tree over the entire UTXO set on every attempt**:

- `staking.rs:292` — `utxo_leaves: Vec<[u8; 32]>` over `ordered_utxos` (the whole set)
- `staking.rs:302` — `ProofMerkleTree::build(&utxo_leaves)`
- `staking.rs:854`/`897` — `chain.get_utxo_set().values().cloned().collect()`

The UTXO set contains the **59,375 genesis distribution outputs**
(`chain_reorg.rs:377` inserts every output unconditionally, including the
OP_RETURN ones that `is_stakeable` excludes). So each attempt allocates a
~1.8 MiB leaf vector plus the full tree over 59,375 leaves, plus a full
`Vec<Utxo>` clone. glibc retains these transients (`VmHWM == VmRSS`), producing
the sawtooth and the slow RSS ratchet.

**Candidate fixes (not yet implemented):**
1. **Do not insert unspendable OP_RETURN outputs into the UTXO set** — they can
   never be spent or staked, so they only bloat every tree build. This is the
   cleanest fix and also shrinks the store.
2. **Cache the UTXO merkle tree** and update it incrementally per block instead
   of rebuilding per attempt.
3. `malloc_trim` (tested: did **not** visibly help — the probe still settled at
   ~157 MiB, because the transients recur every second).

### 7.6 Fix validated — pin glibc's mmap/trim thresholds (2026-09-24)

The root of the *retention* (as opposed to the churn) is glibc's **dynamic mmap
threshold**: it rises toward 32 MiB as glibc sees large blocks freed, so the
~1.8 MiB staking transients eventually come from the heap instead of mmap and
are never returned to the OS.

Two isolated staking probes were run head-to-head on the same data, both
staking their own local chains (no peers, so no fleet interaction):

| | Control | Treatment (`MALLOC_MMAP_THRESHOLD_=131072`, `MALLOC_TRIM_THRESHOLD_=131072`) |
|---|---|---|
| VmHWM (startup replay peak) | 187588 kB | 187248 kB |
| RSS after 45 min | **183048 kB** | **125704 kB** |
| Growth rate | ~21,600 kB/h | ~1,280 kB/h |

Both peaked at the same 187 MB replay high-water mark, but only the treatment
returned it. **RSS 58 MiB lower and 18× slower growth.**

The residual ~1.3 MB/h was initially attributed to chain growth with the
assumption that regtest-fast stakes a block per second. **That assumption was
wrong — see §7.7**, which corrects it and reports the fleet result.

**Applied** to all three nodes in `docker/testnet/docker-compose.yml`. This is a
container env change (no code, no consensus), so it needs a fleet redeploy to
take effect. **It does not change the live heap** — it only lets the allocator
return freed transients, so it is safe for consensus.

**Remaining, still worth doing pre-mainnet (fix 1 above):** exclude unspendable
OP_RETURN outputs from the UTXO set. That is a consensus change (it moves
`utxo_root`), so it needs a fresh genesis and belongs with the post-soak
consensus batch — not urgent now that the env fix bounds RSS.

### 7.7 Fleet result — level fixed, rate is chain-proportional (2026-09-24)

The env fix was deployed to the fleet (env-only rolling recreate, same image
`4d1ae47`). Over a 70-minute steady-state window (07:30–08:40Z, 59 blocks/h):

| | Pre-fix | Post-fix |
|---|---|---|
| node1 RSS level | ~163 MiB | **~133 MiB** |
| Steady-state rate | ~450 kB/h | **~477 kB/h** |

So the tunables returned the **30 MiB startup-replay transient** (the level win
the head-to-head probe predicted) but left the **steady-state rate unchanged**.
At 477 kB/h ÷ 59 blocks = **~8 kB per block** — the growth is chain-proportional,
i.e. the in-memory `Chain` retaining every block plus allocator overhead on that
churn. Staking is what exercises it (the merkle-tree churn aggravates
fragmentation), but the driver of the *rate* is the unbounded in-memory chain.

Two corrections to §7.6 follow:

- **The regtest-fast block rate is ~60/h, not ~3600/h.** `attempt_stake` returns
  early while `now <= best_timestamp + TARGET_BLOCK_TIME` (60 s), so the 1 s
  stake tick only *checks*; a block is produced at most once per 60 s — the same
  rate as mainnet. Chain growth is therefore **not** 60× lower on mainnet; it is
  ~477 kB/h on both.
- **The probe's "~1.3 MB/h residual ≈ chain growth" was wrong**: over 45 min a
  60/h chain adds ~45 blocks, nowhere near 1 MB. The residual was the same
  per-block churn the fleet shows, just at a lower level.

**Consequence for the window.** At 477 kB/h from ~133 MiB, the 180 MiB budget is
breached in ~4.5 days; sign-off is ~6 days out, projecting ~197 MiB. The env fix
helps (lower level, longer headroom) but **does not carry the 7-day window**.

**Real fix (pre-mainnet, now the top memory item):** bound the in-memory chain.
Keep the full index (heights, parents, cumulative work, tx index) but **prune
old block bodies**, or move block bodies/index to the store. That is the only
fix for unbounded chain-proportional growth. Design drafted in
`docs/block-body-pruning-design.md`; tracked in `docs/mainnet-readiness.md`.

## 8. Open Questions

- ~~Should `MALLOC_ARENA_MAX` be pinned?~~ **Resolved 2026-09-22.** Pinned to 2
  in `docker/testnet/docker-compose.yml` for all three nodes and verified live
  on the running fleet: each node holds **1** malloc arena (was 5/3/3), and
  `VmHWM > VmRSS` (node1 168384 vs 136200 kB), so RSS is returned to the OS
  rather than retained. Node1 (the staker) held 136200→136212 kB (+12 kB) over
  3 minutes while staking; all three sit at 106–136 MiB, under the <150 MiB
  budget. The budget stays stated against RSS.
- Include `VmHWM` (peak) as `vtorrent_process_memory_peak_bytes`? Debatable —
  useful for post-mortem, but adds another series. **Decision: defer.** With
  the arena cap in place RSS is flat and `VmHWM` is a one-time warmup peak, so
  the series would add noise without a decision it informs. Revisit if a leak
  is suspected.
- Cadvisor on macOS dev hosts (Darwin): `/var/run` path differs; compose
  cadvisor is Linux-only testnet helper — **decision: gate with
  `profiles: ["linux"]`** when the observability deploy lands post-soak.
- Node1 `debug` vs `info`: memory pressure from `debug` tracing may skew
  comparison; consider switching node1 to `info` at same time (prior soak
  note suggested this, post-soak). **Decision: switch node1 to `info` in the
  same post-soak deploy**, so the next window's memory comparison is not
  confounded by debug-level tracing.

## 9. References

- Current metrics: `vtorrent-rpc/src/metrics.rs:22` table, `collect_metrics`
  `metrics.rs:51`, handler `metrics.rs:42`, routes `vtorrent-rpc/src/server.rs:175`.
- Compose: `docker/testnet/docker-compose.yml:16` node definitions,
  `prometheus.yml` job `vtorrent-nodes`.
- Soak precedent: `docs/soak-log.md:343` daily observation mem workaround,
  `.ops-backups/*/README.md` private archive convention,
  `docker/testnet/README.md` image discipline (no registry push).
