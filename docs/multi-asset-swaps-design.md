# Multi-Asset Atomic Swaps — Design

Status: DRAFT (no VTR consensus change; swap engine + chain adapters)
Scope: `vtorrent-node` (`atomic_swap`), `vtorrent-btc` (becomes one adapter),
new chain adapters, RPC/UI.
Motivation: the DEX advertises `target_asset: String` (`atomic_swap.rs:424`) but
the engine is **BTC-hardcoded** — `maker_btc_address`, `btc_funding_txid`,
`btc_amount`, `btc_expiry`, `BtcFunding`/`BtcFunded` states, `BTC_HTLC_FEE_SATOSHIS`
(`:48`). So VTR↔BTC works and nothing else does, despite the field implying
otherwise. This generalizes swaps to other chains (LTC, DOGE, …) via a chain
adapter, and makes `target_asset` meaningful.

## 1. What exists

- `SwapOrder`/`SwapState` with `btc_*` fields and `BtcFunding`/`BtcFunded`
  (`atomic_swap.rs:522,524,545-558`).
- `vtorrent-btc`: BIP84 keys, HTLC funding/claim/refund, regtest — a full
  BTC adapter in all but name.
- `target_asset: String` on the order, carried through announcements, but not
  used to select a chain.

## 2. Design

### 2.1 Chain adapter trait

```rust
pub trait SwapChain {
    fn id(&self) -> ChainId;                 // "BTC", "LTC", …
    fn htlc_script(&self, p: &HtlcParams) -> Result<Vec<u8>>;
    fn fund(&self, p: &FundParams) -> Result<UnsignedTx>;
    fn claim(&self, p: &ClaimParams) -> Result<UnsignedTx>;
    fn refund(&self, p: &RefundParams) -> Result<UnsignedTx>;
    fn confirmations(&self, txid: &[u8;32]) -> Result<u32>;
    fn fee_policy(&self) -> FeePolicy;
    fn address_is_valid(&self, addr: &str) -> bool;
}
```

- `vtorrent-btc` implements `SwapChain` (a refactor, not a rewrite).
- New adapters (LTC, DOGE, …) implement the same trait; each is a separate crate
  with its own HTLC script template, address format, and confirmation policy.
- A **registry** maps `target_asset` → adapter; an unknown asset is rejected at
  order creation (R3).

### 2.2 Generalize the swap state machine

- Replace `btc_*` fields with a **per-counterparty-chain** structure:
  `counterparty_chain: ChainId`, `counterparty_funding_txid`,
  `counterparty_amount`, `counterparty_expiry`, `counterparty_claim_txid`,
  `counterparty_refund_txid`, `counterparty_refund_raw`, …
- `BtcFunding`/`BtcFunded` become `CounterpartyFunding`/`CounterpartyFunded`.
- The VTR side stays as-is (it's the home chain).

### 2.3 Timing invariant (the critical cross-chain rule)

- The HTLC expiries must be **ordered** so the party who reveals the preimage
  (the VTR claimer) has a **longer** window than the counterparty's refund
  window on the other chain. Otherwise a counterparty can refund after the
  preimage is revealed and steal the swapped asset.
- This ordering must be **enforced per chain pair** (the relative block times
  differ: BTC ~10 min, LTC ~2.5 min, DOGE ~1 min). Compute the ordering from each
  chain's target block time, not a fixed constant.

### 2.4 Trust model for the other chain

- Each adapter needs a way to observe its chain: a **full node** (trusted) or a
  **light client** (SPV). For BTC the daemon already runs SPV
  (`docs/btc-spv-soak-plan.md`); other chains need an equivalent or a trusted
  node.
- The confirmation policy differs per chain (reorg depth); the adapter owns it.

### 2.5 UI

- The Trade page shows the **asset pair** (VTR↔BTC, VTR↔LTC, …), the rate in the
  target asset, and the **per-chain expiry countdowns** (generalize
  `docs/atomic-swap-ux-design.md`).

## 3. Adversarial review

- **R1 — the timing invariant is safety-critical.** Wrong expiry ordering lets a
  counterparty steal the asset. Enforce it per chain pair from each chain's block
  time; test the boundary.
- **R2 — confirmation/reorg differences.** A fast chain (DOGE) reorgs deeper in
  wall-clock time; the adapter's confirmation policy must account for it, or a
  reorg breaks a "confirmed" funding.
- **R3 — `target_asset` must be validated.** A typo/unknown asset must be
  rejected at creation, not create an unfulfillable order. Registry lookup.
- **R4 — HTLC script differences.** SegWit vs legacy, sighash flags, and fee
  estimation differ per chain; the adapter owns them. Don't assume BTC's.
- **R5 — no VTR consensus change.** The VTR side is unchanged; the other chain's
  rules are external.
- **R6 — trust model must be explicit.** A trusted full node for the other chain
  is a trust assumption; SPV is weaker than full validation. State which.
- **R7 — scope.** Each new chain is real work (adapter + observation + tests);
  ship BTC first (done), then one more, and prove the abstraction before
  promising many.

## 4. Test plan

- `vtorrent-btc` implements `SwapChain` with no behaviour change (existing swap
  tests pass).
- A second adapter (e.g. a regtest LTC-like chain) completes a full swap
  end-to-end.
- Timing invariant: a swap with the wrong expiry ordering is rejected; the
  correct ordering is accepted.
- Unknown `target_asset` rejected at creation.
- Confirmation policy per chain; a reorg below the policy doesn't break the swap.
- No VTR consensus change.

## 5. Non-goals

- Not adding many chains at once (one adapter proves the abstraction).
- Not changing the VTR HTLC or the home-chain rules.
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/atomic_swap.rs:48,424,522,524,545-558` BTC-hardcoded swap.
- `vtorrent-btc/src/` (the de-facto BTC adapter).
- `docs/atomic-swap-protocol.md`, `docs/atomic-swap-ux-design.md`,
  `docs/btc-spv-soak-plan.md`.
