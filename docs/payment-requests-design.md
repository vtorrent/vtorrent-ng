# Payment Requests & Invoices — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: to receive VTR today a user copies an address and tells the sender
the amount out-of-band. There is **no payment URI, no QR, no request/invoice** —
`qrcode.react` is already a dependency (used for TOTP) but not for receiving.
This makes "request payment" a first-class, safe flow (and pairs with the
torrent/swap/DEX flows that all need "pay this address this amount").

## 1. What exists

- `get_addresses` (`handlers/wallet.rs:146`), `get_balance` (`:13`),
  `send_vtr` (`:343`), `get_wallet_utxos` (`:103`).
- `qrcode.react` in the UI (`package.json:17`), used for TOTP
  (`SecurityCenterPage.tsx:146`).
- No URI scheme, no request object, no receive QR.

## 2. Design

### 2.1 Payment URI

- Define a URI scheme:
  ```
  vtorrent:<address>?amount=<VTR>&label=<name>&message=<note>&req=<id>
  ```
  - `amount` in VTR (decimal, 8 dp) — parse to satoshis exactly (no float).
  - `label`/`message` are display-only, URL-encoded.
  - `req` is an optional request id (see §2.3).
- Parse/build in the wallet (one place), so the UI, CLI, and any external app
  agree. Reject foreign-network addresses (the builder already does this for
  P2PKH).

### 2.2 Receive QR

- A **Receive** panel: pick an address (default: a **fresh** one), enter an
  amount/label, and render a QR of the URI (reuse `qrcode.react`).
- Show the address as text too (QR scanning fails sometimes); copy button.
- **Fresh-address default** (address-reuse hygiene, ties to
  `docs/privacy-design.md`).

### 2.3 Requests / invoices

- A **request** is a small object `{ id, address, amount, label, message,
  created_at, status }` persisted in the wallet (or a sidecar), so the sender can
  include `req=<id>` and the receiver can match the incoming payment to the
  request.
- **Status**: pending → paid (matched by an incoming tx to the address with the
  amount) → expired. Ties into `docs/notifications-design.md` ("payment
  received").
- **Invoices** are requests with a memo; exportable as a URI/QR or a link.

### 2.4 Send integration

- The Send flow accepts a **URI** (paste or scan), pre-filling address + amount +
  label, and shows the label/message so the user confirms what they're paying.
- **Anti-phishing**: display the resolved address prominently and warn if the
  label doesn't match the address's saved label (address-book tie-in).

## 3. Adversarial review

- **R1 — amount parsing must be exact.** VTR↔satoshi conversion via floats loses
  precision (the classic bug). Parse the decimal string to satoshis with integer
  math and test edge cases (0.00000001, large values).
- **R2 — URI is attacker-controllable.** A pasted/scanned URI can carry a
  malicious address/label. Always show the resolved address and amount for
  confirmation; never auto-send from a URI.
- **R3 — request matching must be robust.** Match on address + amount + a time
  window; a partial/over payment should be flagged, not silently "paid".
- **R4 — fresh-address default.** Reusing one address links payments; default to
  a fresh address per request (privacy).
- **R5 — no consensus change.** Wallet/RPC/UI only.
- **R6 — persistence bounds.** Requests are bounded (ring buffer / expiry);
  don't grow forever.

## 4. Test plan

- URI build/parse round-trip; exact satoshi conversion at 8 dp; foreign-network
  address rejected.
- QR renders the URI; scanning it in the Send flow pre-fills correctly.
- Request lifecycle: created → matched by an incoming tx → paid; expired if not.
- Over/under payment flagged, not marked paid.
- Fresh address per request; label mismatch warns.
- No consensus/chain change.

## 5. Non-goals

- Not a Lightning-style off-chain payment channel.
- Not a hosted invoicing service.
- Not part of the current soak window.

## 6. References

- `vtorrent-rpc/src/handlers/wallet.rs:13,103,146,343` balance/utxos/addresses/send.
- `vtorrent-ui/package.json:17` `qrcode.react`; `SecurityCenterPage.tsx:146` QR usage.
- `docs/privacy-design.md` (address reuse), `docs/notifications-design.md`
  (payment-received alerts), `docs/watch-only-wallets-design.md` (labels).
