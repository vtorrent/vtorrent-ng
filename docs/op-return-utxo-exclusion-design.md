# OP_RETURN / Unspendable-Output UTXO Exclusion — Design

Status: DRAFT (post-soak; consensus change — do not deploy without a plan)
Scope: `vtorrent-node` UTXO set + commitment.
Constraint: not part of the current soak window; belongs to the post-soak
consensus batch with a fresh-genesis decision.

## 1. Motivation

`OP_RETURN` outputs are **provably unspendable** (`vtorrent-script/src/engine.rs:176`,
`ScriptType::OpReturn` in `standard.rs:60`). They carry no spendable value and
can never be a stake candidate (`is_stakeable` is P2PKH-only, T4). Yet the node
**stores every output in the UTXO set**, including OP_RETURN ones
(`chain/chain_reorg.rs:358-380`), so they inflate:

- the in-memory `utxo_set` BTreeMap,
- the per-block **UTXO commitment** (leaf + Merkle cost — the commitment is
  computed over `utxo_set`, now via `recompute_utxo_root`), and
- the store's `UTXOS` table.

Bitcoin excludes such outputs from its UTXO set. Doing the same here removes
dead entries from the commitment and is the remaining cleanliness item in the
post-soak batch (the memory *leak* question is already closed —
`docs/soak-log.md` 2026-09-26).

## 2. Current behaviour

`apply_transaction_journaled` (`chain/chain_reorg.rs:358-380`) unconditionally:

```rust
for (vout, output) in tx.outputs.iter().enumerate() {
    let key = (txid, vout as u32);
    let utxo = Utxo { .. };
    if is_stakeable(&utxo) { journal.staked_delta += ..; }   // OP_RETURN already excluded here
    chain.utxo_set.insert(key, utxo);                        // <-- includes OP_RETURN
    journal.changes.push(UtxoChange::Added { key });
}
```

Rollback removes exactly the journalled `UtxoChange::Added` keys, so insert and
rollback are symmetric.

## 3. Design

**Skip inserting outputs whose script is provably unspendable.**

- Add a predicate, e.g. in `vtorrent-node/src/block.rs`:
  ```rust
  /// True if an output can ever be spent (and so belongs in the UTXO set).
  pub fn is_utxo_eligible(output: &TxOutput) -> bool {
      !matches!(classify_script(&output.script_pubkey), ScriptType::OpReturn)
  }
  ```
  `vtorrent-node` already depends on `vtorrent-script` (`Cargo.toml:17`).
- In `apply_transaction_journaled`, `continue` past ineligible outputs: do **not**
  insert and do **not** push a `UtxoChange::Added`. Rollback needs no change
  (nothing was added).
- Leave output-value accounting untouched: supply is derived from
  coinbase/claims, not the UTXO set (verify in §6), and `staked_delta` already
  excludes OP_RETURN.

Scope decision: **only OP_RETURN** for the first cut. Do *not* also drop
zero-value or non-standard scripts — those are spendable (Bitcoin keeps
zero-value outputs), and widening the predicate enlarges the consensus surface
for no benefit.

## 4. Consensus impact

- The post-apply `utxo_root` is computed over `utxo_set`; excluding OP_RETURN
  changes the root **for any block that contains an OP_RETURN output**. This is
  a **consensus change**: two nodes differing on the predicate would diverge.
- `get_utxos_for_address`, `resolve_output`, and the staking proof all operate on
  spendable outputs, so unaffected in practice (an OP_RETURN output is neither a
  valid spend nor a stake candidate).
- The header commitment becomes "the spendable UTXO set", which is the
  meaningful commitment for stake proofs (SPV mirrors spendable UTXOs).

## 5. Activation

Two options:

1. **Fresh genesis, no activation height** (recommended pre-mainnet). If the
   genesis block and the legacy snapshot contain **no OP_RETURN outputs** (they
   are P2PKH balances), then a chain with no OP_RETURN txs is byte-identical
   before and after the change, so the rule can be adopted unconditionally at
   launch. Confirm by scanning genesis/snapshot scripts.
2. **Height activation** if any pre-existing chain must be preserved with
   existing roots: apply the predicate only for blocks at height ≥ H, and
   compute H−1's root the old way. More code, only needed if we must not change
   historical roots.

The regtest soak chain likely contains no OP_RETURN outputs (all coinbase,
coinstake, and faucet outputs are P2PKH), so option 1 should apply cleanly —
**verify** with a one-off scan (§6).

## 6. Open questions (resolve before implementing)

- **Supply accounting**: confirm `total_supply` is not computed by summing the
  UTXO set. If it is, excluding OP_RETURN would change supply reporting.
- **Does the current chain contain OP_RETURN outputs?** Scan the store/chain;
  if zero, the change is a no-op on existing data (option 1 clean).
- **`get_recent_transactions` / fee paths** that call `resolve_output`: an
  OP_RETURN output would no longer resolve. Those are history/display paths and
  already treat misses as "unknown" — confirm they do not error.
- **Policy vs consensus**: a wallet could still *create* OP_RETURN outputs
  (data carriers). The rule only affects whether they enter the UTXO set; block
  validity is unchanged. Confirm no validation path requires the output to be in
  the set.

## 7. Test plan

- A block containing an OP_RETURN output: excluded from `utxo_set`, absent from
  `journal.changes`; `utxo_root` equals a reference computed over spendable
  outputs only.
- Rollback of such a block is symmetric (no phantom changes).
- Determinism: `recompute_utxo_root` == `compute_utxo_root_sorted` over the
  post-apply spendable set.
- Genesis/snapshot: root unchanged (no OP_RETURN in the snapshot) — assert.
- Regression: existing chain/reorg/staking tests unchanged.

## 8. Non-goals

- Not changing spend validation, fee rules, or script semantics.
- Not dropping zero-value or non-standard outputs.
- Not part of the current soak window.

## 9. Adversarial review

**R1 — BLOCKER: the producer must apply the same predicate.** The staking
producer computes the post-apply root itself in
`staking.rs::build_post_apply_leaves` (`compute_post_apply_root` lineage), which
currently adds **every** output. If the chain excludes OP_RETURN but the
producer does not, the producer's `header.utxo_root` ≠ the chain's
`journal.utxo_root` and `add_block` rejects the block
(`chain_reorg.rs:567`). The predicate must be applied in **both** places, from
one shared helper (`is_utxo_eligible`). This is the single most important
implementation constraint.

**R2 — The pre-apply inclusion proof is unaffected, but verify.** The staking
UTXO is spendable, so its leaf exists under both rules; only the *index* shifts
(fewer leaves). No correctness issue, but the leaf-index computation must scan
the filtered set consistently.

**R3 — Existing stores hold OP_RETURN UTXOs.** Blocks applied before the change
wrote them to the store's `UTXOS` table. After the change, replay rebuilds the
in-memory set correctly (filtered), but the store table is stale. This is
another reason to prefer **fresh genesis (option 1)**; an in-place migration
would need to delete the stale rows by re-deriving them.

**R4 — Genesis/snapshot assumption is load-bearing.** Option 1 is only clean if
the genesis coinbase + legacy distribution contain **no OP_RETURN outputs**.
They are P2PKH today, but this must be *asserted in a test* (`genesis` root
unchanged), not assumed — otherwise the genesis commitment silently changes.

**R5 — Supply.** If `total_supply` is derived by summing the UTXO set anywhere,
exclusion changes reported supply. §6 requires confirming it is coinbase/claim-
derived. Also decide whether OP_RETURN value is "burned" for supply purposes
(it is simply not a UTXO; supply bookkeeping should be independent of the set).

**R6 — Only OP_RETURN, first byte.** `classify_script` keys on
`script_pubkey[0] == 0x6a`, which matches Bitcoin's "provably unspendable"
definition. Do not broaden to zero-value/non-standard outputs (spendable).

**R7 — Activation is a network-wide flag.** Whether option 1 or 2, every node
must agree. A height activation with a wrong H forks. Prefer option 1 with a
fresh genesis + a test asserting no genesis OP_RETURN, so no activation height
is needed.

**R8 — Not urgent.** With the RSS investigation closed (no leak) this is a
cleanliness/commitment-size item, not a correctness or performance blocker.
Land it in the post-soak batch, not before sign-off.

## 10. References

- `vtorrent-node/src/chain/chain_reorg.rs:358-380` output insert loop.
- `vtorrent-script/src/standard.rs:34,60` `classify_script` / `ScriptType::OpReturn`;
  `engine.rs:176` OP_RETURN spend failure.
- `vtorrent-node/src/consensus.rs` `is_stakeable` (T4, P2PKH-only).
- `docs/utxo-commitment-scratch-design.md` (commitment computation).
- `docs/soak-log.md` 2026-09-25/26 (RSS investigation closed).
