# Governance PoS fork and stake-proof relay

This follow-up to `78b61c0` extends the real-peer branch test with genuine
coinstakes, stake kernels, UTXO commitments, and stake proofs. The scenario is
`governance_pos_fork_relays_stake_proofs` in
`vtorrent-daemon/tests/governance_reorg/mod.rs`.

## Fixture and live branch switch

The fixture retains the two-proposal, 32-outpoint workload. Height 1 is a
regtest-only funding block with a correct UTXO commitment. Height 2 contains
the common proposals in a PoS block. All subsequent old-branch and winning-
branch blocks are PoS blocks built by `StakingEngine` with a signing key.
Their kernels, signatures, and post-state commitments pass normal validation.

Historical fixture timestamps advance by at least the normal six-hour stake
age between stakes. This avoids wall-clock waits without lowering the stake
age. Both daemons use the regtest-only governance activation override.

The receiver starts on the old branch at height 4. A peer with a persisted
winning branch at height 5 connects over loopback P2P. The receiver reorganizes
from the common ancestor at height 2. Its governance state must change from
64 VTR Yes on both proposals and 32 long locks to:

- 64 VTR Yes on the shorter proposal.
- 32 VTR Yes on the longer proposal.
- 16 locks expiring at height 4006 and 16 at height 4106.

The scenario uses the same per-proposal and per-outpoint RPC assertions as the
earlier branch test.

## Failing-before evidence

The initial test reached the winning PoS tip and verified governance state,
but timed out waiting for the three relayed fork proofs:

```text
the receiving daemon must cache all relayed fork proofs: Elapsed(())
```

Source tracing identified three gaps:

1. The reorg handler did not request proofs for the applied fork blocks.
2. A node loaded from the store had an empty proof cache. `getproof` only read
   that cache, so a restarted serving node could not provide the proofs.
3. The proof RPC required a header in the separate SPV chain, even though the
   daemon shared its full-node proof cache with that endpoint.

## Repairs and proof binding

The reorg handler now requests a proof for every applied PoS fork block. On a
P2P cache miss, the serving node can reconstruct a canonical proof from its
retained undo history. It copies the current UTXO set, reverses the required
UTXO changes into that copy, and verifies the reconstructed pre-state root
against the parent header before constructing inclusion paths.

Reconstruction does not mutate the chain. It does not depend on retaining the
parent block body, because the parent header supplies the commitment and stake
modifier. Requests outside the retained undo window return no reconstructed
proof.

Received proofs are bound to a validated canonical block. Checks include the
coinstake transaction ID, the transaction-Merkle leaf and index, the exact
staking outpoint, the parent stake modifier, and both commitment roots. The
previous handler could verify a membership path without binding its leaf to
the actual staking coin or coinstake transaction.

The RPC now recognizes headers from either the full-node chain or the SPV
chain. It returns the shared cache entry, or `proof: null` for a known header
whose proof is not cached. RPC lookup does not trigger reconstruction.

## Successful evidence

After the live reorg, all three fork proofs appeared in the receiver's cache.
Each matched the original `StakingEngine` proof, including both inclusion
paths, UTXO fields, and modifier. The RPC reads the cache only, so this check
does not substitute local RPC reconstruction for relay.

The test then persisted a regtest faucet descendant at height 6, shut down the
receiver, and restarted it. The tip, parameters, proposal tallies, and mixed
locks matched. The restarted proof cache was empty. A separate protocol-v3
P2P probe requested the latest PoS proof and received an exact match to the
original, exercising cold-cache reconstruction and serving after restart.

Four node regressions also cover exact reconstruction without mutation,
missing parent bodies, a valid membership path for the wrong staking coin, a
valid transaction path for the wrong transaction, incorrect Merkle metadata
and modifier, and failure outside the undo window.

## Verification

```bash
cargo test -p vtorrent-node --lib stake_proof -- --nocapture
cargo test -p vtorrent-daemon --release --features test-util \
    --test process_recovery governance_pos_fork_relays_stake_proofs -- --nocapture
cargo test --workspace --all-features --quiet
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

The targeted test and the complete all-features workspace suite passed,
including 210 node unit tests, 21 daemon recovery tests, and 141 RPC tests.
Formatting and strict Clippy checks passed.

## Limits

Proof reconstruction uses retained canonical undo history, normally 100 blocks.
An already cached proof can outlive that window, but a cold node does not gain
an archival proof service from this repair. Persistence or historical replay
for older proofs remains separate work.

This is a functional fork and relay test, not a new memory or latency
measurement. A cold-cache P2P request copies the full UTXO set and builds a
Merkle tree while holding the chain mutex. Successful reconstruction populates
the bounded proof cache. Requests outside retained history return before
copying the UTXO set. The existing per-peer message budget applies, but this
repair adds no reconstruction-specific budget. Concurrent-peer request cost
and chain-lock latency remain unmeasured.

This repair does not enable SPV PoS header acceptance, which remains
fail-closed. A subsequent scenario covers SIGKILL between governance-reorg
store operations in [the interrupted-reorg test](governance-interrupted-reorg-test.md).
No production deployment is scheduled.
