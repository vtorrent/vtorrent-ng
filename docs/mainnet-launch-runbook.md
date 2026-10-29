# Mainnet Launch Runbook

Status: RUNBOOK. The step-by-step procedure to launch mainnet, including the
consensus-batch decision, the coordinated upgrade, verification, and rollback.
Companion to `docs/mainnet-readiness.md` (checklist) and
`docs/oncall-runbook.md` (ongoing ops).

## 1. Launch parameters (verify before launch)

| Parameter | Value | Source |
|---|---|---|
| Network magic | `56 54 52 58` (`VTRX`) | `network.rs:43` |
| P2P port | 22526 | `network.rs:46` |
| RPC port | 22527 (daemon default 22525) | `network.rs:49`, `config.rs:22` |
| Address prefix | 70 (`V…`) | `network.rs:37` |
| WIF prefix | 198 (`W…`) | `network.rs:40` |
| Genesis message | `"vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed"` | `genesis.rs:20` |
| Genesis timestamp | `1_700_000_000` — **frozen; do not change** | `genesis.rs:24` |
| Target block time | 60 s | `consensus.rs:53` |
| Annual rate | 5% | `consensus.rs:28` |
| Min/max stake age | 6 h / 6 d | `consensus.rs:31,34` |
| Min stake | 1 VTR | `consensus.rs:43` |
| Max supply | 20,000,000 VTR | `consensus.rs:46` |
| DNS seeds | seed1/2/3.vtorrent.org + `bootstrap/peers.txt` | `docs/dns-seeds.md` |
| Review candidate source | `70ea7b9` (`bab2456` plus Playwright E2E setup, Vite dev-origin CORS allowance, Tauri path capability fix, durable cold-stake registry fix, desktop restart/persistence parity fixes, wallet-path/IPC hardening, and staking ownership warnings) | `docs/security-review-brief.md` |
| Post-review mainnet fixes | `1901bb9`, `513efde`, `c465d1b` (BTC SPV peer agreement and failover; five-peer headroom and in-cycle retry); `7ec3b2f` (BTC SPV health metrics and stale-scan alert); `2c381fa` (persistent compact-filter peer cache); `aeb103a` (four fresh-review BTC findings); `8639a7f` (restart lineage); `c3b3847` (hide restored outputs); `287a505` (stuck-scan alert); `c3a79fa` (partial-scan/restart lock); `5f4c788` (canonical txid); `8ba6331` (whole-project remediation); `9a22bcd` (release lockfile); `c292fc4` (taker-side external VTR claim RPC); `dd98176` (four-peer target and 256 MiB steady RSS policy) | `docs/btc-spv-soak-plan.md` |

**Genesis is deterministic** and embeds the legacy snapshot (59,375 addresses,
11,589,746.63 VTR). `GENESIS_TIMESTAMP` and the genesis message are frozen.
Changing either value changes the genesis hash and invalidates the independent
verifier and the snapshot commitment. Verify both values; do not redefine them
at launch.

## 2. Pre-launch gate (all must be green)

1. **External security review** complete; findings fixed with regression tests
   (`docs/security-review-brief.md`, pinned to `70ea7b9`; include the `1901bb9`, `513efde`, `c465d1b`, `7ec3b2f`, `2c381fa`, `aeb103a`, `8639a7f`, `c3b3847`, `287a505`, `c3a79fa`, `5f4c788`, `8ba6331`, `9a22bcd`, and `c292fc4` mainnet fix deltas). The fresh internal review in `docs/2026-10-26_security-vtorrent-report.md` does not satisfy the independent third-party gate.
2. **Review candidate selected** (§3): the current build includes the
   consensus batch; launch on the candidate commit and image after review, not
   beta.3.
3. **Fresh 7-day soak green** on the review candidate (window
   `2026-10-21T02:47:54Z` → `2026-10-28T02:47:54Z`; `docs/soak-log.md`).
4. **`mainnet-readiness.md`** §1–§4 all `[x]` (except items explicitly deferred).

## 3. Consensus-batch decision

The current build already includes the consensus batch (OP_RETURN exclusion,
cold staking, governance, state rent) and the governance and proof-service
repairs. The core soak fleet runs `vtorrent/node:batch-e7691fb` with BTC SPV
disabled. The review candidate `70ea7b9` is `bab2456` plus the Playwright browser
E2E setup, the Vite dev-origin CORS allowance, the Tauri path capability fix,
the durable cold-stake registry persistence fix, the desktop
restart/persistence parity fixes, the wallet-path confinement, import
overwrite guard, version bump, IPC error surfacing, and staking ownership
warnings, and the live desktop journey, restart, and BTC discovery evidence. It
adds the BTC wallet birthday flag,
desktop commands, health-field updates, the shared BTC sync loop, desktop chain
persistence, and the cold-stake/BTC/compact-block/desktop hardening from the
adversarial review; rebuild the release image from it. The frontier-model review's ten findings and three follow-up concerns are addressed across `61f4be4`, `cb765ce`, `f272e64`, `3975693`, `6f82b63`, `07a6881`, `326ce27`, `39da204`, `afe5974`, and `2287026`; verified 2FA enrollment, the torrent control availability, the reviewed send quote, verified encrypted backup and restore, the guided swap workspace, the staking readiness assistant, the activity and recovery center, the cold-stake delegation wizard, node settings and diagnostics, torrent controls and reward accounting, and navigation grouping and accessibility improvements, the external review remediation, the activity journal, and the icon-label audit are in `70ea7b9`. The external review should verify them. The fresh 7-day soak
started 2026-10-21T02:47:54Z. Launch on that candidate build after the external
review and the soak. Do not launch on beta.3; it predates the governance and
proof-service repairs.

Swaps are enabled at mainnet launch. The repaired BTC SPV probe has completed
mainnet-scale header sync and a 100,001-block scan, and
`taker_claims_vtr_on_a_separate_node` validates the taker claim on a second
VTR node. The `9a22bcd` 24 h probe completed with zero scan errors; the final source
`dd98176` uses four peers and a 256 MiB steady-state alert. a fleet-level active swap soak remains open. A 60-cycle
component-level active soak passed. The header retention bound is covered by an 870,000-header test.

## 4. Launch procedure

1. **Verify the frozen genesis**: confirm `GENESIS_TIMESTAMP` and the genesis
   message match the signed parameters, rebuild, and record the genesis hash +
   the daemon binary sha256. Do not change either value; the independent
   verifier and the snapshot commitment depend on them.
2. **Build & sign the release**: tag `v2.0.0`, run the desktop matrix, publish
   with checksums (`docs/release-notes-*.md`). Run
   `python3 scripts/verify-genesis.py` and record the output with the release
   checksums.
3. **Bring up the seeds first**: deploy the launch binary to seed1/2/3; verify
   they agree on the genesis hash and connect.
4. **Publish bootstrap**: update `bootstrap/peers.txt` and the DNS seeds
   (`docs/dns-seeds.md`).
5. **Open the network**: announce; monitor peer count and height.
6. **Verify**: genesis hash identical across seeds; block production begins;
   first legacy claims succeed; no forks.

## 5. Verification checklist (post-launch)

- All seeds report the **same genesis hash** and advance together.
- The independent genesis verifier passes
  (`python3 scripts/verify-genesis.py`).
- The deployed image and commit match the build that passed the external
  review (release image rebuilt from `70ea7b9`).
- Block cadence ~60 s; no sustained divergence.
- A legacy claim succeeds end-to-end (rehearsed on regtest).
- RPC reachable, auth enforced, rate limiting active.
- Prometheus scrapes all seeds; alerts wired (`docs/oncall-runbook.md`).

## 6. Rollback / abort

- **Before the first block after genesis**: abort is trivial — stop the seeds,
  fix, restart (no chain to unwind).
- **After blocks exist**: a bad launch is a **new genesis** (the chain is
  young); coordinate a restart with a corrected binary. Record the decision in
  `docs/soak-log.md`.
- **Batch**: the batch replayed offline to an identical tip hash on the
  pre-batch chain (`docs/soak-log.md`, 2026-10-08/09). Reverting the binary is
  safe only while no OP_RETURN/P2CS output exists and the reverted binary
  accepts every block produced since the upgrade. Verify both conditions before
  reverting. After such an output exists, rollback requires a reorg or
  coordinated re-upgrade.

## 7. Post-launch

- Move to the ongoing ops runbook (`docs/oncall-runbook.md`).
- Schedule the **BTC-SPV soak** and the remaining post-soak items
  (`docs/roadmap.md`).
- The batch is included in the launch build; no BIP-9 batch upgrade is
  scheduled.

## 8. References

- `docs/mainnet-readiness.md`, `docs/soak-log.md`,
  `docs/consensus-batch-activation-plan.md`, `docs/security-review-brief.md`,
  `docs/network-upgrade-design.md`, `docs/dns-seeds.md`,
  `docs/oncall-runbook.md`, `docs/incident-comms.md`,
  `docs/backup-policy.md`.
- `vtorrent-node/src/genesis.rs`, `vtorrent-core/src/network.rs`,
  `vtorrent-node/src/consensus.rs`.
