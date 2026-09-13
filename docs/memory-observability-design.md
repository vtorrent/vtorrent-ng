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

## 8. Open Questions

- Include `VmHWM` (peak) as `vtorrent_process_memory_peak_bytes`?
  Debatable — useful for post-mortem, but adds another series.
- Cadvisor on macOS dev hosts (Darwin): `/var/run` path differs; compose
  cadvisor is Linux-only testnet helper — gate with `profiles: ["linux"]`?
- Node1 `debug` vs `info`: memory pressure from `debug` tracing may skew
  comparison; consider switching node1 to `info` at same time (prior soak
  note suggested this, post-soak).

## 9. References

- Current metrics: `vtorrent-rpc/src/metrics.rs:22` table, `collect_metrics`
  `metrics.rs:51`, handler `metrics.rs:42`, routes `vtorrent-rpc/src/server.rs:175`.
- Compose: `docker/testnet/docker-compose.yml:16` node definitions,
  `prometheus.yml` job `vtorrent-nodes`.
- Soak precedent: `docs/soak-log.md:343` daily observation mem workaround,
  `.ops-backups/*/README.md` private archive convention,
  `docker/testnet/README.md` image discipline (no registry push).
