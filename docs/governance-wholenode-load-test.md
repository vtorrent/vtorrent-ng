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

## Follow-up: same-block proposal and vote ordering

The driver confirms the proposal in one block before broadcasting votes, so
votes always reference a proposal already in the chain. A block that contains a
vote before the proposal it references is rejected, because
`GovernanceState::process_transaction` processes transactions in block order and
the vote finds no proposal yet. The staking loop includes mempool transactions
in arbitrary order, so a staker holding both a proposal and its votes could
build a block that its own validation rejects, then retry with the same order
until the transactions are evicted.

This is a liveness hazard, not a safety one: no invalid state is accepted. It
should be fixed before activation by ordering proposal transactions before vote
transactions during block assembly, or by excluding votes whose proposal is not
yet in the chain. It is recorded here rather than fixed because it touches the
staking loop and is outside this measurement's scope.

## Reproduce

```bash
cargo build -p vtorrent-daemon --release --features test-util
cargo build -p vtorrent-node --release --example governance_load
target/release/examples/governance_load \
    --daemon target/release/vtorrent-daemon \
    --data-dir /tmp/vtr-gov-load \
    --voters 600 --votes-per-tx 50 --tail-secs 200
```

The driver writes the daemon's output to `<data-dir>/daemon.log` and prints a
JSON report. It requires ports 22825 (RPC) and 22826 (P2P) to be free; the soak
fleet's docker proxies hold 22525/22526 and 22625/22626.

## Remaining activation decisions

The whole-node measurement is one input. Activation still requires agreement on
the permanent 100 VTR proposal burn, parameter and timing policy, an
independent protocol review, and the same-block ordering fix above. This test
does not schedule activation or change the running fleet.
