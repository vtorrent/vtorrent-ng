# Governance competing-branch daemon test

This scenario complements the
[multi-proposal staking run](governance-multiproposal-load-test.md). Two real
daemons exchange competing branches over loopback P2P. The receiving daemon
must undo abandoned votes, restore the correct lock durations, persist the
winning branch, and recover after restart.

## Branches and expected state

Both branches share a funding block at height 1 and two proposals at height 2.
The fixture contains 32 voting outpoints, each holding 2 VTR. Proposal 1 closes
at height 4000 and proposal 2 at height 4100.

The old branch votes all 32 outpoints on both proposals at heights 3 and 4. The
winning branch votes all 32 on proposal 1, then only the first 16 on proposal 2.
An empty block at height 5 makes it longer than the old branch.

| State | Proposal 1 Yes | Proposal 2 Yes | Short locks, expiry 4006 | Long locks, expiry 4106 |
|---|---:|---:|---:|---:|
| Old branch, height 4 | 64 VTR | 64 VTR | 0 | 32 |
| Winning branch, height 5 | 64 VTR | 32 VTR | 16 | 16 |

The first daemon starts on the old branch. A second daemon starts on the
winning branch and connects to it as an explicit seed. The test verifies that
the first daemon's PID remains unchanged, its tip becomes the winning tip, and
its log reports `Reorg: fork point at height 2`.

RPC assertions check both proposal records by ID, including tallies, quorum
supply, and outcome fields. Every voting outpoint must have the expected owner,
proposal ID, expiry, and remaining duration. All six consensus parameters are
checked. The abandoned vote transaction must no longer be reported confirmed;
it may be absent or unconfirmed in the mempool.

## Persistence and restart

After the live reorg, the test appends a faucet descendant at height 6 and waits
for persistence. It then terminates the receiving daemon using SIGTERM or
SIGKILL, depending on the case.

Before restarting, it opens the store and checks:

- Canonical blocks at every persisted height match the winning history.
- Raw non-genesis UTXOs exactly match the expected chain.
- Replay with the test activation enabled reconstructs the complete
  `GovernanceState`, UTXO set, and parameters.

The restarted daemon must retain the same tip and expose the expected proposal
records, mixed lock durations, and parameters through RPC. This covers a kill
after confirmed persistence. It does not cover interruption during reorg writes.

## Observations

The release test run used the `test-util` feature and the normal regtest stake
age, not fast-stake mode. Both termination cases passed. The complete release
process-recovery suite passed all 20 tests.

| Termination after persistence | RSS before peer connection, KiB | Observed reorg peak RSS, KiB | Samples |
|---|---:|---:|---:|
| SIGTERM | 63,228 | 85,808 | 25 |
| SIGKILL | 63,532 | 86,200 | 25 |

RSS was sampled from the receiving daemon every 20 ms while the peer connected
and the branch switched. These are two functional scenarios, not repeated
performance measurements or a capacity bound. Samples can miss shorter peaks.

## Scope

The fixture reuses the 32-outpoint, two-proposal workload shape. Blocks contain
genuine owner-signed governance votes and fee-input signatures. It uses
regtest-only PoW-style blocks with historical timestamps so the branch test
does not wait for live staking intervals. The prior multi-proposal run exercises
real staking and mempool broadcast; this scenario exercises P2P branch delivery,
live reorganization, storage reconciliation, and restart.

This does not validate a proof-of-stake fork or stake-proof relay. Both
proposals remain open. No production deployment is scheduled by this test.

The subsequent PoS fork and cold-cache proof-serving scenario is recorded in
`docs/governance-pos-fork-proof-test.md`.

## Reproduce

The tests require Linux for RSS sampling and the daemon's `test-util` feature
for the activation override:

```bash
cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery governance_reorg -- --nocapture
cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery --quiet
```

The implementation is in
`vtorrent-daemon/tests/governance_reorg/mod.rs`. Each case uses temporary stores,
ephemeral loopback ports, and the existing daemon recovery helpers. Strict
workspace Clippy, formatting, and dependency-use checks passed.
