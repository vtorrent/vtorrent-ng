# Cold Staking (P2CS) — Implementation Plan

Companion to `docs/cold-staking-p2cs-design.md`. This is the build order for the
post-soak batch. Each step is independently testable; the consensus steps are
gated behind the fresh-genesis decision.

## Step 0 — Decide activation

- **Fresh genesis (recommended).** No P2CS outputs exist pre-launch, so old
  roots are unaffected and no activation height is needed. Confirm the genesis
  and legacy snapshot contain no P2CS scripts (they are P2PKH) — assert in a test.
- Alternative: height activation (more code; only if a chain must be preserved).

## Step 1 — `vtorrent-script`: the P2CS type

1. `ScriptType::P2CS { staking: [u8;20], spending: [u8;20], locktime: u32 }` in
   `standard.rs:16`.
2. `build_p2cs(staking_pubkey_hash, spending_pubkey_hash, locktime) -> Script`
   mirroring `build_htlc` (`standard.rs:160`): `OP_IF <staking> OP_CHECKSIGVERIFY
   OP_ELSE <spending> OP_CHECKSIGVERIFY <locktime> OP_CHECKLOCKTIMEVERIFY
   OP_ENDIF`.
3. `classify_script` (`standard.rs:34`) recognises the exact P2CS shape.
4. Tests: build→classify round-trip; spend via each branch; wrong key rejected;
   CLTV enforced (`engine.rs:494`).

## Step 2 — `vtorrent-node`: stakeability + coinstake

1. `is_stakeable` (`consensus.rs:149`) accepts `P2CS` (value ≥ `MIN_STAKE_AMOUNT`)
   in addition to `P2PKH`. **Consensus** (changes `total_staked`).
2. `sign_coinstake_input` (`staking.rs:537`): for a P2CS staked UTXO, sign with
   the **staking key** and build the scriptSig for the `OP_IF` branch
   (`<sig> <staking_pubkey> OP_TRUE`), not the P2PKH form.
3. **Coinstake-output rule (R1, the security hinge):** in
   `apply_transaction_journaled`, a coinstake spending a P2CS input must pay its
   stake back to the **same P2CS script**. Reject otherwise. Test with a
   malicious coinstake that redirects the stake.
4. Stake-age / bootstrap (`MIN/MAX_STAKE_AGE`, T3) apply to P2CS identically.

## Step 3 — `vtorrent-wallet`: key roles

1. Generate a **staking keypair** and a **spending keypair** (reuse `hd.rs` /
   `keys.rs`); derive the P2CS address.
2. `export_staking_key() -> WIF` (hot, for the node); the spending key stays
   cold. Reuse the WIF/zeroize paths.
3. Build + sign a P2CS coinstake using only the staking key (unit test).

## Step 4 — RPC + UI

- RPC: `createcoldstakeaddress`, `exportstakingkey`, `getcoldstakinginfo`.
- UI: "Delegate staking" flow on the Staking page — show the P2CS address and
  the exported staking key, with a clear "this key is hot" warning.

## Step 5 — SPV stake proof

- `StakeProof`/`SpvUtxo` already carry `script_pubkey`; verify a P2CS stake proof
  against the P2CS commitment leaf (`spv_chain.rs:330`). Test.

## Step 6 — Soak

- Deploy to the regtest fleet with a P2CS staking node that holds **only** the
  staking key; confirm it produces valid blocks and **cannot** spend the coins.
- Run the standard 7-day window; the node's spendable-key absence is the
  acceptance criterion.

## Ordering / risk

- Steps 1–3 are the consensus core; do them together, review adversarially
  (design §6 R1–R8), and land behind the fresh-genesis decision.
- Steps 4–5 are additive and can follow.
- Step 6 is a fresh soak window.

## References

- `docs/cold-staking-p2cs-design.md` (design + adversarial review).
- `vtorrent-script/src/standard.rs:16,34,160`; `engine.rs:494` CLTV.
- `vtorrent-node/src/consensus.rs:149`; `staking.rs:537`;
  `chain/chain_reorg.rs` coinstake checks.
- `vtorrent-wallet/src/hd.rs`, `keys.rs`, `tx_builder.rs`.
- `vtorrent-spv/src/spv_chain.rs:330`.
