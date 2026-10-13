# Governance pre-activation validation

This follow-up to `c3c08c8` checks staking eligibility and governance-journal
memory. Governance remains unscheduled and inactive on the current network.

## Staking eligibility

`activation_cannot_remove_every_eligible_stake` failed before the repair. The
validator accepted a 1,200 VTR stake minimum when every available staking output
was smaller. The test observed `min_stake_amount = 120000000000` instead of the
previous `100000000` satoshis.

The repair validates each activation against the pre-block UTXO set and the
block's timestamp. At least one P2PKH or P2CS output must meet the candidate
amount, minimum age, and maximum age requirements. Otherwise, consensus keeps
the old parameters and records `applied = true, activation_failed = true`.
Here `applied` marks the completed activation attempt, not a successful change.
The failed proposal is not retried automatically.

The regression covers three unsafe changes:

- A stake minimum above every available output.
- A maximum stake age below the age of every available staking output.
- A minimum stake age above the age of every available staking output.

The producer calls `next_block_params(timestamp)` with its candidate timestamp
and predicts the same outcome as validation. A separate test accepts a safe
stake-minimum change, recounts the kernel denominator, and verifies rollback.

This is an eligibility check. It does not prove key availability, online
participation, or future block production.

## Journal memory

The earlier implementation retained a full governance-state copy in each of
100 journals. Failed-reorg protection cloned that journal deque again. Memory
therefore grew with state size multiplied by journal depth.

`GovernanceUndo` now stores changes to proposal metadata, vote identities, and
locks. A temporary pre-state copy still exists during block application, and
failed-reorg protection still copies the current governance state once. The
retained history stores compact undo records. Closed proposals release voter
identities after the tally is decided; their counts remain available.

### Method

The Linux release-build probe uses the real `GovernanceState` and
`GovernanceUndo` types. It initializes proposal and voter records, rolls the
journal deque through two full windows, and optionally moves stake locks and
adds new votes. It then creates the governance and journal backups used by the
failed-reorg path, checks equality, and attempts one replacement-block
transition while the backups remain live. Finally, it restores the backups and
drops the state.

An allocator wrapper counts requested live Rust heap bytes and the peak.
Samples from `/proc/self/status` report process RSS, including samples while
the temporary pre-state copies remain live. Requested bytes exclude
allocator metadata and native allocations. RSS includes those costs in the
probe process.

The fixtures bypass signature verification and omit the daemon, UTXO store,
networking, block bodies, and their rollback data. These numbers measure the
governance component, not total node memory or transaction-processing latency.
The `--voters` option counts distinct voting outpoints. Owner labels have a
fixed address length; the probe does not model distinct wallet processes.

The full-snapshot mode reconstructs the previous retention model in the same
binary. Each comparison alternated five full-snapshot and five compact-undo
processes. The largest workload used five compact-only processes to avoid the
old model's allocation cost. All 45 reported runs completed, checked the backup
contents, and released all measured component allocations.

The host had 24 logical CPUs and a load average of 2.35 at the start. No timing
claim is made. The probe stops a workload at its configured allocation limit,
which defaults to 2 GiB.

### Results

Each workload retains 100 journals. The heap columns are median peak requested
bytes. Within each group, minimum, median, and maximum requested-byte counts
were identical across the five runs.

| Proposals | Initial voting outpoints per proposal | Changes per block | Full-snapshot heap bytes | Compact-undo heap bytes |
|---:|---:|---|---:|---:|
| 32 | 1,000 | None | 314,580,168 | 4,661,768 |
| 32 | 1,000 | One stake transfer | 307,576,104 | 4,634,202 |
| 128 | 1,000 | One stake transfer | 1,057,144,672 | 15,711,570 |
| 32 | 1,000 | One stake transfer and 100 new votes | 1,478,791,800 | 33,117,466 |
| 128 | 10,000 | One stake transfer and 100 new votes | Not run | 187,560,954 |

The 32-proposal voting-churn workload fell from about 1.38 GiB to 31.58 MiB.
The larger compact-only workload peaked at 178.87 MiB. The improvement comes
from removing repeated copies of unchanged governance data from retained
journals and reorg backups.

The highest observed RSS per run varied within these ranges:

| Workload | Full-snapshot RSS KiB, median and range | Compact-undo RSS KiB, median and range |
|---|---:|---:|
| 32 proposals, quiet | 317,928; 317,764 to 317,932 | 7,364; 7,288 to 7,476 |
| 32 proposals, stake transfers | 310,976; 310,920 to 310,980 | 7,380; 7,304 to 7,432 |
| 128 proposals, stake transfers | 1,056,988; 1,056,924 to 1,057,056 | 18,408; 18,332 to 18,516 |
| 32 proposals, voting churn | 1,508,248; 1,508,200 to 1,508,368 | 36,360; 36,316 to 36,480 |
| 128 proposals, 10,000 voters, voting churn | Not run | 190,668; 190,620 to 190,748 |

Every compact workload was below the probe's 256 MiB component ceiling. Every
completed full-snapshot workload exceeded it. A whole-node load test is still
needed to account for the other allocations and validate total RSS. That test
has since been run; see `docs/governance-wholenode-load-test.md`.

### Reproduce

From the workspace root:

```bash
cargo build -p vtorrent-node --release --example governance_memory
python3 scripts/governance-memory.py
python3 scripts/governance-memory.py --stake-transfers
python3 scripts/governance-memory.py --proposals 128 --stake-transfers
python3 scripts/governance-memory.py --stake-transfers --votes-per-block 100
python3 scripts/governance-memory.py --proposals 128 --voters 10000 --stake-transfers --votes-per-block 100 --compact-only
```

## Verification and remaining decisions

The all-features workspace tests, strict workspace Clippy, and format check
passed after the repair. Node results included 189 unit tests and 12 governance
regressions. All 18 daemon integration tests also passed.

The compact-undo regression restores proposal insertion, counted votes, locks,
and voter-identity cleanup. Existing block-path tests verify invalid-signature
and commitment rejection, successful and failed reorgs, activation boundaries,
and replay.

Activation still requires agreement on the permanent 100 VTR proposal burn,
parameter and timing policy, an independent review, and whole-node governance
load measurements. This follow-up does not schedule activation or change the
running fleet.
