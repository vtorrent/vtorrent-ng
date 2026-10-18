# Real-daemon proof quota expiry and automatic recovery

`daemon_proof_quota_expires_and_automatic_retry_completes` connects the
production admission budget, retry scheduler, P2P transport, and proof cache
in one real-time scenario. It uses two release-mode daemons and a separate
injecting peer on loopback. No quota limits, retry periods, or monotonic clocks
are overridden.

The initial scenario passed without a functional production repair. The
only node-source additions are test-util request and quota-denial logs.

## Scenario

Both daemons start from the same persisted chain at height 102, with empty
proof caches. The fixture includes the full legacy genesis UTXO snapshot,
two governance proposals, signed votes, and genuine PoS blocks after the
funding block. Governance uses the existing regtest activation override.

The injecting peer requests all 100 proofs at heights 3 through 102 from the
provider. Every response must match its original producer proof. The provider
must log one successful reconstruction for each distinct hash. The drain
must finish within 45 seconds, before the earliest 60-second admissions can
expire.

The peer then injects a new valid PoS block at height 103 without supplying
its proof. The provider accepts and relays the block to the receiver. The
injecting peer leaves the provider's own proof requests unanswered, so it
cannot populate the provider's cache with the expected test proof.

The receiver requests the new proof from the provider. Both connections use
the same source IP, `127.0.0.1`, so the receiver shares the exhausted per-IP
allowance. Test-only provider logs must confirm a budget denial and an
unavailable reconstruction result. Both caches must remain empty for the
target at this point.

The test waits for the receiver's ordinary event loop to retry. Its request
must arrive after the real 65-second delay, when the earlier admissions have
expired. The provider reconstructs the target, and both daemons must expose
the exact original proof through their cache-only RPC endpoint.

After recovery, observation continues for another 67 seconds, beyond the
next possible retry deadline. The provider must have received exactly two
target requests overall: one denied initial request and one successful
retry. It must have reconstructed the target exactly once. Both daemons then
shut down gracefully, and their persisted block, UTXO, governance, parameter,
supply, and stake state must match clean replay through height 103.

## Recorded result

The final recorded run passed with:

| Observation | Result |
| --- | --- |
| Successful cold admissions before injection | 100 |
| Time to drain the allowance | 11.927 seconds |
| Initial target denial after injection | 0.316 seconds |
| Exact target proof cached after injection | 66.332 seconds |
| Target observation ended after injection | 133.378 seconds |
| Provider target requests | 2 |
| Target budget denials | 1 |
| Target reconstructions | 1 |

The 66.332-second recovery is an end-to-end observation, not a throughput
benchmark. It includes the 65-second retry delay, maintenance polling, P2P
processing, reconstruction, and RPC sampling. It is not a latency guarantee.

Artifacts are retained at
`/tmp/opencode/vtr-proof-budget-recovery-AawdVb/`:

- `report.json` contains the measured phase times and asserted counts.
- `provider/daemon.log` and `receiver/daemon.log` contain the daemon logs.
- Each daemon directory contains the persisted `chain.db` used for replay
  comparison after shutdown.

The target hash is
`a9200d8b909d4d7360f7f2c3d9bea2df3b838d999be2ddf4c6747f9f66d6e678`.

## Reproduce

The test is ignored by default and requires a release build because it
replays more than 100 full-snapshot blocks and waits through two real retry
intervals. It retains temporary artifacts on success or failure and prints
their directory before launching the daemons.

```bash
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery daemon_proof_quota_expires_and_automatic_retry_completes \
    -- --ignored --nocapture
```

Allow at least three minutes for compilation and execution. The recorded
test itself took about 158 seconds. Test-util logging records decoded proof
requests and budget denials. Those extra log statements are absent without
`test-util` and do not change admission or retry behavior.

The ordinary release recovery suite also passed with 22 tests and two
manual tests ignored. The ignored recovery scenario above was run
separately. Workspace formatting and strict Clippy passed with all features
and with the daemon's default features.

## Limits

This scenario exhausts one per-IP allowance, not the process-wide limit.
The daemons stay connected on loopback throughout recovery. Global
exhaustion, address normalization, and disconnected-peer handling have
separate focused tests. This scenario does not measure Internet latency,
shared-NAT fairness, sustained cache churn, or restart recovery of the
in-memory retry queue.

It does not enable SPV PoS header acceptance or schedule governance activation.
