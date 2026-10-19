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
| RPC port | 22525 | `config.rs:22` |
| Address prefix | 70 (`V…`) | `mainnet::PUBKEY_ADDRESS_PREFIX` |
| WIF prefix | 198 (`7…`) | `mainnet::SECRET_KEY_PREFIX` |
| Genesis message | `"vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed"` | `genesis.rs:20` |
| Genesis timestamp | `1_700_000_000` — **set to the real launch time** | `genesis.rs:24` |
| Target block time | 60 s | `consensus.rs:53` |
| Annual rate | 5% | `consensus.rs:28` |
| Min/max stake age | 6 h / 6 d | `consensus.rs:31,34` |
| Min stake | 1 VTR | `consensus.rs:43` |
| Max supply | 20,000,000 VTR | `consensus.rs:46` |
| DNS seeds | seed1/2/3.vtorrent.org + `bootstrap/peers.txt` | `docs/dns-seeds.md` |
| Reviewed build | `vtorrent/node:batch-ff6f472` (code `ff6f472`) | `docker/testnet/docker-compose.yml` |

**Genesis is deterministic** and embeds the legacy snapshot (59,375 addresses,
11,589,746.63 VTR). Changing `GENESIS_TIMESTAMP` or the message changes the
genesis hash — freeze both before launch and record the resulting hash.

## 2. Pre-launch gate (all must be green)

1. **External security review** complete; findings fixed with regression tests
   (`docs/security-review-brief.md`, pinned to `ff6f472`).
2. **Reviewed build selected** (§3): the current build includes the consensus
   batch; launch on the reviewed commit and image, not beta.3.
3. **Fresh 7-day soak green** on the reviewed build (window
   `2026-10-19T09:06:31Z` → `2026-10-26T09:06:31Z`; `docs/soak-log.md`).
4. **`mainnet-readiness.md`** §1–§4 all `[x]` (except items explicitly deferred).

## 3. Consensus-batch decision

The current build already includes the consensus batch (OP_RETURN exclusion,
cold staking, governance, state rent) and the governance and proof-service
repairs. The fleet is soaking `vtorrent/node:batch-ff6f472` (code `ff6f472`).
Launch on that reviewed build after the fresh 7-day soak. Do not launch on
beta.3; it predates the governance and proof-service repairs.

## 4. Launch procedure

1. **Freeze genesis**: set `GENESIS_TIMESTAMP` to the launch time, rebuild, and
   record the genesis hash + the daemon binary sha256.
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
- The deployed image and commit match the reviewed build
  (`vtorrent/node:batch-ff6f472`, code `ff6f472`).
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
- **Batch**: because it is a no-op until an OP_RETURN/P2CS output appears,
  reverting the binary before such an output is safe. After one exists,
  rollback requires a reorg or coordinated re-upgrade, so verify the no-op and
  coordinate to avoid that window.

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
  `docs/oncall-runbook.md`, `docs/backup-policy.md`.
- `vtorrent-node/src/genesis.rs`, `vtorrent-core/src/network.rs`,
  `vtorrent-node/src/consensus.rs`.
