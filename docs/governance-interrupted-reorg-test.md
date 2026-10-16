# Governance recovery during interrupted PoS reorg writes

`governance_pos_reorg_recovers_at_every_daemon_write_boundary` in
`vtorrent-daemon/tests/governance_reorg/mod.rs` kills the receiving daemon
at six persistence boundaries during a live PoS branch switch.

The test uses the two-proposal, 32-outpoint fixture from
[the PoS fork proof-relay test](governance-pos-fork-proof-test.md).
The common ancestor is height 2. The old branch ends at height 4, and the
winning branch ends at height 5. All blocks after the funding block are
genuine PoS blocks. Governance uses the regtest-only activation override.

## Crash points and expected disk state

The daemon's event bridge pauses before persistence or after a completed
`rollback_tip` or `append_block` call. The test waits for the pause log,
sends SIGKILL, and waits for process exit before opening the store.

| Completed writes | Last operation | Expected persisted tip |
| --- | --- | --- |
| 0 | No persistence operation | Old height 4 |
| 1 | Roll back old height 4 | Old height 3 |
| 2 | Roll back old height 3 | Common height 2 |
| 3 | Append winning height 3 | Winning height 3 |
| 4 | Append winning height 4 | Winning height 4 |
| 5 | Append winning height 5 | Winning height 5 |

Each case starts from a separate copy of the old branch. The test checks
the raw store before any startup repair can mask a persistence error.
Stored block bodies, the tip, and non-genesis UTXOs must match the expected
prefix. No block may remain above that tip, and the fixture has no claimed
addresses.

Store replay must reproduce the complete UTXO set, governance state,
consensus parameters, total supply, and total staked amount. The test then
starts the daemon without a peer and checks its tip, proposal records,
tallies, and per-outpoint vote locks through RPC.

After another restart, a winning peer connects over loopback P2P. The
receiver must reach the winning tip with 64 VTR Yes on the shorter proposal,
32 VTR Yes on the longer proposal, and 16 locks at each expiry height,
4006 and 4106. A faucet descendant at height 6 forces a subsequent write.
The test checks the store again, rejects old-branch transactions from the
canonical replay, and verifies one final daemon restart.

## Fault-injection hook

`VTR_TEST_REORG_PAUSE_AFTER` selects the number of completed reorg store
operations before the event bridge waits indefinitely. The hook exists only
with the daemon's `test-util` feature and rejects use without `--regtest`.
The recovery harness removes inherited hook settings and supplies the
requested boundary only to the receiver that it will kill.

The hook pauses the actual daemon event bridge. The older test in
`vtorrent-daemon/tests/interrupted_reorg/mod.rs` instead kills a separate
writer subprocess that calls the store APIs.

## Verification

The targeted scenario passed all six boundaries. The complete release-mode
`process_recovery` suite also passed with 22 tests.

```bash
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery governance_pos_reorg_recovers_at_every_daemon_write_boundary -- --nocapture
TMPDIR=/tmp/opencode cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery -- --nocapture
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p vtorrent-daemon --all-targets -- -D warnings
```

## Limits

The test interrupts the sequence between committed store operations. It does
not interrupt a redb transaction internally, simulate power loss, or inject
I/O failures. Both proposals remain open, so these cases do not test a crash
at proposal decision or parameter activation. Proof reconstruction request
cost remains unmeasured. Governance activation remains unscheduled.
