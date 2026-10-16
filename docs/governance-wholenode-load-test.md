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
3. Funds proposers, independent fee payers, and voters through the faucet.
4. Waits for maturity using the tip timestamp and active minimum stake age.
5. Broadcasts signed proposals, waits for confirmation, then submits voting
   waves. Alternate waves use ascending and descending proposal deadlines.
6. Verifies each proposal's tally by ID and each outpoint's longest lock through
   the authoritative governance RPC after every voting stage.
7. Samples the daemon's `VmRSS` from `/proc/<pid>/status` every two seconds
   throughout, with synchronous samples at phase boundaries and an optional tail.

The workload uses the production paths: the public broadcast endpoint admits
transactions through `Mempool::admit_with_chain_fee`, the staking loop includes
them in a stake block, and `Chain::add_block` applies the governance rules and
journals the undo record.

## Results

This section records the original single-proposal run. The later two-proposal,
two-wave run and its stronger restart check are recorded in
`docs/governance-multiproposal-load-test.md`.

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

The current `--restart` check stops staking, requests graceful SIGTERM shutdown,
and waits for the child to exit before restarting on the same data directory.
It compares the tip height and hash, parameters, proposal rows, and lock rows.
Derived `remaining_blocks` values are checked against the tip before being
excluded from the normalized comparison. Both session logs are retained.

The original four-outpoint check used a forced kill and compared proposals and
locks. It reported `restart_verified: true`. That historical result does not
replace a governance crash-recovery or competing-branch load test.

This exposed a gap in the test seam. The daemon previously applied the
`VTR_GOVERNANCE_TEST_ACTIVATION` override after the chain load, so replay ran
with governance inactive and rebuilt no governance state. Production replay
uses the scheduled deployment. The store now applies the test override before
replay in both normal and fast regtest modes.

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

The driver appends both daemon sessions to `<data-dir>/daemon.log`, saves
`report.json`, and prints one JSON report. Add `--restart` for the restart check,
`--proposals 2` for two proposals with different deadlines, and
`--voting-waves 2` to divide the voters into two cohorts.

By default the driver creates a fresh temporary data directory and removes it
after a successful run. Failed runs retain artifacts and print their location.
If you pass `--data-dir`, the directory must not exist or must be empty; the
driver never deletes existing files. It checks port availability before each
spawn and checks child liveness during RPC readiness. The default ports are
22825 (RPC) and 22826 (P2P). Startup also checks the regtest network label.

## Remaining follow-ups

The whole-node measurement is one input. The deposit, parameter, and timing
policy is approved (see `docs/governance-step3-design.md`). Activation still
requires an independent protocol review and scheduling the deployment. This
test does not schedule activation or change the running fleet.

Still open on the load path:

- Larger repeated multi-proposal runs. The completed two-wave case uses 32
  outpoints and is one observation.
- Interrupted governance-reorg writes and historical proof availability beyond
  the undo window. The real-peer branch tests are recorded in
  `docs/governance-reorg-load-test.md` and
  `docs/governance-pos-fork-proof-test.md`.
