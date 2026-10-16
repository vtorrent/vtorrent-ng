# Canonical stake-proof request cost

Cold-cache reconstruction causes measurable RPC tail latency in a finite
multi-peer burst. This experiment measures the existing implementation.
It does not compare an optimization or establish production capacity.

## Workload and measurement

The ignored release-mode test `canonical_proof_request_cost` extends the
governance PoS fixture to height 18. The chain contains 59,430 UTXOs,
including the legacy genesis snapshot. Sixteen canonical PoS proofs at
heights 3 through 18 are the request targets.

For each of five repetitions, the harness runs 1, 4, then 8 loopback peers.
Every case starts a fresh daemon and verifies that all target cache entries
are empty. After all peer handshakes complete, a second barrier releases the
workers into the timed request phase. Each peer requests its disjoint share
of the 16 proofs, with one outstanding request per peer. The same number of
peers then request the proofs from the populated cache through new connections.
Connection setup is outside the timed phases.

Each response must match the original `StakingEngine` proof exactly. The
daemon must log exactly 16 successful reconstructions across both phases.
Missing proofs, incorrect responses, disconnects, and timeouts fail the run.
The completed experiment verified 240 cold and 240 warm responses with zero
errors.

One concurrent RPC client repeatedly requests `/api/v1/info`, checks the
canonical tip, and sleeps 1 ms between completed responses. These RPCs are
single-flight, not an open-loop latency test. The proof clients also poll
events with a 1 ms sleep when no proof is ready.

With `test-util`, the `getproof` handler records time waiting for the chain
mutex and time spent reconstructing while holding it. The timing ends after
the guard is dropped, before logging and cache insertion. No timing log is
compiled into the handler without `test-util`.

## Results

The final run used Linux x86_64, Rust 1.90.0, release optimization, and 24
available logical CPUs. Host load averages were below 2.0 before and after
the experiment. Other host workloads were not disabled.

The following batch times are medians of five runs. Ranges show the minimum
and maximum run times. Each batch contains 16 checked proof responses.

| Peers | Cold batch, ms | Warm batch, ms | Cold RPC p95, ms | Warm RPC p95, ms |
| --- | --- | --- | --- | --- |
| 1 | 1891.4, range 1877.6 to 1905.9 | 1611.9, range 1610.6 to 1617.2 | 0.507 | 0.455 |
| 4 | 647.5, range 642.8 to 682.1 | 397.9, range 396.2 to 400.4 | 0.501 | 0.478 |
| 8 | 443.4, range 442.2 to 455.6 | 196.9, range 194.1 to 199.3 | 35.290 | 0.452 |

RPC p95 columns are medians of the five per-run p95 values, not pooled
percentiles. At eight peers, cold per-run p95 ranged from 33.887 to 36.462 ms.
The largest cold RPC latency across all cases was 79.291 ms. Low p95 at one
peer does not mean reconstruction never blocks RPC. Its largest observed
cold RPC latency was 28.041 ms.

Across the 80 reconstructions in each peers-count group, median lock-hold
times were 19.319 ms at one peer, 16.359 ms at four, and 16.740 ms at eight.
The overall observed range was 14.200 to 31.503 ms. Time waiting to acquire
the mutex was at most 0.544 ms. This is the reconstruction handler's wait,
not the wait experienced by RPC requests.

Daemon CPU accounting used `/proc/<pid>/stat` with 100 ticks per second.
Median cold batch CPU times were 390, 300, and 300 ms for 1, 4, and 8 peers.
Warm batch CPU times were 50, 20, and 10 ms. These values include the RPC
sampling workload and have 10 ms resolution. Sampled RSS is present in the
raw report, but it is not an allocation measurement or a guaranteed peak.

## Interpretation

`Node::start` polls peer events after a 100 ms sleep and handles the returned
events sequentially in `vtorrent-node/src/node/mod.rs`. That explains the
roughly 100 ms warm request cadence and why more peers complete the fixed
batch sooner. Batch wall time is not pure reconstruction throughput.

On cache misses, `Chain::reconstruct_stake_proof` clones the UTXO map,
reverses retained journals, hashes the reconstructed UTXOs, and builds a
Merkle tree under the chain mutex. The direct lock timings establish that
this work blocks other chain users for tens of milliseconds per request.
Eight peers produce repeated waits visible in the RPC tail even though the
proof handlers themselves rarely wait for the mutex.
RPC response times include runtime scheduling and lock waits. The experiment
does not isolate those contributions.

The experiment uses only 16 distinct proofs inside the retained history.
Successful reconstructions stay cached. It does not test sustained cache
churn, a larger UTXO set, network latency, rate-limit exhaustion, or active
staking during proof requests. It sets no latency pass threshold and makes
no claim that the existing general message budget bounds reconstruction CPU
adequately. A reconstruction-specific budget or shorter mutex ownership
requires a separate implementation and before-and-after measurement.

## Reproduce

Use a new report filename because the harness refuses to overwrite an
existing report. The report includes all request and RPC samples, CPU ticks,
sampled RSS, and reconstruction timing records.

```bash
TMPDIR=/tmp/opencode VTR_PROOF_COST_REPORT=/tmp/opencode/vtr-proof-cost-new.json \
    cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery canonical_proof_request_cost -- --ignored --nocapture
python3 scripts/proof-cost-summary.py /tmp/opencode/vtr-proof-cost-new.json
```

The observed final report is
`/tmp/opencode/vtr-proof-cost-20261017-final.json`. The earlier exploratory
run is separate at `/tmp/opencode/vtr-proof-cost-20261017.json`.
