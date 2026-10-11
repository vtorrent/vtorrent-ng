# Governance Step 3 self-review

Reviewed commit: `cd2220e`. Verdict: **not ready for activation**.

The earlier claims that this implementation was complete, activation-gated,
and reorg-safe were incorrect. Existing tests exercise the implemented happy
paths, not the required consensus invariants.

## Findings

| Severity | Finding | Evidence | Required correction |
|---|---|---|---|
| Critical | Activation is nondeterministic. Passed proposals for the same parameter are applied in randomized map iteration order, repeatedly on subsequent blocks. | `vtorrent-node/src/governance.rs:377-408`. Executable probe produced both 300 and 400 bps from identical proposal records. | Define a deterministic conflict order and apply each activation once. |
| Critical | Voter identity is untrusted metadata. `claim_address` selects whose entire address balance is counted and locked, without proving ownership of that balance. Standard transactions do not validate ownership of `claim_address`. | `vtorrent-node/src/chain.rs:315-342`; `vtorrent-node/src/consensus.rs:398-476`. | Authenticate explicit voting outpoints against their spending authority; do not derive voter authority from claim metadata. |
| Critical | Vote replay inflates voting power. Every matching OP_RETURN adds the same stake again, even when already locked. | `vtorrent-node/src/governance.rs:331-361`. Probe: two votes counted 200 units from one 100-unit UTXO. | Track unique voting outpoints per proposal and define replacement/revote semantics. |
| High | Reorg loses common-ancestor governance and can leak parameters from failed branches. Rollback does not undo governance; reorg clears all proposals/locks and replays only the fork suffix; failure restores governance but not `chain.params`. | `vtorrent-node/src/chain/chain_reorg.rs:50-110,490-525,594-617`. | Journal governance and parameter changes atomically with UTXO changes, restoring the exact fork-point state. |
| High | No BIP-9 activation gate exists on the new rules. Governance processing and vote-lock enforcement run unconditionally. | `vtorrent-node/src/chain.rs:1128-1133`; `vtorrent-node/src/chain/chain_reorg.rs:243-256`. | Gate processing and enforcement on a defined deployment's Active state. |
| High | Age uses blocks but the configured minimum is seconds. The default 21,600 therefore means 21,600 blocks, not six hours. A young dust output also invalidates the entire address's vote. | `vtorrent-node/src/governance.rs:339-346`; `vtorrent-node/src/consensus.rs:31`; `Utxo.timestamp` exists in `chain.rs`. | Use consensus block time versus UTXO timestamp and explicitly select eligible voting coins. |
| High | A longer proposal reuses an earlier lock without extending it. | `vtorrent-node/src/governance.rs:348-355`. Probe counted the second proposal's stake but found it spendable at height 16 while that proposal remained open through height 100. | Preserve all outstanding lock obligations or extend to their maximum expiry. |
| High | Proposal contents are decoded but not validated. Zero or inconsistent stake ages, unchecked narrowing of annual rate, and arbitrary voting/activation windows can reach parameter application. | `vtorrent-node/src/governance.rs:51-60,320-330,377-408`. | Validate parameter bounds and cross-parameter invariants, windows, deposit funding, and activation delay at the consensus boundary. |
| Medium | Voting locks also forbid coinstake spends, contradicting the promised ability to keep staking. | `vtorrent-node/src/chain/chain_reorg.rs:241-256`. | Define a constrained lock-preserving coinstake transition, or explicitly revise the staking promise. |
| Medium | RPC tallies are recomputed using today's UTXOs for historical blocks rather than authoritative governance state. | `vtorrent-rpc/src/handlers/governance.rs:65-104`. | Serve the chain's accepted governance records and lock state consistently. |

## Executable evidence

Temporary integration probes were run with:

```text
cargo test -p vtorrent-node --test governance_review_probe -- --nocapture

reproduce_duplicate_vote_inflation ... ok
reproduce_longer_vote_reuses_short_lock ... ok
reproduce_hashmap_activation_divergence ... ok
3 passed; 0 failed
```

These probes assert the presence of the defects, not correct behavior. They
were moved out of the normal test suite to
`/tmp/opencode/governance_review_probe.rs` after execution. They can be copied
back into `vtorrent-node/tests/` to repeat the checks against the reviewed
commit. Fix verification should instead assert the required invariants.

Authorization and rollback findings are source-traced, not end-to-end exploit
reproductions. Fleet deployment status was not checked in this review.

## Repair order

1. Authenticate explicit voting outpoints, enforce unique voting power, and
   validate proposal contents.
2. Make transitions deterministic, apply activations once, and journal all
   governance/parameter mutations across rejection and reorg.
3. Add the deployment gate, timestamp-based maturity, complete lock lifetimes,
   and agreed staking semantics. Serve authoritative state through RPC.
4. Verify signed unauthorized votes, replay, conflicting activations,
   activation-height boundaries, successful and failed reorgs, restart replay,
   overlapping locks, and inactive-deployment behavior.

## Self-evaluation of the original delivery

| Axis | Score | Evidence |
|---|---|---|
| Accuracy | 1/5 | Activation-gated and reorg-safe claims contradicted the code. |
| Completeness | 1/5 | Ownership, duplicate counting, activation gating, and rollback invariants were missing. |
| Clarity | 3/5 | The feature summary was readable, but omitted the blocks-versus-seconds mismatch and staking restriction. |
| Actionability | 2/5 | The patch cannot safely be activated; it needs consensus rework. |
| Conciseness | 3/5 | Repeated success summaries overstated what testing established. |

Overall: **2.0/5**. The highest-impact improvement is to test consensus
invariants before treating happy-path tests as completion evidence.

## Repair follow-up

The working-tree repair replaces address-based votes with authenticated
`SignedVote` outpoints, unique vote identities, longest-expiry locks, and
deterministic one-shot activation. Maturity now uses timestamps. P2CS owners
authorize votes; operators do not. Locked stakes can re-lock to the same script
without duplicating voting identity. Legacy uncompressed owner keys also work.

Block journals restore governance and parameters with UTXO state on rejection
and reorg. An additional error-path issue was repaired: input removal is now
journaled before script verification, so an invalid signature cannot leave the
input missing after rejection. Stake production reads the next block's active
parameters, and threshold changes recount the stakeable denominator.

The deployment gate shares its schedule with the RPC. No governance deployment
is scheduled. A new failing-before test also exposed the skipped BIP-9 lock-in
period, which is now enforced.

The initial regression run failed with `200` versus `100` units of vote weight,
an expired overlapping lock, and two active parameter outcomes versus one.
The repaired core and block-path checks cover those defects, signed forgery,
cross-network replay, cold-stake ownership, bad-root rejection, successful and
failed reorgs, activation boundaries, and restart-style block replay.

The all-features workspace tests passed during the repair. After the final
changes, the node suite passed with 188 unit tests and 11 governance regression
tests, plus its existing integration tests. The authoritative-state RPC test,
workspace strict Clippy, format check, and diff whitespace check also passed.
The current protocol, deposit-burn semantics, parameter-policy limitations, and
snapshot-memory review requirement are in `docs/governance-step3-design.md`.
This repair does not schedule or deploy governance activation.

### Repair self-evaluation

| Axis | Score | Evidence and remaining gap |
|---|---|---|
| Accuracy | 4/5 | Executable regressions cover the reported defects. Independent consensus review is still absent. |
| Completeness | 4/5 | Authorization, uniqueness, locks, activation, rollback, and RPC state are repaired. Activation policy and snapshot-memory measurements remain open. |
| Clarity | 4/5 | The current protocol document specifies signed votes, bounds, and deposit burns. Historical drafts still need its supersession notice. |
| Actionability | 4/5 | The changes and tests are ready for code review. Production activation remains deliberately unscheduled. |
| Conciseness | 4/5 | Unsafe state and duplicate parameter copies were removed. The signed-vote and rollback fixtures add substantial test code. |

Overall: **4.0/5**, as a tested repair ready for review, not a mainnet sign-off.
The next improvements are independent protocol review, approval of parameter
and deposit policy, and memory measurements for active governance journals.
