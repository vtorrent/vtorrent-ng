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
| Review candidate source | `bab2456` (rebuild image as `vtorrent/node:batch-e7691fb`) | `docker/testnet/docker-compose.yml` |

**Genesis is deterministic** and embeds the legacy snapshot (59,375 addresses,
11,589,746.63 VTR). `GENESIS_TIMESTAMP` and the genesis message are frozen.
Changing either value changes the genesis hash and invalidates the independent
verifier and the snapshot commitment. Verify both values; do not redefine them
at launch.

## 2. Pre-launch gate (all must be green)

1. **External security review** complete; findings fixed with regression tests
   (`docs/security-review-brief.md`, pinned to `bab2456`).
2. **Review candidate selected** (§3): the current build includes the
   consensus batch; launch on the candidate commit and image after review, not
   beta.3.
3. **Fresh 7-day soak green** on the review candidate (window
   `2026-10-19T09:06:31Z` → `2026-10-26T09:06:31Z`; `docs/soak-log.md`).
4. **`mainnet-readiness.md`** §1–§4 all `[x]` (except items explicitly deferred).

## 3. Consensus-batch decision

The current build already includes the consensus batch (OP_RETURN exclusion,
cold staking, governance, state rent) and the governance and proof-service
repairs. The core soak fleet runs `vtorrent/node:batch-e7691fb` with BTC SPV
disabled. The review candidate `bab2456` adds the BTC wallet birthday flag, desktop
commands, health-field updates, the shared BTC sync loop, desktop chain
persistence, and the cold-stake/BTC/compact-block/desktop hardening from the
adversarial review; rebuild the release image from it. The fresh 7-day soak started
2026-10-20T13:51:39Z. Launch on that candidate build after the external review
and the soak. Do not launch on beta.3; it predates the
governance and proof-service repairs.

Swaps are enabled at mainnet launch. The BTC SPV mainnet-scale soak and the
separate maker and taker VTR node validation are launch gates. The header
retention bound is covered by an 870,000-header test; the active swap soak
remains to be run.

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
  review (`vtorrent/node:batch-e7691fb`, code `e7691fb`).
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
