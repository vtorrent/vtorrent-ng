# Transaction Preview & Simulation — Design

Status: DRAFT (no consensus change; wallet-service + RPC + UI)
Scope: `vtorrent-wallet-service`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: `send_vtr` (`handlers/wallet.rs:343`) builds, signs, and broadcasts
in one call — the user never sees the **exact** transaction (inputs, outputs,
change, fee) before it's irreversible. For a custody product, a **preview/dry-run**
step is essential: it catches wrong addresses, wrong amounts, and surprising
fees, and it's the foundation for hardware signing (the device must display what
it signs).

## 1. What exists

- `tx_builder`: `select_coins` (`tx_builder.rs:62`), fee estimation, change,
  `build_script_sig` (`:141`).
- `send_vtr` (`handlers/wallet.rs:343`): auth → select UTXOs → build → sign →
  broadcast, no preview.
- `SendRequest.memo` (`models.rs:164`) — unused.

## 2. Design

### 2.1 Preview (dry-run)

- `POST /api/v1/wallet/preview` (or `?dry_run=true` on send): run the **same**
  build path but **stop before signing/broadcast**, returning:
  - inputs (txid:vout, value),
  - outputs (recipient, amount; change address, amount),
  - **absolute fee** and **fee rate**,
  - total in / total out, and the **change**,
  - a **txid** (of the unsigned tx) so the user can confirm the exact bytes.
- The UI shows this and requires an explicit **Confirm & Send**.
- **Same code path**: preview must use the identical build logic as send, or the
  preview lies (the classic bug). Factor the build into a shared function that
  returns the unsigned tx + metadata; `send` signs+broadcasts it.

### 2.2 Simulation (policy checks)

- Before signing, **simulate** the tx against the mempool/chain policy:
  - fee rate ≥ `min_fee_rate` (would it be relayed?),
  - no double-spend of an already-spent UTXO,
  - outputs above dust,
  - recipient address valid + on the right network,
  - (optional) the tx would be accepted by `add_block`'s validation.
- Report any policy failure in the preview so the user fixes it before signing.

### 2.3 Anti-fat-finger

- Show the **fee as a percentage of the amount** and warn on unusually high fees
  or a fee that exceeds the amount.
- Show the **resolved recipient** (contact/name if known,
  `docs/wallet-organization-design.md`), never just a raw address.

### 2.4 Hardware/PSBT synergy

- The preview is exactly what a hardware device must display
  (`docs/hardware-wallet-signing-design.md`): the unsigned tx + inputs/outputs.
  Reuse the preview structure as the PSBT-lite payload.

## 3. Adversarial review

- **R1 — preview must use the same build path.** A separate "preview" builder
  that diverges from the real one is worse than no preview. Share the code; test
  that preview and send produce the identical unsigned tx (same txid).
- **R2 — the txid must be of the exact bytes.** Show the txid of the unsigned tx
  so the user confirms *those* bytes; if signing changes anything, that's a bug.
- **R3 — simulation must not be authoritative.** A "would be relayed" check is
  best-effort (the mempool changes); don't imply a guarantee. It catches obvious
  errors, not races.
- **R4 — no secrets in the preview.** Preview must not require or expose the WIF;
  signing is a separate, authenticated step.
- **R5 — fee honesty.** Show absolute fee + rate + % of amount; a preview that
  hides the fee is not a preview.
- **R6 — no consensus change.** Wallet-service/RPC/UI only.

## 4. Test plan

- Preview and send produce the **same unsigned txid** for the same request.
- Preview shows inputs/outputs/change/fee exactly; totals balance.
- Policy simulation flags a below-min-fee tx, a double-spend, a dust output, and
  a wrong-network address.
- Fat-finger warning fires for a fee > amount.
- Preview requires no WIF; send requires auth.
- No consensus/chain change.

## 5. Non-goals

- Not a full script interpreter for arbitrary txs (P2PKH/P2CS/P2SH covered).
- Not a guarantee of relay (best-effort simulation).
- Not part of the current soak window.

## 6. References

- `vtorrent-rpc/src/handlers/wallet.rs:343` `send_vtr`.
- `vtorrent-wallet/src/tx_builder.rs:62,141` `select_coins`, `build_script_sig`.
- `vtorrent-rpc/src/models.rs:164` `SendRequest.memo`.
- `docs/hardware-wallet-signing-design.md`, `docs/wallet-organization-design.md`,
  `docs/fee-market-design.md`.
