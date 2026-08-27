# vTorrent 2.0 — Mainnet Readiness Checklist

> **Status:** Pre-launch. This document tracks everything required before
> announcing mainnet. Items are grouped by owner (code vs. infrastructure vs.
> operations). Check items off as they complete.

## 1. Code & Consensus Verification

- [x] **Genesis block verification**: hash `11f2093333a718616ba1f2173b31487cf4be5e44a861b516685acdb1088cfb21`
      verified via RPC + `test_snapshot_sum_matches_documented_supply` + `test_snapshot_binary_roundtrip`.
      See `docs/consensus-parameters.md`. Second independent verifier still needed.
- [x] **Consensus parameter sign-off**: all values frozen and documented in
      `docs/consensus-parameters.md` (network magic, ports, address/WIF prefixes,
      PoS rules, genesis hash, snapshot totals).
- [x] **Network magic + port freeze**: magic `0x56 0x54 0x52 0x58` ("VTRX"),
      P2P 22526, RPC 22527 (daemon defaults to 22525). See
      `docs/consensus-parameters.md`.
- [x] **Address/WIF prefix audit**: prefix 70 (`V...`) + WIF 198 (`7...`)
      verified end-to-end via wallet import + send + claim on soak. See
      `docs/consensus-parameters.md`.
- [ ] **Script engine differential testing**: run the legacy client's standard
      script corpus against `vtorrent-script`; results must match on every case.
- [x] **Long-run fuzzing**: 25h continuous (90001s) on all four targets
      (`fuzz_script_engine`, `fuzz_p2p_codec`, `fuzz_tx_deser`,
      `fuzz_btc_psbt`), 2026-08-24→25 — 11.4B / 41.7B / 4.7B / 3.1B
      executions respectively. Zero crashes, zero artifacts.
- [x] **Benchmark regression gate**: `scripts/bench-gate.sh` + 23 committed
      baselines in `vtorrent-node/benches/baselines/`; CI job runs on every push.
- [ ] **External security review** of wallet encryption (Argon2id +
      ChaCha20-Poly1305), RPC auth, and the atomic-swap HTLC flow.

## 2. Testnet Soak

> **Run 1 started 2026-08-24** — `docker/testnet/docker-compose.yml` on the dev
> workstation (3 regtest nodes, isolated bootstrap, node1 staking 500 VTR;
> first stake blocks expected after the 6h min stake age). Check daily with
> `scripts/soak-status.sh`; Grafana at http://localhost:3300 (admin/admin).

- [ ] **3+ node Docker testnet** (`docker/testnet/docker-compose.yml`) runs
      ≥7 days: blocks propagate between all nodes, no forks beyond expected
      PoS reorg depth, no memory growth, no peer churn storms.
- [ ] **Staking soak**: at least one node stakes continuously and produces
      blocks at the expected ~60s average over the soak window.
- [x] **Atomic swap E2E on testnet**: 2026-08-24 via compose stack — full
      VTR↔BTC cycle against BTC regtest: VTR HTLC funded (match), BTC HTLC
      funded and confirmed (P2WSH, block 127), taker claimed VTR revealing
      preimage, maker claimed BTC with preimage witness (block 130), refund
      rejected pre-expiry / accepted post-expiry on both chains. Found+fixed:
      BTC txids reported in internal byte order (`915af74`). Follow-ups filed
      in Known Issues below.
- [ ] **Legacy claim rehearsal**: import a legacy `wallet.dat`, check snapshot
      balance, submit a claim on testnet, verify funds arrive.
- [x] **Upgrade/downgrade drill**: 2026-08-24 on soak `vtr-node3` — binary
      swapped under the running container, restarted: chain state loaded from
      disk ("Resuming from persisted chain"), peers re-established, fleet tip
      hash matched; downgrade to prior binary also resumed cleanly; post-drill
      block propagated to all nodes. Note: `docker restart` is broken on the
      soak host for root-owned containers (use `sudo kill <pid>` +
      `docker start`) — investigate separately.

## 3. Network Infrastructure

- [x] **Seed nodes deployed** (3 geographically distributed VPS: DE, FI, US)
      per `docs/dns-seeds.md`; static A records live
      (`seed1/seed2/seed3.vtorrent.org`).
- [x] **Bootstrap peers published**: real IPs appended to
      `bootstrap/peers.txt` (replacing examples); CDN mirrors confirmed fresh.
- [x] **`BOOTSTRAP_PEERS` constant updated** in
      `vtorrent-p2p/src/peer_manager.rs` to match deployed seeds.
- [x] **Block explorer API** — explicitly deferred with an announcement:
      `docs/explorer-faucet-policy.md` (RPC covers all primitives; minimal
      explorer planned post-launch).
- [x] **Faucet service** — policy documented: no mainnet faucet (hard-capped
      supply); regtest faucet remains for development. See
      `docs/explorer-faucet-policy.md`.

## 4. Release Engineering

- [ ] **CI green on billing-fixed account**: tests, fmt, clippy `-D warnings`,
      cargo audit all pass on GitHub Actions (currently blocked — see below).
- [ ] **Desktop builds verified** on all three platforms from a `v*` tag:
      Linux (deb/AppImage), macOS (Intel + Apple Silicon), Windows x64.
- [ ] **Reproducible build check**: two independent builds of the same tag
      produce identical daemon binaries where feasible.
- [ ] **Release notes drafted** from CHANGELOG `[Unreleased]` section.
- [ ] **Tag created**: `git tag -s v2.0.0-beta.N && git push origin v2.0.0-beta.N`
      (beta tags first; final `v2.0.0` only after §1–§3 complete).

## 5. Operations

- [x] **Monitoring live**: Prometheus scrapes seed nodes (vtr-seed1 central;
      see `deploy/seeds-monitoring/`); Grafana dashboard shows height/peers/
      staking across the fleet; alerts on stalled height + peer-count drop
      (+ daemon down, disk, RAM) pushed via ntfy. On-call rotation/runbook
      still open below.
- [x] **On-call rotation + runbook**: `docs/oncall-runbook.md` covers node
      restart, ban-list inspection, chain-state recovery, RPC key rotation,
      seed failover/DNS changes, reorg response, and alert triage. Formal
      on-call rotation (who carries the pager) is a launch-week decision.
- [ ] **Incident comms channel** announced (status page / Telegram / X).
- [x] **Backup policy**: `docs/backup-policy.md` — daily cron snapshot of seed
      data dirs (14-day retention); genesis/snapshot binaries in repo + release
      assets, checksums appended at tag time.

## Known Blockers

| Blocker | Owner | Notes |
|---|---|---|
| GitHub Actions billing failure | Account admin | Fix at github.com/billing; until then run CI locally (`cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings`) |
| External security review not scheduled | Lead | Required before final v2.0.0 |

## Known Issues (found during soak/E2E, 2026-08-24)

- [x] ~~**Imported wallets are memory-only**~~ — FIXED `8a78acd`: encrypted
      wallet (Argon2id + ChaCha20-Poly1305) persists to `<data-dir>/wallet.json`
      (0600, atomic rename) and is restored locked on startup.
- [x] ~~**SPV wallet UTXO staleness**~~ — FIXED `8a78acd`: BIP-158 scan
      checkpoints only the covered range and loops to tip within one cycle.
- [x] ~~**BTC HTLC locktime mismatch**~~ — RESOLVED: code and docs now agree
      on 48-hour time-based CLTV (matches original design intent; stays longer
      than the ~2.4h VTR window). Block-height CLTV can be revisited at launch
      review if desired.
- [x] ~~**Docker cannot stop root-owned containers**~~ — RESOLVED `30ac2e1`:
      root cause is the **snap-packaged Docker** (AppArmor signal mediation
      denies `snap.docker.dockerd` → `docker-default`, kernel audit log
      confirms). Workaround: soak stack runs with `apparmor=unconfined`
      (trusted images; namespaces/cgroups still apply) and `docker stop` now
      works. Permanent option: migrate to apt `docker-ce`.
- [x] ~~**BTC peer IP cached across restarts**~~ — FIXED `30ac2e1`: `--btc-peer`
      stored as hostname, resolved per sync cycle and per broadcast.

> Resolved 2026-08-24: seed nodes deployed (vtr-seed1 DE, vtr-seed2 FI),
> peers.txt published with real IPs, DNS seeds live on `vtorrent.org`.
