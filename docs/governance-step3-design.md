# Governance Step 3

Status: repaired after the [self-review](governance-step3-self-review-2026-10-11.md).
The implementation remains inactive on the current network.

## Activation

`deployments::governance_deployment()` is the shared source for consensus and
the deployments RPC. It currently returns `None`. Block application processes
governance only when the scheduled deployment reaches `Active`.

A completed signalling window is followed by a full `LockedIn` window before
activation. A test-only height override exercises active rules without
scheduling a production deployment.

## Authenticated voting

Option B uses an explicit mature UTXO at vote time. There is no historical
UTXO archive. Maturity means the UTXO's timestamp is at least the active
`min_stake_age` seconds before the voting block's timestamp. The default is
six hours. This is UTXO age, not a requirement to create a lock six hours before
voting.

`SignedVote` encodes a `VTRV2` payload with these fields:

| Field | Bytes |
|---|---:|
| Prefix | 5 |
| Proposal id | 8 |
| Choice | 1 |
| Voting transaction id | 32 |
| Output index | 4 |
| Recoverable owner signature | 65 |

The signature commits to the genesis hash, proposal, choice, and outpoint under
a governance-specific domain. A P2PKH owner authorizes their output. For P2CS,
the spending key authorizes the vote. The staking operator cannot vote with
the hot key. Compressed and legacy uncompressed owner keys are supported.

`claim_address` grants no voting authority. Active consensus rejects unsigned
`VTRV1` votes. `SignedVote::script()` constructs the 115-byte payload using
`OP_PUSHDATA1`; the generic 80-byte OP_RETURN builder is not used.

An outpoint contributes its value once per proposal. Duplicate votes reject
the block. Votes are valid after the proposal's creation block and strictly
before `voting_end`. The quorum denominator is the pre-block stakeable supply
at proposal creation. Turnout must reach 33%, and Yes must reach 66% of
non-abstain votes.

## Locks and staking

A counted outpoint stays locked through `voting_end + 6` blocks. Voting on a
second proposal extends the lock to the later expiry. Ordinary spends reject
while the lock is active.

A coinstake can consume a locked outpoint only if it returns all value in one
value-bearing output with the identical script. The lock moves to that output.
The original voting identity and principal remain unchanged, so staking and
reward accrual cannot multiply the original vote or its weight while locked.

Locks expire at their unlock height. Block application prunes expired locks.

## Proposals and parameter bounds

Proposals retain the `VTRG1` encoding. Their id is the first eight bytes of the
hash of their encoding and creation height.

The declared deposit must equal the value of the proposal's OP_RETURN output
and must be at least 100 VTR. The deposit is permanently burned on both pass
and failure. Refund-on-pass is not implemented. This replaces the earlier
unfunded deposit metadata and refund claim.

The voting window is 2,880 to 10,080 blocks. The minimum window is nominally
48 hours at the default 60-second target. Proposals choose their window within
these bounds. Activation is 2,016 to 20,160 blocks after voting closes.

| Parameter | Allowed value |
|---|---|
| Annual reward rate | 1 to 1,000 basis points |
| Reward age cap | 60 seconds to 365 days |
| Minimum stake age | 60 seconds to 365 days, no greater than maximum age |
| Maximum stake age | 60 seconds to 365 days, no less than minimum age |
| Minimum stake amount | 1 VTR to `MAX_SUPPLY` |
| Target block time | 30 to 600 seconds |

`MAX_SUPPLY` and fee policy are not votable parameters. Bounds and age
consistency are checked at proposal admission and again at activation.
These bounds do not guarantee liveness for every parameter choice and UTXO
distribution. Parameter policy remains part of the activation review.

## Deterministic activation and rollback

At the start of a block, consensus decides closed proposals and applies due
changes before validating transactions and stake rewards. Each activation runs
once, ordered by activation height, creation height, then proposal id.

Conflicting changes use that same order on every node. If an earlier change
makes a later candidate invalid, the later record gets `activation_failed` and
does not change the parameters.

`Chain.params` is the parameter source. The staking loop uses the next block's
parameters, including due activations. A minimum-stake-amount change recounts
the kernel denominator under the new threshold.

The block journal saves governance, parameters, supply, and stakeable supply
before mutation. Transaction failure, commitment mismatch, rollback, and
failed reorg restore that state with the UTXOs. Successful reorgs retain the
common ancestor's state and apply the replacement branch. Restart replay uses
the same block-application path.

Active journals currently retain governance snapshots. Memory usage under a
large voting workload needs measurement before scheduling activation.

## RPC and verification

`GET /api/v1/governance/proposals` returns accepted chain records, including
`applied`, `activation_failed`, and `quorum_supply`. It does not recalculate
historical votes using current balances. The locks and parameters endpoints
read the same accepted state. Proposal and vote construction UI remains a
separate task.

Regression checks are in `vtorrent-node/tests/governance_regression.rs` and
`vtorrent-node/src/chain/governance_tests.rs`. They cover owner authorization,
vote replay, overlapping locks, timestamp maturity, cold-stake ownership,
activation order, lock-preserving staking, rejection rollback, reorgs, and
block replay. Deployment tests cover the full lock-in window.

```bash
cargo test -p vtorrent-node --test governance_regression
cargo test -p vtorrent-node chain::governance_tests
cargo test --workspace --all-features --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
