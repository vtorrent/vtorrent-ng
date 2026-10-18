# Stake-proof reconstruction outside the chain lock

The snapshot implementation reduces chain-lock ownership and eight-peer RPC
tail latency in the committed proof-request workload. It does not reduce
proof batch wall time or eliminate reconstruction CPU cost.

## Reconstruction and final canonicality check

`Chain::stake_proof_snapshot` captures the canonical target block, its parent
header, a copy of the current UTXO map, and the required UTXO undo changes.
It verifies canonical height and contiguous retained history before copying
the full map. Requests outside the retained undo window still return no proof.

The P2P handler drops the chain guard and calls
`StakeProofSnapshot::reconstruct` through `tokio::task::spawn_blocking`.
The worker reverses journals, hashes UTXOs, builds the Merkle trees, and
checks the proof against the captured block and parent commitments. It owns
its inputs and does not access live chain state.

Before a reconstructed or cached proof can be returned, the handler calls
`Node::cache_canonical_stake_proof`. This method checks the target against
the live canonical chain and validates the complete proof binding. It holds
the chain guard through cache insertion so a reorg cannot occur between the
check and insertion. A requested orphan cache entry is removed. Invalid
relayed data for a canonical block does not evict an existing valid proof.
The received-proof handler uses the same final-check method.

Canonicality is established at that final check. A later reorg can still
occur before the network response arrives. The wire format and consensus
acceptance rules are unchanged.

## Race regressions

`reorg_during_reconstruction_rejects_result_and_evicts_cached_orphan` pauses
a blocking worker after snapshot capture, applies a genuine longer PoS fork
to the live chain, and releases the worker. The reconstructed old proof is
still identical to its original. The final check must reject that proof,
remove the cached orphan, and accept a proof for the winning branch.
The version without the final check failed with:

```text
a reconstruction from the abandoned branch must not be served
```

`invalid_relay_does_not_evict_a_valid_canonical_cached_proof` protects the
cache from invalid replacement data. An initial implementation removed the
existing proof when rejecting any invalid candidate. The regression failed
with `left: None` against the expected cached proof. Eviction now requires
that the target block is no longer canonical.

The four existing proof regressions also pass after migration to the
snapshot API. They cover exact reconstruction without mutation, missing
parent bodies, incorrect Merkle or stake fields, and missing undo history.

## Interleaved comparison

The comparison uses the same 59,430-UTXO, 16-proof workload described in
[the baseline experiment](governance-proof-request-cost.md). For each of
five repetitions at 1, 4, and 8 peers, the harness alternates the baseline
daemon at `f3850b1` and the snapshot daemon. Every case starts a fresh
process and verifies cold cache state before the cold and warm phases.
All 960 proof responses matched the original proofs, with zero errors.

Both daemons use release optimization, `test-util`, Rust 1.90.0, and the same
workspace `Cargo.lock`. The first baseline install resolved newer
dependencies and was discarded. `scripts/proof-cost-baseline.py` rebuilt
the measured baseline from `f3850b1` with the workspace lock and `--locked`.

The final comparison ran on Linux x86_64 with 24 logical CPUs. Host load
averages were 1.40, 0.60, and 0.43 before the build and experiment, and 4.31,
2.03, and 0.98 afterward. Other host workloads remained active. Interleaving
keeps adjacent baseline and snapshot cases close in time.

| Peers | Baseline lock median, ms | Snapshot capture median, ms | Baseline cold RPC p95, ms | Snapshot cold RPC p95, ms |
| --- | --- | --- | --- | --- |
| 1 | 17.173 | 3.431 | 0.504 | 0.556 |
| 4 | 17.211 | 3.368 | 0.526 | 1.657 |
| 8 | 17.026 | 3.514 | 37.607 | 2.626 |

Lock medians pool 80 captures per peers-count group and variant. Baseline
`hold_us` covers full reconstruction under the guard. Snapshot `hold_us`
covers capture only. It excludes the later canonicality and cache-insertion
guard. The largest snapshot capture was 10.709 ms across all cases.

RPC p95 values are medians of five per-run percentiles. At eight peers,
baseline per-run p95 ranged from 32.727 to 38.376 ms. Snapshot per-run p95
ranged from 2.619 to 3.500 ms, with no overlap. The largest sampled cold RPC
response at eight peers fell from 79.264 to 7.837 ms.

Four-peer p95 rose from 0.526 to 1.657 ms. Its largest sampled cold RPC
response fell from 77.481 to 10.606 ms. The single-flight sampler can make
more requests while the worker builds a proof, changing the sampled latency
distribution. These observations do not establish a latency improvement
for every concurrency level or percentile.

| Peers | Baseline cold batch median, ms | Snapshot cold batch median, ms |
| --- | --- | --- |
| 1 | 1875.3, range 1857.9 to 1886.6 | 1887.3, range 1869.5 to 1891.4 |
| 4 | 666.0, range 661.4 to 674.9 | 663.8, range 656.1 to 674.1 |
| 8 | 459.4, range 455.9 to 471.0 | 459.8, range 456.6 to 464.1 |

Batch ranges overlap. There is no measured batch-throughput improvement.
At eight peers, both variants used a median 310 ms of daemon CPU per cold
batch at 10 ms accounting resolution. The existing 100 ms peer-event poll
and sequential dispatch still pace proof service. Awaiting the blocking
worker frees the chain mutex and Tokio workers, but it does not process
multiple peer messages concurrently.

## Reproduce

Prepare a clean baseline checkout and build it with the current workspace's
dependency lock. The helper rejects another revision or modified tracked
files and copies `Cargo.lock` only into the separate checkout.

```bash
git clone --no-hardlinks . /tmp/opencode/vtr-proof-baseline-source
git -C /tmp/opencode/vtr-proof-baseline-source checkout --detach f3850b1
python3 scripts/proof-cost-baseline.py \
    /tmp/opencode/vtr-proof-baseline-source /tmp/opencode/vtr-proof-baseline-pinned-new
TMPDIR=/tmp/opencode \
VTR_PROOF_COST_BASELINE_DAEMON=/tmp/opencode/vtr-proof-baseline-pinned-new/bin/vtorrent-daemon \
VTR_PROOF_COST_REPORT=/tmp/opencode/vtr-proof-comparison-new.json \
    cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery canonical_proof_request_cost -- --ignored --nocapture
python3 scripts/proof-cost-summary.py /tmp/opencode/vtr-proof-comparison-new.json
```

The final measured report is
`/tmp/opencode/vtr-proof-snapshot-comparison-20261017-final.json`.
Report names must be unused because the harness refuses to overwrite them.

## Verification

The complete all-features workspace suite passed in release mode, including
212 node unit tests, 22 daemon recovery tests, and 141 RPC tests. The manual
comparison is ignored by the ordinary suite and passed separately with both
variants. All six focused proof tests also passed in debug mode.
Workspace formatting and strict Clippy passed with all features and with
the daemon's default features.

```bash
cargo test -p vtorrent-node --lib stake_proof -- --nocapture
TMPDIR=/tmp/opencode cargo test --workspace --all-features --release --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p vtorrent-daemon --all-targets -- -D warnings
```

The unoptimized full-workspace attempt exceeded a 600-second terminal limit
during the recovery suite. That run did not complete and is not counted as
passing. The release-mode command above completed successfully.

## Remaining limits

The full UTXO copy still runs under the chain mutex. This snapshot change
added no reconstruction-specific request budget. A subsequent
[admission budget](governance-proof-reconstruction-budget.md) limits cold
reconstruction attempts. Neither change provides archival proofs
beyond retained history. The experiment covers a finite loopback burst
with one RPC sampler and no active staking. Sustained cache churn and larger
UTXO sets require separate measurements.
