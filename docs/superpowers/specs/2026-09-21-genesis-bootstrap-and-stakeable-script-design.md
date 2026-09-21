# Genesis Bootstrap + Stakeable-Script Restriction — Design

**Date:** 2026-09-21
**Status:** Draft — approved via brainstorming (T3 approach B, T4 P2PKH-only)
**Author:** vTorrent Dev
**Related:** `docs/code-review-2026-09-20.md` (T3, T4), `docs/code-review-2026-09-20-fix-status.md`, `vtorrent-node/src/consensus.rs:147`, `vtorrent-node/src/consensus.rs:172`, `vtorrent-node/src/chain/chain_reorg.rs:312`, `vtorrent-node/src/chain.rs:246`, `vtorrent-node/src/genesis.rs:86`, `vtorrent-rpc/src/handlers/dex.rs:214`

## 1. Overview

Two open findings from the third-pass review are consensus-rule changes around
staking eligibility. They are independent but touch the same denominator
(`Chain::total_staked`) and are specified together.

- **T4 — `is_stakeable` over-counts.** `is_stakeable` accepts any non-OP_RETURN
  output ≥ `MIN_STAKE_AMOUNT`, but the staking engine only ever stakes a UTXO
  whose script equals its own P2PKH script (`staking.rs:242`). P2SH / P2MS /
  P2PK / HTLC / `NonStandard` outputs therefore count in the kernel denominator
  but can never win a kernel, so any holder can park coins to dilute every
  honest staker's hit probability. Consensus stays internally consistent
  (dilution, not a split).
- **T3 — genesis bootstrap deadlock.** `check_stake_kernel_v2` returns `false`
  when `total_staked == 0` (`consensus.rs:178`). Genesis has **zero** stakeable
  UTXOs: the coinbase output is 0 and all 59,375 distribution outputs are
  OP_RETURN (`genesis.rs:86-89`), which `is_stakeable` excludes. A `LegacyClaim`
  creates a spendable P2PKH output but must be mined in a block; blocks are PoS
  and a coinstake needs a stakeable UTXO. Production rejects PoW, and the only
  production path that creates spendable outputs is staking itself. From a fresh
  mainnet genesis, staking can therefore never start.

## 2. Goals / Non-Goals

**Goals:**
- Restrict `is_stakeable` to the script class the staking engine can actually
  spend (P2PKH), closing T4's dilution vector.
- Define a deterministic, in-repo bootstrap path so a fresh mainnet chain can
  produce its first stakeable UTXO and begin PoS, closing T3.
- Preserve the frozen genesis block (`36ca792a…`) and the running soak chain:
  both changes must replay existing blocks unchanged.
- No premine: total supply is unchanged by the bootstrap.

**Non-Goals:**
- Changing the v2 kernel math or `MIN_STAKE_AMOUNT` / `MIN_STAKE_AGE`.
- Making the staking engine able to spend non-P2PKH outputs (the alternative T4
  fix). P2PKH-only is the smaller, safer change.
- A generic `submitblock` RPC or offline block-construction tooling.
- Any change to the legacy snapshot, claim amounts, or claim signature scheme.

## 3. T4 — Restrict `is_stakeable` to P2PKH

`is_stakeable` (`consensus.rs:147`) changes from "not OP_RETURN" to "P2PKH":

```rust
pub fn is_stakeable(utxo: &Utxo) -> bool {
    utxo.value >= MIN_STAKE_AMOUNT
        && vtorrent_script::classify_script(
            &vtorrent_script::Script::from_bytes(utxo.script_pubkey.clone()).unwrap_or_default(),
        ) == vtorrent_script::ScriptType::P2PKH
}
```

`is_stakeable` is the single source of truth for the denominator: it is called
on both the removal side (`chain_reorg.rs:290`) and the addition side
(`chain_reorg.rs:345`), so both `total_staked` and the producer/validator
denominators stay consistent. No other call sites exist.

**Replay safety.** On every existing chain this is a no-op:

- The soak chain's stakeable UTXOs are all 25-byte P2PKH (verified: the
  staker's 4 wallet UTXOs are P2PKH; all 25,176 blocks are `tx_count=1`; zero
  HTLC/swap activity in the node logs; the DEX order book is empty).
- Genesis distribution outputs are OP_RETURN and already excluded.
- The regtest faucet mints P2PKH (`mint_to_address` → `address_to_p2pkh_script`).

No activation height is needed, for the same reason the v2 kernel needed none:
the rule is strictly *narrower* than the old one, so no previously-valid block
becomes invalid. The rule applies from genesis, which is what mainnet wants.

## 4. T3 — Height-1 bootstrap claim block

### 4.1 Consensus rule

At height 1 (parent is genesis, `total_staked == 0`), a PoS block whose **first
and only transaction is a `LegacyClaim`** is valid. It is a "bootstrap block."
From height 2 on, normal PoS rules apply unchanged.

The claim is validated exactly like any other legacy claim: snapshot membership,
exact-amount match, and the v2 signature over `(claim_address, outputs)`
(`consensus.rs:407`). The claim's P2PKH output becomes the first stakeable UTXO,
so `total_staked > 0` and block 2 can be staked.

### 4.2 Validation split

The rule needs chain state (the claim path lives in the journal), so it spans
two places:

- **`validate_block_inner` (`consensus.rs:272`).** Currently a PoS block must
  begin with a `Coinstake`. Add: when `prev_height == 0`, also accept a PoS
  block whose first transaction is a `LegacyClaim`, and require exactly one
  transaction (a bootstrap block carries only the claim). All other checks
  (height, bits, modifier, merkle, timestamp, size) are unchanged.
- **`apply_block_journaled` (`chain_reorg.rs:312`).** The existing claim branch
  already validates the claim. Add: a height-1 bootstrap block must create at
  least `MIN_STAKE_AMOUNT` of stakeable (P2PKH) output, else reject. This
  guarantees the bootstrap actually unblocks staking.

### 4.3 Age exemption

The bootstrap UTXO is created at height 1, so its coin age at height 2 is ~0,
below mainnet `MIN_STAKE_AGE` (6h). Skip the `min_stake_age` check in the
coinstake path (`chain_reorg.rs:381`) when the stake input was created at
height 1 (`staked.height == 1`). The `max_stake_age` check is unchanged.

This is bounded: the only UTXOs created at height 1 are the bootstrap claim's
outputs (the block carries exactly one transaction), and they can only be
staked at height 2, so the exemption expires naturally. From height 3 on,
normal age rules apply.

### 4.4 Producer path

There is no production block-submission path today; the only block producer is
the staking loop, and the regtest faucet mints blocks directly via
`mint_to_address`. The bootstrap block therefore needs an explicit producer.

New endpoint `POST /api/v1/blockchain/bootstrap` taking the legacy WIF and a
recipient address:

1. Gate: `best_height == 0` and `total_staked == 0`. Reject otherwise (one-shot).
2. Build the claim transaction via a shared helper
   `build_legacy_claim_tx(wif, recipient)`, factored out of `submit_claim`
   (`dex.rs:214`) so the signature construction exists in exactly one place.
   `submit_claim` is refactored to call the same helper.
3. `Chain::apply_bootstrap_claim(claim_tx)` (mirrors `mint_to_address`):
   - Build the height-1 block: `nonce = 0` (PoS), `bits = GENESIS_BITS`,
     `stake_modifier = compute_stake_modifier(genesis_modifier, genesis_hash)`,
     merkle over the single claim tx, `lock_time = 1` (height).
   - Compute the post-apply UTXO root over the genesis UTXO set plus the claim
     output, so the header's `utxo_root` matches the journal (the same
     `compute_post_apply_root` logic the staking producer uses).
   - `add_block`, then emit `NodeEvent::NewBlock` (so the event bridge persists
     it) and announce `inv` to peers, mirroring the faucet's `block_submit`
     path (`mod.rs:751`).

### 4.5 Security properties

- **Permissionless.** Any legacy holder can bootstrap; the first valid claim
  wins. This is the intended launch behavior (the bootstrap is a real claim by
  a real legacy holder).
- **One-shot.** Gated to `best_height == 0`; once height 1 exists the endpoint
  is inert.
- **Replay-safe.** It only *adds* an accepted case at height 1 and changes no
  existing block. Regtest height 1 is a faucet PoW block, never a claim, so the
  soak chain replays unchanged.
- **No new trust.** The claim signature, snapshot balance, and exact-amount
  checks are the existing `validate_legacy_claim` path, unchanged.

## 5. Testing

- **T4 unit:** `is_stakeable` accepts P2PKH ≥ `MIN_STAKE_AMOUNT`; rejects P2SH,
  P2MS, P2PK, HTLC, `NonStandard`, OP_RETURN, and dust. Extend the existing
  `test_is_stakeable_excludes_op_return_and_dust` (`consensus.rs:697`).
- **T4 regression:** a chain with a non-P2PKH output ≥ `MIN_STAKE_AMOUNT` does
  not count it in `total_staked`; the existing `total_staked` tracking tests
  (`chain_tests.rs:372`) still pass.
- **T3 unit:** a height-1 PoS block whose first tx is a `LegacyClaim` validates;
  a height-1 PoS block with a `Coinstake` and no stakeable UTXO still fails; a
  height-1 bootstrap block with a sub-`MIN_STAKE_AMOUNT` claim output is
  rejected; a height-2+ block cannot use the bootstrap rule.
- **T3 age:** a coinstake spending the height-1 bootstrap UTXO at height 2 is
  accepted with ~0 age; a coinstake spending a height-2 UTXO at height 3 with
  ~0 age is still rejected.
- **T3 integration:** from a fresh `Chain::new()`, apply a bootstrap claim, then
  stake block 2 with the resulting UTXO; assert `total_staked > 0` and the chain
  advances. Assert `submit_claim` and the bootstrap endpoint produce identical
  claim transactions for the same inputs.
- **Replay:** `cargo test --workspace`; the soak chain replays with no new
  rejections (the changes are no-ops on existing blocks).

## 6. Rollout

Both changes are consensus rules but are no-ops on every existing chain, so
they can land on `main` without a fleet redeploy and without breaking the soak
window. They are **not** deployed to the soak fleet during the window (the
fleet keeps `vtorrent/node:13489d4`); they ship with the post-soak batch and
the `v2.0.0-beta.3` build. The bootstrap endpoint is inert until a fresh
mainnet genesis exists.

## 7. Open Questions

- **Bootstrap recipient.** The endpoint takes the recipient from the caller; no
  operator/foundation address is hard-coded. Confirm no launch procedure expects
  a specific first claimer.
- **Second bootstrap block.** If the first bootstrap claim's output is spent
  before block 2 (it cannot be, since no other block exists), the rule would
  need revisiting. Not reachable at height 1.
