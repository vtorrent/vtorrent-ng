# vTorrent 2.0 — Mainnet Readiness Checklist

> **Status:** Pre-launch. This document tracks everything required before
> announcing mainnet. Items are grouped by owner (code vs. infrastructure vs.
> operations). Check items off as they complete.

## 1. Code & Consensus Verification

- [ ] **Genesis block verification**: independently recompute the genesis hash
      from `vtorrent-node/src/genesis.rs` and confirm the embedded legacy UTXO
      snapshot totals (59,375 addresses, 11,589,746.63 VTR) match the extracted
      snapshot. Two people verify with independent tooling; record both hashes.
- [ ] **Consensus parameter sign-off**: confirm final values in
      `vtorrent-core/src/constants.rs` — 60s block time, 5% annual reward,
      min stake 1 VTR, stake age 6h–6d, max supply 20M VTR, adjustment
      interval 2016 blocks. These are consensus-critical after launch.
- [ ] **Network magic + port freeze**: magic `0x56 0x54 0x52 0x32` ("VTR2"),
      P2P 22526, RPC 22525. Document any deviation for hosted deployments.
- [ ] **Address/WIF prefix audit**: address prefix 70 (`V...`), WIF prefix 198
      (`7...`) verified against legacy claim tooling end-to-end.
- [ ] **Script engine differential testing**: run the legacy client's standard
      script corpus against `vtorrent-script`; results must match on every case.
- [ ] **Long-run fuzzing**: ≥24h continuous runs of all four targets
      (`fuzz_script_engine`, `fuzz_p2p_codec`, `fuzz_tx_deser`,
      `fuzz_btc_psbt`) with zero crashes on the release commit.
- [ ] **Benchmark regression gate**: record criterion baselines; CI fails if
      sighash/merkle/kernel regress >25%.
- [ ] **External security review** of wallet encryption (Argon2id +
      ChaCha20-Poly1305), RPC auth, and the atomic-swap HTLC flow.

## 2. Testnet Soak

- [ ] **3+ node Docker testnet** (`docker/testnet/docker-compose.yml`) runs
      ≥7 days: blocks propagate between all nodes, no forks beyond expected
      PoS reorg depth, no memory growth, no peer churn storms.
- [ ] **Staking soak**: at least one node stakes continuously and produces
      blocks at the expected ~60s average over the soak window.
- [ ] **Atomic swap E2E on testnet**: full VTR↔BTC swap cycle executed against
      BTC regtest via the compose stack (fund → claim → refund paths all hit).
- [ ] **Legacy claim rehearsal**: import a legacy `wallet.dat`, check snapshot
      balance, submit a claim on testnet, verify funds arrive.
- [ ] **Upgrade/downgrade drill**: stop a node, upgrade binary, restart —
      chain state loads from disk and sync resumes without manual repair.

## 3. Network Infrastructure

- [x] **Seed nodes deployed** (2 of ≥3 geographically distributed VPS) per
      `docs/dns-seeds.md`; static A records live (`seed1/seed2.vtorrent.org`).
      Third node in another region still pending.
- [x] **Bootstrap peers published**: real IPs appended to
      `bootstrap/peers.txt` (replacing examples); CDN mirrors confirmed fresh.
- [x] **`BOOTSTRAP_PEERS` constant updated** in
      `vtorrent-p2p/src/peer_manager.rs` to match deployed seeds.
- [ ] **Block explorer API** available (read-only chain index) or explicitly
      deferred with an announcement.
- [ ] **Faucet service** for new users on testnet only; mainnet faucet policy
      documented (or none).

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

- [ ] **Monitoring live**: Prometheus scrapes seed nodes; Grafana dashboard
      (`docker/grafana/dashboards/vtorrent-overview.json`) shows height/peers/
      staking across the fleet; alerts on stalled height + peer-count drop.
- [ ] **On-call rotation + runbook**: node restart, ban-list inspection,
      chain-state recovery from store corruption, RPC key rotation.
- [ ] **Incident comms channel** announced (status page / Telegram / X).
- [ ] **Backup policy**: seed node data dirs snapshotted daily; genesis and
      snapshot binaries archived in ≥2 locations with checksums published.

## Known Blockers

| Blocker | Owner | Notes |
|---|---|---|
| GitHub Actions billing failure | Account admin | Fix at github.com/billing; until then run CI locally (`cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings`) |
| External security review not scheduled | Lead | Required before final v2.0.0 |

> Resolved 2026-08-24: seed nodes deployed (vtr-seed1 DE, vtr-seed2 FI),
> peers.txt published with real IPs, DNS seeds live on `vtorrent.org`.
