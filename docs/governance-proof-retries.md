# Bounded retries for recent canonical stake proofs

The node now retries recent stake-proof requests after an unavailable or
missing response. This complements the
[cold reconstruction admission budget](governance-proof-reconstruction-budget.md).
The wire response still carries no distinction between a missing proof and
quota exhaustion.

## Scheduling policy

| Setting | Value |
| --- | --- |
| Pending targets | At most 100 |
| Eligible history | Last 100 canonical block heights |
| Delay before each retry | 65 seconds |
| Total attempts per tracked target | Four, including the initial request |
| Maximum tracked lifetime | Five minutes |
| Retry batch | At most eight requests |
| Minimum gap between maintenance batches | One second |

The retry delay is derived from the 60-second reconstruction budget window
plus five seconds. With no scheduling delay, requests occur at the initial
attempt and approximately 65, 130, and 195 seconds later. After the final
attempt, the entry gets one response-wait interval before removal. The
five-minute lifetime also applies when there are no connected peers.

Only accepted canonical PoS blocks and applied PoS fork blocks create
requests. Cached proofs skip initial requests. Duplicate targets do not
reset attempts or extend the lifetime. When a full queue needs another
entry, it removes the lowest-height target.

Maintenance runs from the existing peer-event poll, at most once per second.
It does not create another timer that resets the 100 ms peer-event sleep.
The next maintenance deadline is set after dispatch completes, so a delayed
loop does not generate catch-up batches.

## Responses, peers, and cancellation

A valid canonical proof is cached and completes the pending request. An
unavailable, malformed, or invalid proof response does not complete it.
No response is required to make an attempt eligible for retry after the
delay. Unsolicited or late unavailable responses never create pending work.

Retries prefer the original source while it remains connected. If that
source is gone, retries cycle through connected, handshaken peers using
each connection's current wire version. With no eligible peers, the node
does not consume attempts, but the lifetime still expires.

Before dispatch, maintenance removes cached, abandoned, expired, exhausted,
and aged-out targets. The canonicality and cache checks acquire the chain
guard before the proof-cache guard. Neither guard is held during network
sends. A reorg after dispatch is still possible. The existing final proof
binding check rejects a response for an abandoned block.

## Regression evidence

`unavailable_proof_is_retried_over_p2p_and_cached_after_response` uses a real
loopback P2P connection and protocol-v3 handshake. A scripted peer returns
`proof: None` to the initial request, then the original valid proof to a
retry. The receiver must cache the exact proof and clear pending work.
Cached targets and late or unsolicited unavailable responses must not reopen
the queue.

With request tracking disabled to reproduce the previous one-shot behavior,
the test failed at:

```text
retry after unavailable proof: peer did not receive the expected proof request
```

With tracking enabled, it passed. The test advances the retry maintenance
method's monotonic timestamp instead of sleeping for 65 seconds. It exercises
real messages and the production response handler, but does not measure a
65-second wall-clock wait or exhaust a real remote server's quota. Budget
expiry and admission are covered by the separate budget regressions.

Queue tests cover retry deadlines, attempt exhaustion, offline expiry,
fallback rotation, source reconnection, duplicate requests, queue capacity,
and batch bounds. The existing reorg-during-reconstruction test now also
checks that an abandoned target leaves the retry queue.

All 18 proof-related debug tests passed. The complete all-features workspace
suite passed in release mode, including 222 node unit tests, 22 daemon
recovery tests, and 141 RPC tests. Workspace formatting and strict Clippy
passed with all features and with the daemon's default features.

```bash
cargo test -p vtorrent-node --lib proof -- --nocapture
TMPDIR=/tmp/opencode cargo test --workspace --all-features --release --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p vtorrent-daemon --all-targets -- -D warnings
```

## Limits

Retries are best-effort and bounded. Persistent quota pressure or unavailable
proof data can exhaust all attempts. The queue is in memory, resets on
restart, and does not backfill historical proofs at startup. It targets
recent canonical blocks rather than an archival proof service. This change
does not enable SPV PoS header acceptance or schedule governance activation.
