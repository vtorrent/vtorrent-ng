# Multi-proposal governance daemon load test

One isolated release-daemon run on 2026-10-16 exercised two proposals, two voting
waves, overlapping locks, and graceful restart. The source baseline was
`81a8179`; this follow-up changes the driver, not consensus rules or activation
scheduling.

## Workload

The daemon used `--regtest --regtest-fast-stake --testnet --isolated --no-dht`
and the test-util governance activation at height 2. RPC and P2P listened on
loopback ports 22965 and 22966.

- 32 distinct voting outpoints, each holding 2 VTR.
- Two annual-reward proposals with different values and voting deadlines.
- Two cohorts of 16 outpoints, with eight votes per transaction.
- Eight vote transactions, using independently funded fee inputs.
- Each outpoint votes once on each proposal, for 64 votes in total and 32 locks.

Both proposals were confirmed at height 44:

| Proposal | ID | Voting end | Required unlock height |
|---|---|---:|---:|
| 1, 400 basis points | `9dd9452804ee36dd` | 4044 | 4050 |
| 2, 401 basis points | `85522ac83394a6ad` | 4144 | 4150 |

These proposals remain open during the run. The test does not decide quorum or
activate either parameter change.

## Assertions at each voting stage

The first cohort votes in ascending deadline order. The second cohort votes in
descending deadline order. This checks both extending a short lock and
preserving a long lock when a shorter proposal is voted on later.

| Stage | Height | Yes stake on proposal 1 | Yes stake on proposal 2 | Locks | Verified lock expiry |
|---|---:|---:|---:|---:|---:|
| Proposals confirmed | 44 | 0 VTR | 0 VTR | 0 | None |
| Wave 1, proposal 1 | 45 | 32 VTR | 0 VTR | 16 | 4050 |
| Wave 1, proposal 2 | 46 | 32 VTR | 32 VTR | 16 | 4150 |
| Wave 2, proposal 2 | 47 | 32 VTR | 64 VTR | 32 | 4150 |
| Wave 2, proposal 1 | 48 | 64 VTR | 64 VTR | 32 | 4150 |

The verifier matches proposals by ID, checks metadata and open status, and
requires exact Yes totals with zero No and Abstain totals. For every lock it
checks the original outpoint, owner address, proposal ID, and longest expiry.
Duplicate or unknown lock rows fail the run. The expiry column records the
per-outpoint assertions that passed, not a count-only inference.

The four verifier tests detect incorrect second-proposal tallies, shortened
locks, and duplicate lock rows. They also check disjoint voter cohorts and both
deadline orders.

## Restart result

The runner stopped staking, requested SIGTERM, waited for successful shutdown,
and restarted the same store. It compared the chain tip, parameters, proposal
rows, and normalized lock rows. All matched.

The tip remained height 48 with hash
`111cb8207f941adf1ec1e2427be0dd3e23341ec02d5d4de4bd3f2a2742a0047e`.
Both proposals retained 6,400,000,000 satoshis of Yes stake, and all 32 locks
retained expiry 4150. `restart_verified` and `shutdown_graceful` were true.
Logs show replay completing at height 48 in the restarted process.

This is graceful restart coverage. It does not exercise SIGKILL, a corrupt
store tail, or a competing branch.

## Memory observations

RSS came from `/proc/<daemon-pid>/status`. A background sampler ran every two
seconds, and each phase took a synchronous sample after its state check. There
were no missed periodic samples in the completed run.

| Phase | RSS KiB |
|---|---:|
| RPC ready | 63,444 |
| Funding complete | 97,364 |
| Maturity wait complete | 97,492 |
| Proposals confirmed | 107,340 |
| Wave 1, proposal 1 | 111,148 |
| Wave 1, proposal 2 | 111,148 |
| Wave 2, proposal 2 | 111,196 |
| Wave 2, proposal 1 | 111,196 |
| Before shutdown | 111,204 |
| After restart | 63,748 |

Observed peak RSS was 111,204 KiB, about 108.6 MiB, for this configuration. Total
duration was 354 seconds, including a 103-second maturity wait. This single
run is not a capacity bound or a latency benchmark. The restarted process had
fresh caches and a locked wallet; its lower RSS is not evidence of a memory
optimization.

Faucet blocks advance timestamps by at least one second each, even when requests
arrive faster. The driver now accounts for the tip timestamp when waiting for
maturity. Fixed wall-clock waits alone can otherwise leave the tip in the
future and delay the next staking attempt.

## Artifacts and reproduction

The completed run retained `report.json`, both daemon sessions in `daemon.log`,
and the store under `/tmp/opencode/vtr-multi-gp3Pki/data`. The report includes
phase PIDs, heights, hashes, tallies, and RSS samples. An earlier setup attempt
stopped before funding because the driver expected `regtest` instead of the
RPC's actual `vtorrent-regtest` network label. That guard was corrected before
the completed run.

From the workspace root:

```bash
cargo build -p vtorrent-daemon --release --features test-util
cargo build -p vtorrent-node --release --example governance_load
target/release/examples/governance_load \
    --daemon target/release/vtorrent-daemon \
    --rpc-port 22965 --p2p-port 22966 \
    --voters 32 --votes-per-tx 8 --proposals 2 --voting-waves 2 \
    --tail-secs 0 --restart
```

The default temporary directory is removed on success. Pass a fresh or empty
`--data-dir` to retain successful-run artifacts. Failed runs retain artifacts
automatically. The runner prints one JSON result and saves it as `report.json`.

Verification ran the four example tests, strict all-targets workspace Clippy,
formatting, and the real daemon workload. The next local load scenario is a
competing-branch reorg with governance state present. Governance remains
unscheduled and inactive on the current network.
