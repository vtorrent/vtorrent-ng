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

- Process RSS: <150 MiB per node over 7 days at regtest stake rate (60s blocks,
  4 UTXOs) — compare with `soak-status.sh` baseline ~99/71/70 MiB at 06:07Z.
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
