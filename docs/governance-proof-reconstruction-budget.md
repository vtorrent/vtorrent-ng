# Stake-proof reconstruction admission budget

Cold-cache P2P proof reconstruction now has a process-wide limit and a
per-IP limit. Cached proof serving bypasses these limits. The existing
general P2P message limits still apply to every `getproof` request.

## Policy

| Limit | Maximum admissions in the preceding 60 seconds |
| --- | --- |
| One source IP | 100 |
| Entire node process | 200 |

The per-IP allowance admits a complete normal 100-block undo window when
the budget is unused. The process allowance permits two such bursts across
different IPs. These are initial request-count limits, not CPU-time limits.
Proof cost still depends on UTXO size and retained changes.

The policy lives in `vtorrent-node/src/node/proof_service.rs`. It uses
monotonic `Instant` timestamps and a sliding window. Each accepted entry
expires 60 seconds after its own admission. Denied requests do not extend
deadlines or consume another source's remaining allowance.

The key is the source IP rather than its TCP port. Disconnecting or
reconnecting does not clear the allowance. IPv4-mapped IPv6 addresses share
the corresponding IPv4 bucket. Peers behind the same source IP share a
bucket. State is local to the process and resets on restart.

## Admission and response

`Node::get_stake_proof` first checks the shared cache. On a miss it acquires
the chain guard and checks canonical height, PoS header type, and retained
undo history without loading a block body or copying UTXOs. Ineligible
targets do not consume reconstruction allowance.

For an eligible target, both budgets must admit the attempt before snapshot
capture. The charge remains if a later body lookup, reconstruction, or
canonicality check fails. Journal replay and Merkle construction still run
outside the chain guard, as described in
[the snapshot implementation](governance-proof-snapshot-reconstruction.md).
Cache hits and accepted worker results still pass the final canonical proof
binding check.

Budget exhaustion returns the existing P2P `ProofMsg` with `proof: None`.
It does not ban the peer or change the wire version. That response does not
distinguish quota exhaustion from an unavailable proof. A peer can retry
after older admissions expire. The budget layer itself adds no retry
scheduler. The subsequent [bounded retry layer](governance-proof-retries.md)
handles recently requested canonical proofs.
Cold proof availability remains best-effort, including during a large reorg
or after earlier requests have consumed the allowance.

## Bounded tracking

Only admitted attempts create tracking entries. Each timestamp in an IP
queue corresponds to one timestamp in the global queue. The implementation
prunes expired entries and removes empty IP queues before admission.
The global cap therefore bounds live tracking to 200 IP entries and 400
accepted-request timestamps across both levels. Rejected requests from new
IPs cannot grow that state.

## Verification

The focused exhaustion regression initially failed with:

```text
exhausted admission must not reconstruct a cold proof
```

After enforcement, the service test rejects a cold request after exhaustion,
serves an exact cached proof, rejects a cold request after a same-IP port
change, permits another IP while global allowance remains, and rejects a
third IP once the global limit is reached.

Other regressions cover unknown and out-of-window targets, PoW headers,
IPv4-mapped aliases, per-IP isolation, sliding expiry boundaries, rotating
IPs, and tracking cleanup. All 13 proof-related debug tests passed. The
complete release-mode all-features workspace suite passed, including 217
node unit tests, 22 daemon recovery tests, and 141 RPC tests. Formatting and
strict Clippy passed with all features and with daemon default features.

The real-daemon cost workload also passed with the default budget enabled:
five runs each at 1, 4, and 8 peers, verifying 240 cold and 240 warm proof
responses. Every case began with a fresh process and requested 16 distinct
proofs, so this is a serving-compatibility check, not a quota-exhaustion load
test or a new before-and-after performance comparison.
Its raw report is `/tmp/opencode/vtr-proof-budget-20261018.json`.

The later [real-daemon recovery test](governance-proof-quota-recovery.md)
exhausts a provider's per-IP quota and verifies automatic recovery after
the actual retry delay.

```bash
cargo test -p vtorrent-node --lib proof -- --nocapture
TMPDIR=/tmp/opencode cargo test --workspace --all-features --release --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p vtorrent-daemon --all-targets -- -D warnings
TMPDIR=/tmp/opencode VTR_PROOF_COST_REPORT=/tmp/opencode/vtr-proof-budget-new.json \
    cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery canonical_proof_request_cost -- --ignored --nocapture
```

The UTXO snapshot copy remains under the chain mutex. The limits bound
admission counts, not individual lock time, wall time, or total CPU time.
