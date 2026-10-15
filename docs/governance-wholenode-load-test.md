# Governance whole-node load test

This test measures a real daemon's memory under an active governance workload.
It follows the component-level measurements in
`docs/governance-preactivation-validation-2026-10-12.md`, which covered the
governance state and undo journals but not the rest of the node.

Governance remains unscheduled and inactive on the current network.

## What it exercises

The driver `vtorrent-node/examples/governance_load.rs`:

1. Spawns a regtest daemon built with the `test-util` feature and the
   `VTR_GOVERNANCE_TEST_ACTIVATION` override, so governance rules run from
   height 2.
2. Imports a staking key, unlocks the wallet, and starts staking.
3. Funds a proposer, fee payers, and voters through the faucet.
4. Waits for the fast-regtest 60-second minimum stake age.
5. Broadcasts a signed proposal, waits for a staked block to confirm it, then
   broadcasts signed votes batched into relayed transactions.
6. Verifies the tallies and locks through the authoritative governance RPC.
7. Samples the daemon's `VmRSS` from `/proc/<pid>/status` every two seconds
   throughout, including a tail period after the votes confirm.

The workload uses the production paths: the public broadcast endpoint admits
transactions through `Mempool::admit_with_chain_fee`, the staking loop includes
them in a stake block, and `Chain::add_block` applies the governance rules and
journals the undo record.

## Results

One run, 600 voters, 50 votes per relayed transaction (12 transactions):

| Metric | Value |
|---|---|
| Voters (distinct voting outpoints) | 600 |
| Vote transactions | 12 |
| Counted Yes stake | 120,000,000,000 sat (600 × 2 VTR) |
| Locks | 600 |
| Proposal inclusion height | 615 |
| Staked blocks during the run | 5 (heights 615–619) |
| Rejected blocks | 0 |
| Total duration | 940 s |
| RSS after funding, before votes | 101,532 KiB |
| Peak RSS | 115,156 KiB |
| Final RSS | 115,156 KiB |

A four-voter smoke run peaked at 111,060 KiB, confirming the flow end to end
before the full run.

The governance workload added roughly 13 MiB over the post-funding baseline.
Peak RSS stayed below the 138–166 MiB the soak fleet observed and well below a
256 MiB component budget.

## Caveats

- One run. The component probe repeated its runs; this whole-node run did not.
  Treat the number as a single observation, not a distribution.
- Single node, no P2P peers. Peer connections, block relay, and the compact
  block path are not exercised.
- Regtest with fast-stake. Mainnet block cadence and stake ages differ.
- RSS is read from `/proc/<pid>/status`; it includes allocator and native
  allocations but is a point-in-time sample, not a peak-tracking allocator.
- Staking cadence varied: the first staked block took about ten minutes to
  appear, then blocks came every ~64 seconds. The vote confirmation time
  (667 s) is dominated by that wait, not by governance processing.

## Mempool dust exemption

Votes carry their payload in an OP_RETURN output. Consensus rejects zero-value
outputs, so a vote carrier holds one satoshi, which is burned because data
carriers never enter the UTXO set. The mempool dust rule previously rejected
any output below 546 satoshis, which would have rejected every vote. The rule
now exempts outputs that classify as `OpReturn`, using the same classification
as `block::is_utxo_eligible`, so an exempt output can never become a dust UTXO.
A regression test covers both the rejection of a normal dust output and the
acceptance of a sub-dust data carrier.

## Fixed: same-block proposal and vote ordering

A vote requires `height > proposal.created_height`, so a block that contains
both a proposal and its vote is invalid in either order: vote-first fails with
"unknown governance proposal", proposal-first fails with "voting window is
closed". A staker holding both in its mempool would build a block its own
validation rejects, then retry with the same transactions until they were
evicted — a liveness stall. No invalid state was ever accepted.

Block assembly now uses `Chain::select_block_transactions`. It tentatively
applies due activations, the selected coinstake, and candidate transactions using
the same governance and transaction transitions as validation. Rejected
candidates roll back their own changes before selection continues. The selector
restores the chain's pre-selection state before the staking engine constructs
the final block and proof.

A vote whose proposal is not yet recorded is excluded from the current
template. It can be selected later if the proposal confirms and the voting
outpoint remains mature and unspent. Selection accounts for earlier spends,
vote locks, and the coinstake consuming an outpoint. It also applies the block
size limit before processing each candidate.

Two tests cover this: `same_block_proposal_and_vote_is_rejected` records the
hazard, and `block_assembly_defers_votes_for_unrecorded_proposals` verifies the
filter keeps the proposal, drops the vote, and yields an accepted block.
The broader regression coverage and the correction to the original
governance-only filter are in `docs/governance-template-selection-review.md`.

## Restart rebuilds governance state

The driver's `--restart` flag kills the daemon after the votes confirm, starts
it again on the same data directory, and compares the proposals and locks from
the governance RPC before and after. A four-outpoint run reported
`restart_verified: true`: replay rebuilt identical proposals and locks.

This exposed a gap in the test seam. The daemon previously applied the
`VTR_GOVERNANCE_TEST_ACTIVATION` override after the chain load, so replay ran
with governance inactive and rebuilt no governance state. Production replay is
correct because `governance_active` reads the deterministic deployment, but the
seam did not reproduce it. The store now applies the override before replay
through `load_into_fast_regtest_chain_with_cache_and_governance`.

Red-check: with the override applied after the load, the same run failed with
`governance state changed across restart: proposals_match=false
locks_match=false`. With the override applied before replay, it passes. The
first run succeeds in both cases, so the failure isolates the restart path.

## Reproduce

```bash
cargo build -p vtorrent-daemon --release --features test-util
cargo build -p vtorrent-node --release --example governance_load
target/release/examples/governance_load \
    --daemon target/release/vtorrent-daemon \
    --voters 600 --votes-per-tx 50 --tail-secs 200
```

The driver writes the daemon's output to `<data-dir>/daemon.log` and prints a
JSON report. Add `--restart` to run the restart check.

By default the driver creates a fresh temporary data directory and removes it
when the run ends. If you pass `--data-dir`, the directory must not exist or
must be empty; the driver never deletes existing files. It fails before
starting the daemon when either port is already bound, so it cannot silently
talk to another process such as a soak-fleet docker proxy. The default ports are
22825 (RPC) and 22826 (P2P).

## Remaining follow-ups

The whole-node measurement is one input. The deposit, parameter, and timing
policy is approved (see `docs/governance-step3-design.md`). Activation still
requires an independent protocol review and scheduling the deployment. This
test does not schedule activation or change the running fleet.

Still open on the load path:

- Multiple proposals and voting waves in one run, including overlapping locks.
- A reorg load scenario. The restart check covers replay; it does not exercise a
  competing branch.
