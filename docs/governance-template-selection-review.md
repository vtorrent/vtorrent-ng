# Governance block-template selection

The governance-only filter introduced in `7f54346` deferred votes for unknown
proposals, but did not reproduce the full state that validation sees. This
review found and reproduced four defects against baseline `efc3903`.

## Reproduced failures

| Case | Expected result | Before repair |
|---|---|---|
| A batch has a valid vote followed by an unsigned vote, then a separate valid transaction votes from the same outpoint | Reject the batch and keep the separate transaction | Selection returned no transactions because the rejected batch left its first vote counted in the simulation |
| A stake-minimum change activates before a vote is evaluated | Exclude a vote below the new minimum | Selection used current parameters and kept the invalid vote |
| An earlier transaction spends the voting outpoint | Keep the spend and exclude the vote | Selection kept both transactions against unchanged pre-block UTXOs |
| The coinstake consumes the voting outpoint | Exclude the vote and build an accepted block | The assembled PoS block failed with `voting outpoint is not unspent` |

All four regressions failed before the repair. They now pass.

## Transactional selection

`Chain::select_block_transactions` replaces `includable_transactions`. The
staking loop supplies the actual coinstake chosen by kernel search, the
mempool's ordered candidates, the next height, and the candidate timestamp.

Selection uses the same block-start activation helper and governed transaction
application helper as `apply_block_journaled`. It performs these transitions:

1. Apply due parameter changes and recount the staking denominator if needed.
2. Validate and apply the coinstake. Regtest tests can supply a coinbase prefix.
3. Reject oversized candidates and extra coinbase or coinstake transactions
   before they reserve inputs or alter governance state.
4. Validate each remaining candidate against the evolving UTXO set and vote
   locks. Accepted transactions remain applied for subsequent candidates.
5. On rejection, restore the candidate's governance entries, UTXO changes,
   claims, and supply deltas. Later candidates see only accepted predecessors.
6. Restore the full pre-selection state before returning the selected list.

Candidate checkpoints contain the touched proposal metadata, newly counted
voting identities, and previous lock entries. They do not clone the entire
governance state per candidate. One temporary governance pre-state copy remains
for restoration of the overall selection attempt.

The selector applies the transaction-byte limit before each candidate. This
keeps simulation and the builder's size limit aligned. Candidates that do not
fit are skipped without consuming inputs or voting identities.

Selection also validates ordinary transactions when governance is inactive.
The staking loop no longer filters against pre-block inputs before selection,
so a child can use an output from an earlier accepted parent. Mempool
membership and relay-admission policy are unchanged by selection.

The selector checks transactions and the prefix, not an arbitrary block header.
The staking engine constructs the header, commitment, and stake proof after
selection has restored the original UTXO set. Normal block validation remains
authoritative.

## Verification

Nine `template_selection` regressions cover the four failures, rejected script
execution after a vote was counted, vote-before-spend locking, oversized
candidates, parent/child transactions with governance inactive, and an invalid
prefix during parameter activation.

The regtest selection helper asserts that UTXOs, governance, parameters, supply,
stakeable supply, claims, tip, and journal length are unchanged after selection.
Accepted-template checks then pass the selected transactions to actual block
validation. The coinstake regression builds a real PoS block and stake proof.

```bash
cargo test -p vtorrent-node --lib template_selection -- --nocapture
cargo test -p vtorrent-node --lib governance_tests
cargo test --workspace --all-features --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The all-features workspace run passed, including 201 node unit tests, 12
governance integration regressions, and 18 daemon integration tests. Format and
strict Clippy checks passed.

A release daemon smoke run with the regtest-only governance override also
passed through the public broadcast, mempool, and staking paths. It confirmed
the proposal at height 10, counted 800,000,000 satoshis from four voting
outpoints, and created four locks. This was a functional smoke test, not a new
memory-budget measurement.

The existing single-run 600-outpoint memory result predates this selector
repair. It does not establish peak RSS for the revised path under a large
workload. Multi-proposal restart/reorg load tests and load-driver hardening
remain separate follow-ups. Governance activation is still unscheduled.

## Self-evaluation

This is a tested template-selection repair, not an independent consensus review.

| Axis | Score | Evidence and limitation |
|---|---|---|
| Accuracy | 4/5 | Four failing-before checks now pass and selected blocks validate. The original author performed this review. |
| Completeness | 4/5 | Nine focused cases cover rejection, activation, spend order, coinstake, size, and restoration. Selector-specific legacy-claim and swap fixtures remain useful additions. |
| Clarity | 4/5 | The report distinguishes tentative state from committed state and transaction selection from header validation. Reviewing checkpoints still requires tracing two modules. |
| Actionability | 4/5 | Reproduction commands, the full workspace run, and a daemon smoke run are recorded. Large-workload validation of the revised selector remains open. |
| Conciseness | 4/5 | The old API was removed and consensus transitions are shared. Most added code is checkpoint handling and regression fixtures. |

Overall: **4.0/5**. The next improvements are broader transaction-type fixtures,
multi-proposal restart/reorg load tests, and a non-destructive load driver.
Self-check: this assessment treats the repair as verified against the listed
cases and does not imply mainnet approval.

## Second self-review

A second pass over the selector, the restart seam, and the load driver found one
real gap and several unmeasured or untested areas.

### Fixed: silent no-op in the test seam

The daemon ignored `VTR_GOVERNANCE_TEST_ACTIVATION` when `--regtest` was set
without `--regtest-fast-stake`. It loaded the chain without the override and
never set it, so governance stayed inactive while the operator believed it was
active. The store now exposes the regtest (non-fast) variant, and the daemon
uses it. This is a test-seam gap, not a production path.

### Unmeasured: selection cost

`select_block_transactions` fully applies every mempool candidate, including
script verification and UTXO mutation. The previous filter ran only governance
processing. Selection runs once per successful kernel, so the cost is amortized
over a block, but it is a real increase for a large mempool and has not been
measured. The 600-outpoint run used twelve transactions.

### Untested: legacy-claim and swap rollback

Selection rolls back `claimed_addresses` and applies atomic-swap transactions
through the same path, but no regression covers either. The nine tests cover
votes, proposals, ordinary spends, the coinstake, block size, and the prefix.

### Redundant: conflict filter

`build_from_kernel_with_proof` still runs its `seen` input-conflict filter.
Selection already guarantees no conflicts, so the filter drops nothing. It is
harmless, but it could mask a selection regression by silently dropping a
conflicting transaction instead of failing.

### Leaky: test-only parameter

`load_into_chain_with_mode` carries a `governance_activation` parameter that is
always `None` in production builds. It is documented as test-only.

### Scores

| Axis | Score | Evidence |
|---|---|---|
| Accuracy | 4/5 | Selector transitions match validation and tests pass; the silent no-op was a real inaccuracy, now fixed. |
| Completeness | 3/5 | Legacy-claim and swap rollback are untested; the selection cost change is unmeasured. |
| Clarity | 4/5 | The report distinguishes tentative from committed state; the redundant filter is undocumented. |
| Actionability | 4/5 | Reproduction commands and the red-check are recorded; untested paths are named. |
| Conciseness | 4/5 | The selector reuses the validation helpers; checkpoint code is the bulk. |

Overall: **3.8/5**. Highest-impact improvements: add legacy-claim and swap
rollback regressions, then measure selection cost with a large mempool.
