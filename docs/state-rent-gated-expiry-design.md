# State rent: gated expiry design

Status: DESIGN for 2.0.0. Implementation ships disabled; a governance proposal activates it.
Owner decision (2026-11-28): governance active at a fixed height; state rent implemented but gated.

## 1. Why gated

Expiry changes the social contract that an unspent output stays spendable forever. The
existing design record recommends measuring chain growth and deciding through
governance. This design therefore implements the mechanism, keeps it inactive at
launch, and exposes it only as a governance-set parameter with a fixed activation
height.

## 2. Mechanism

Expiry, not demurrage.

- Each UTXO gains an `expiry_height: Option<u32>`. `None` means "never expires";
  this is the value for every output created before activation.
- A governance parameter `StateRentExpiryBlocks` sets the lifetime, bounded to
  525,600 to 21,024,000 blocks (roughly one to forty years at 60 s blocks).
- At activation, outputs created after the activation height receive
  `expiry_height = creation_height + lifetime`. Existing outputs stay `None`
  forever, so no pre-existing balance can expire. This is the reversible part of
  the design and the reason it can ship gated.
- A UTXO is unspendable once `height >= expiry_height`. A spend of an expired
  output is rejected by the same path that rejects spent outputs, so no new
  validation branch is needed for correctness.
- Pruning is a block-application step: after a block is applied, outputs whose
  `expiry_height` is at or below the new height are removed from the UTXO set and
  from `total_supply`. Removal is journalled so a reorg restores them.
- The commitment (`utxo_root`) covers the post-prune set, so every node agrees.

## 3. Supply accounting

Pruned value is burned. `total_supply` decreases by exactly the pruned value and
the journal records the delta, so a rollback restores both the UTXOs and the
supply counter. `MAX_SUPPLY` is unchanged and remains an upper bound.

## 4. Wallet and UI

- The wallet shows the expiry height of every spendable output and warns 30 days,
  7 days, and 1 day before expiry, with the block estimate in wall-clock terms.
- Sending from an output near expiry is allowed and does not inherit the old
  expiry: the new output gets a fresh lifetime.
- The activity journal records each expiry warning and each burn, so a user can
  see exactly what happened.
- The staking page refuses to select an output inside the warning window as a
  stake candidate when another eligible output exists.

## 5. Governance integration

- New `ParamId::StateRentExpiryBlocks`, validated to the range above.
- The parameter has no effect until governance is active and a proposal
  activating it has passed and reached its activation height, which is at least
  one voting period after launch.
- Lowering the lifetime applies only to outputs created after that activation;
  it never shortens an existing output's expiry. Raising it applies to new
  outputs only. This keeps the rule monotonic and avoids surprise burns.

## 6. Reorg and restart

- Pruning is part of the block journal: a rollback re-inserts pruned outputs and
  restores `total_supply`.
- A restart replays from the store, so expiry is recomputed deterministically
  from headers and the parameter history.
- A node with a different parameter history cannot agree on the UTXO root at the
  activation height; the upgrade uses the existing coordinated-activation
  mechanism and a checkpoint.

## 7. Test plan

1. Unit: expiry arithmetic at the bounds; expired spend rejected; unexpired spend
   accepted; `None` outputs never expire.
2. Unit: pruning removes exactly the expired outputs and adjusts `total_supply`
   by their sum.
3. Unit: reorg restores pruned outputs and the supply counter.
4. Unit: parameter activation cannot shorten an existing expiry.
5. Integration: two nodes with the same parameter history agree on `utxo_root`
   across activation, restart, and reorg.
6. Integration: a node missing the activation rejects the block with a clear
   error rather than forking silently.
7. Wallet: warnings fire at 30, 7, and 1 day; the journal records the burn.
8. Fleet: a soak with a short lifetime on regtest expires and burns outputs, and
   the chain continues.

## 8. Rollout

1. Ship the code with the parameter unset.
2. Pass a governance proposal that sets the lifetime.
3. At the activation height, new outputs begin receiving expiry heights.
4. Publish the first burn height in the release notes and on the explorer.

## 9. Non-goals

- No rent on pre-existing outputs.
- No demurrage and no silent balance reduction.
- No change to `MAX_SUPPLY` or block rewards.
