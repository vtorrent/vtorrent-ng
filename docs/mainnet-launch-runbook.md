# Mainnet Launch Runbook

Status: RUNBOOK. The step-by-step procedure to launch mainnet, including the
consensus-batch decision, the coordinated upgrade, verification, and rollback.
Companion to `docs/mainnet-readiness.md` (checklist) and
`docs/oncall-runbook.md` (ongoing ops).

> **Current candidate (pass 13).** The launch candidate is the pass-13
> remediation (`80f9eef` plus the docs/review package). It supersedes the
> historical `70ea7b9`/`e091936` baselines named below. The exact image, binary
> digest, window, and evidence are recorded in
> `docs/2026-11-24_ai-supplementary-review-pass13.md`. Do not launch from an
> older image or count an older window.

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
| Review candidate source | Pass-13 candidate (`80f9eef` + docs tip); see the current-candidate note | `docs/2026-11-24_ai-supplementary-review-pass13.md` |
| Post-review mainnet fixes | `1901bb9`, `513efde`, `c465d1b` (BTC SPV peer agreement and failover; five-peer headroom and in-cycle retry); `7ec3b2f` (BTC SPV health metrics and stale-scan alert); `2c381fa` (persistent compact-filter peer cache); `aeb103a` (four fresh-review BTC findings); `8639a7f` (restart lineage); `c3b3847` (hide restored outputs); `287a505` (stuck-scan alert); `c3a79fa` (partial-scan/restart lock); `5f4c788` (canonical txid); `8ba6331` (whole-project remediation); `9a22bcd` (release lockfile); `c292fc4` (taker-side external VTR claim RPC); `dd98176` (four-peer target and 256 MiB steady RSS policy) | `docs/btc-spv-soak-plan.md` |

**Genesis is deterministic** and embeds the legacy snapshot (59,375 addresses,
11,589,746.63 VTR). `GENESIS_TIMESTAMP` and the genesis message are frozen.
Changing either value changes the genesis hash and invalidates the independent
verifier and the snapshot commitment. Verify both values; do not redefine them
at launch.

## 2. Pre-launch gate (all must be green)

1. **External security review** complete; findings fixed with regression tests
   against the pass-13 candidate (`docs/security-review-request-2026-10-27.md`;
   include the pass-12/13 deltas in that document). The fresh internal review in
   `docs/2026-10-26_security-vtorrent-report.md` and the AI supplementary passes
   do not satisfy the independent third-party gate.
2. **Review candidate selected** (§3): the current build includes the
   consensus batch; launch on the pass-13 candidate commit and exact image
   after review, not beta.3.
3. **Fresh 7-day soak green** on the pass-13 review candidate (the window and
   image are recorded in `docs/2026-11-24_ai-supplementary-review-pass13.md`;
   `docs/soak-log.md`).
4. **`mainnet-readiness.md`** §1–§4 all `[x]` (except items explicitly deferred).

## 3. Consensus-batch decision

The current build already includes the consensus batch (OP_RETURN exclusion,
cold staking, governance, state rent) and the governance and proof-service
repairs. The core soak fleet runs `vtorrent/node:batch-e7691fb` with BTC SPV
disabled. The launch candidate is the pass-13 build (see the current-candidate note),
which includes the consensus batch and the pass-12/13 remediation: structural
stake reserve, live-journal removal guard, canonical torrent block geometry,
payout binding, legacy-import BTC state/default-key binding, seeder/watchdog
hardening, and the deterministic two-peer regression. It supersedes
`70ea7b9` and `e091936`; rebuild the release image from the exact commit in
`docs/2026-11-24_ai-supplementary-review-pass13.md`. The independent review
must verify the pass-13 deltas, not only the historical baseline. Do not launch
on beta.3; it predates the governance and proof-service repairs.

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
2. **Build & sign the release**: tag `v2.0.0` from a commit on `main`, run the
   desktop matrix, publish with checksums (`docs/release-notes-*.md`). Before
   tagging, dispatch `Build vTorrent Desktop Client` on the candidate commit
   (`workflow_dispatch`) and confirm all four desktop jobs, the frontend jobs,
   the audit, the image build, and the Rust suites are green. Attach only the
   desktop assets produced by that run for the same commit. The checksum job
   runs in the protected `release` environment and rejects a tag whose commit
   is not on `main`. Run `python3 scripts/verify-genesis.py` and record the
   output with the release checksums. Do not generate a production signing key
   or certificate without explicit authorization; if signing is enabled,
   `release/minisign.pub` must be committed and the secret configured first.
3. **Bring up the seeds first**: deploy the launch binary to seed1/2/3; verify
   they agree on the genesis hash and connect.
4. **Publish bootstrap**: update `bootstrap/peers.txt` and the DNS seeds
   (`docs/dns-seeds.md`).
5. **Open the network**: announce; monitor peer count and height.
6. **Verify**: genesis hash identical across seeds; block production begins;
   first legacy claims succeed; no forks.

7. **Publish the launch checkpoint**: after 10,000 blocks (about seven days),
   read the block hash at that height from at least two seeds and confirm they
   agree:
   `scripts/checkpoint-hash.sh 10000 http://seed1:22525 http://seed2:22525`.
   Publish `HEIGHT:HASH` in the release notes and
   `docs/checkpoints.md`, then restart each seed with
   `--checkpoint HEIGHT:HASH`. Every new node must use it. The daemon rejects a
   main-chain block at that height with a different hash and refuses any reorg
   whose fork point is below it. Without this, a new node can be fed a
   long-range forged chain from genesis; minimum block spacing only limits how
   fast such a chain can be produced.

## 5. Verification checklist (post-launch)

- All seeds report the **same genesis hash** and advance together.
- The independent genesis verifier passes
  (`python3 scripts/verify-genesis.py`).
- The deployed image and commit match the exact pass-13 candidate and the
  reviewed image digest.
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
- **Drill status (2026-11-24): PARTIAL.** A disposable single-node drill
  loaded a pass-13 chain containing a P2CS output and a P2CS coinstake block
  (height 10) with the rebuilt `v2.0.0-beta.3` binary; both were accepted. A
  fresh candidate data dir produced genesis `36ca792a…` and mined height 1. The
  store is readable across the versions and P2CS is **not** the rollback
  blocker. The remaining incompatible-output target is the batch's
  **governance/state-rent** path, plus a multi-node fleet reorg during rollback.
  See `/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/rollback-drill/DRILL.md`.
  The gate is not closed until those run on a disposable fleet.
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
