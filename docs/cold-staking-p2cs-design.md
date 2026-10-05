# Cold Staking (P2CS) — Design

Status: DRAFT (post-soak; **consensus change** — fresh-genesis decision required)
Scope: `vtorrent-script` (new script type), `vtorrent-node` (stakeability + coinstake),
`vtorrent-wallet` (key roles), RPC + UI.
Motivation: today staking requires the **spending key on the staking node**
(`staking.rs:537` signs the coinstake with the same key that controls the
coins), so any host you stake on can move your funds. Cold staking separates a
cold **spending key** from a hot **staking key** that can only produce blocks.

## 1. Problem

`is_stakeable` (`consensus.rs:149`) accepts only **P2PKH** UTXOs, and
`sign_coinstake_input` (`staking.rs:537`) signs the coinstake with the UTXO's
own key. There is no way to let a node stake coins it cannot spend. For a
PoS chain whose value proposition is staking, that is the largest security gap:
VPS / seed / staking-service operators must hold spendable keys.

## 2. Design — P2CS script

A new standard script, **P2CS** (pay-to-cold-stake):

```
OP_IF
    <staking_pubkey> OP_CHECKSIGVERIFY
OP_ELSE
    <spending_pubkey> OP_CHECKSIGVERIFY
    <locktime> OP_CHECKLOCKTIMEVERIFY
OP_ENDIF
```

- **Stake path** (`OP_IF` true): the coinstake provides the staking key's
  signature. It cannot spend to an arbitrary destination — the coinstake
  output must pay back to the same P2CS script (enforced by the coinstake
  rules), so the staking key can only ever re-stake, never redirect funds.
- **Spend path** (`OP_ELSE`): the spending key's signature, gated by a
  `CHECKLOCKTIMEVERIFY` so the cold key can withdraw after a delay (or
  immediately if `locktime` is 0 — a policy choice; see §5).

The script engine already supports `OP_IF/OP_ELSE/OP_ENDIF` (`engine.rs:143-171`),
`OP_CHECKSIGVERIFY` (`0xad`), and `OP_CHECKLOCKTIMEVERIFY` (`0xb1`). No new
opcodes are needed — only a new `ScriptType::P2CS { staking, spending, locktime }`
in `standard.rs` and a `build_p2cs(...)` constructor.

## 3. Stakeability and coinstake

- **`is_stakeable`** (`consensus.rs:149`) extends to accept `P2CS` (value ≥
  `MIN_STAKE_AMOUNT`), in addition to `P2PKH`. This is the consensus gate that
  decides which UTXOs count toward `total_staked` and can produce a block.
- **Coinstake signing** (`staking.rs:537`): when the staked UTXO is P2CS, sign
  the coinstake input with the **staking key** and use the `OP_IF` (stake) path
  in the scriptSig. The node holds only the staking key.
- **Coinstake output rule** (new, consensus): a coinstake spending a P2CS input
  must pay its stake back to the **same P2CS script** (same staking + spending
  keys). This is what makes the hot key unable to steal: it can only re-lock the
  coins under the cold key's control. Verify in `apply_transaction_journaled`
  alongside the existing coinstake checks.
- **Spending** (cold key): a normal transaction spends the P2CS output via the
  `OP_ELSE` path with the spending key's signature; `CHECKLOCKTIMEVERIFY`
  enforces the delay.

## 4. Wallet / UX

- **Key roles**: the wallet generates a **staking keypair** and a **spending
  keypair**, derives the P2CS address, and can **export the staking key (WIF)**
  for the hot node while the spending key stays cold/offline.
- **RPC**: `createcoldstakeaddress`, `exportstakingkey`, `getcoldstakinginfo`.
- **UI**: a "Delegate staking" flow on the Staking page — show the P2CS address,
  the exported staking key, and a warning that the staking key is hot.
- **Watch-only**: the hot node needs only the staking key + P2CS address, so it
  can run without any spendable key (pairs with a future watch-only wallet).

## 5. Consensus impact and activation

- **Consensus change**: new script type in the UTXO commitment (`hash_utxo`
  covers `script_pubkey`, so P2CS outputs change roots), new `is_stakeable`
  rule, new coinstake-output rule. Two nodes differing on any of these diverge.
- **Activation**: fresh genesis (recommended, pre-mainnet) — no P2CS outputs
  exist before launch, so old roots are unaffected; or a height activation if a
  chain must be preserved. Same class as `docs/op-return-utxo-exclusion-design.md`.
- **Locktime policy**: decide whether the spend path requires a delay
  (`locktime > 0`) or allows immediate withdrawal. A delay is safer against a
  compromised *spending* key but worse UX; a `0` locktime makes the spending key
  equivalent to a normal P2PKH key (still fine — the point is the *staking* key
  is hot, not the spending key).

## 6. Adversarial review (pre-implementation)

- **R1 — the hot key must not be able to redirect the stake.** The whole
  security claim rests on the coinstake-output rule (§3). If a P2CS coinstake
  could pay the stake to an arbitrary script, the hot key could steal. This rule
  must be consensus-enforced and tested with a malicious coinstake.
- **R2 — `CHECKLOCKTIMEVERIFY` semantics.** CLTV compares against the tx
  `lock_time` and requires it to be non-final; the engine's CLTV (`engine.rs:494`) must be exercised in the P2CS spend path, and the wallet must set
  `lock_time` correctly. Verify the engine's CLTV matches Bitcoin semantics.
- **R3 — `is_stakeable` widening is consensus.** Adding P2CS to `is_stakeable`
  changes `total_staked` and kernel probabilities → every node must agree.
  Gate behind the same activation as the script type.
- **R4 — commitment/leaf changes.** `hash_utxo` already hashes the full
  `script_pubkey`, so P2CS leaves are well-defined; no `hash_utxo` change needed.
  Confirm no code assumes P2PKH-only scripts in the commitment.
- **R5 — stake-age / bootstrap rules.** The T3 bootstrap exemption and
  `MIN/MAX_STAKE_AGE` must apply to P2CS identically; check `is_stakeable`'s
  callers.
- **R6 — wallet key handling.** The exported staking key is hot; the UI must
  warn, and the spending key must never leave the cold wallet. Reuse the
  existing WIF/zeroize paths.
- **R7 — SPV stake-proof.** `StakeProof`/`SpvUtxo` carry `script_pubkey`; a P2CS
  stake proof must verify against the P2CS commitment leaf. Check
  `spv_chain.rs:330` handles non-P2PKH scripts.
- **R8 — scope.** This is a large, consensus-touching feature. Land it in the
  post-soak batch with its own soak, not before sign-off.

## 7. Test plan

- Script: P2CS spend via staking key (stake path) and spending key (spend path);
  reject wrong key on each path; CLTV enforced.
- Consensus: `is_stakeable` accepts P2CS ≥ MIN_STAKE_AMOUNT; a P2CS coinstake
  paying to a *different* script is rejected (R1); paying to the same script is
  accepted.
- Commitment: P2CS output leaf/root parity with a reference.
- Wallet: generate pair, export staking key, build+sign a P2CS coinstake with
  only the staking key.
- SPV: P2CS stake proof verifies.
- End-to-end: a node holding only the staking key produces a valid block and
  cannot spend the coins.

## 8. Non-goals

- Not hardware-wallet signing (a natural follow-on).
- Not changing the reward/kernel math.
- Not part of the current soak window.

## 9. References

- `vtorrent-node/src/consensus.rs:149` `is_stakeable`; `:174` `check_stake_kernel_v2`.
- `vtorrent-node/src/staking.rs:537` `sign_coinstake_input`.
- `vtorrent-script/src/engine.rs:143-171` IF/ELSE/ENDIF; `:461-490` CHECKSIG*;
  `:494` CLTV; `standard.rs:16` `ScriptType`, `:34` `classify_script`.
- `vtorrent-node/src/chain/chain_reorg.rs` coinstake checks.
- `vtorrent-spv/src/spv_chain.rs:330` stake-proof verification.
- `docs/op-return-utxo-exclusion-design.md` (consensus-change pattern).
