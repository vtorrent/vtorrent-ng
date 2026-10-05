# Multisig Wallets — Design

Status: DRAFT (no consensus change if P2SH is already valid; wallet + RPC + UI)
Scope: `vtorrent-wallet` (address creation, partial signing), `vtorrent-rpc`,
`vtorrent-ui`.
Motivation: the script engine already has M-of-N multisig
(`build_p2ms`, `ScriptType::P2MS`, `exec_checkmultisig`), but the **wallet only
builds and signs P2PKH** — so there is no way to create a shared wallet. Multisig
is the custody primitive for shared treasuries, joint accounts, escrow, and
script-enforced staking pools.

## 1. What exists

- Script: `build_p2ms(m, keys)` (`standard.rs:121`), `ScriptType::P2MS { m, n }`
  (`:24`), `exec_checkmultisig` (`engine.rs:483`), P2SH classification
  (`standard.rs:48`).
- Wallet: `tx_builder.rs` builds/signs **P2PKH only** (`sign_input` `:129`,
  `P2PKH_INPUT_SIZE` `:47`). No M-of-N address, no partial signing, no combine.
- `is_stakeable` is P2PKH-only (`consensus.rs:149`).

## 2. Design

### 2.1 P2SH-wrapped multisig

- Use **P2SH(P2MS)** (not bare P2MS): the address commits to `HASH160(redeem)`;
  the redeem script is revealed at spend. Bare multisig is non-standard/large;
  P2SH is the norm and the engine already classifies P2SH.
- **Confirm the P2SH spend path is consensus-enabled** (the engine executes the
  revealed redeem script) — this is a prerequisite, not an assumption (§5 R1).

### 2.2 Wallet: create + sign

- **Create**: collect N cosigner pubkeys (from addresses/xpubs), choose M, derive
  the P2SH address, and persist a `MultisigEntry { address, m, n, pubkeys, redeem }`.
- **Receive**: the P2SH address is a normal receive target; balances resolve by
  scriptPubKey as today.
- **Spend**: build the tx, produce **partial signatures** per cosigner
  (`<sig_i> …`), assemble the scriptSig as `OP_0 <sig…> <redeem>` (the `OP_0`
  dummy for the CHECKMULTISIG off-by-one), and broadcast once M sigs are present.
- **Partial-signature exchange**: cosigners exchange the unsigned tx (or a
  PSBT-lite, `docs/hardware-wallet-signing-design.md`) and return their sigs;
  combine offline. No cosigner needs the others' keys.

### 2.3 Signer integration

- Extend the `Signer` abstraction (`hardware-wallet-signing-design.md`) so a
  multisig input is signed by each cosigner's signer (local or device) and the
  partial sigs are combined. This is where multisig and hardware signing meet.

### 2.4 Staking / pools

- `is_stakeable` is P2PKH-only, so a P2SH multisig UTXO **cannot stake** today.
  A multisig **pool script** (for `docs/staking-pools-design.md`) would need
  `is_stakeable` to accept the pool script — a consensus change. Keep multisig
  (custody) and pooled staking (consensus) as separate steps.

## 3. Adversarial review

- **R1 — confirm P2SH is consensus-valid before building on it.** The engine
  classifies P2SH and has CHECKMULTISIG, but the design must *verify* the spend
  path is enabled and tested end-to-end, or multisig is unusable.
- **R2 — the CHECKMULTISIG dummy.** `OP_0` must precede the sigs (the classic
  off-by-one); omitting it invalidates every multisig spend. Test.
- **R3 — signature ordering.** CHECKMULTISIG requires sigs in the same order as
  the pubkeys; partial sigs must be ordered (or the script must handle any
  order). Define and test.
- **R4 — redeem-script binding.** The P2SH address must commit to the exact
  redeem script (M, N, keys, order); a mismatch makes funds unspendable. Verify
  on create and on spend.
- **R5 — no key aggregation.** This is plain multisig, not Schnorr/MuSig; M
  signatures are on-chain (larger, more expensive). State it.
- **R6 — cosigner key hygiene.** Each cosigner holds only their own key; the
  wallet must never require all keys on one host (that defeats multisig).
- **R7 — consensus surface.** Multisig *spending* is no consensus change if P2SH
  is already valid; multisig *staking* (pool script) is a consensus change.

## 4. Test plan

- Create 2-of-3; the P2SH address commits to the redeem; a wrong redeem is
  rejected.
- Spend with 2 sigs succeeds; with 1 fails; the `OP_0` dummy is required.
- Sig ordering matches pubkey order; out-of-order is handled or rejected clearly.
- Partial-signature exchange: three cosigners, each signs locally, combine → valid.
- A device signer produces a partial sig that combines (ties to hardware design).
- No consensus change for spending; staking a multisig UTXO is rejected (today).

## 5. Non-goals

- Not Schnorr/MuSig aggregation.
- Not multisig *staking* (consensus; separate).
- Not part of the current soak window.

## 6. References

- `vtorrent-script/src/standard.rs:24,48,121` P2MS/P2SH/`build_p2ms`;
  `engine.rs:483` `exec_checkmultisig`.
- `vtorrent-wallet/src/tx_builder.rs:47,129` (P2PKH-only signing).
- `vtorrent-node/src/consensus.rs:149` `is_stakeable` (P2PKH-only).
- `docs/hardware-wallet-signing-design.md`, `docs/staking-pools-design.md`.
