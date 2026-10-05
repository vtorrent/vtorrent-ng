# Mobile Companion — Design

Status: DRAFT (no consensus change; remote access + app)
Scope: `vtorrent-rpc` (scoped auth), a mobile app (PWA or native), pairing.
Motivation: the node is desktop/daemon-only; there is no way to check balances,
staking, swaps, or get alerts on a phone. But the RPC is (correctly)
**localhost-only** and includes wallet unlock/send, so exposing it naively is
dangerous. This design adds a **scoped, paired, read-first** mobile path.

## 1. Problem (grounded)

- RPC binds `127.0.0.1:22525` by default (`config.rs:22`, "localhost only for
  security") and is gated by a single `X-Api-Key` (`server.rs:41`).
- The same RPC exposes `wallet/unlock`, `wallet/send`, `swap/*` — so remote
  exposure without strong auth is a fund-loss risk.
- The UI already distinguishes Tauri vs browser (`useNode.tsx` `isTauri()`), so
  a web/PWA client is feasible — but there is no secure remote path.

## 2. Design

### 2.1 Scoped tokens (read-first)

- Split auth into **scopes**: `read` (info, balances, history, staking status,
  swap status, notifications) and `control` (unlock, send, swap fund/claim,
  staking start/stop).
- A mobile device gets a **read token by default**; `control` requires a
  separate, explicitly granted token with its own expiry.
- Tokens are **revocable** and **expiring**; the node lists active tokens.

### 2.2 Pairing

- Pair via **QR code** shown by the desktop app: node address + a one-time
  pairing secret; the phone exchanges it for a scoped token.
- **TLS required** for any non-localhost access: self-signed cert + **certificate
  pinning** in the app, or a user-provided tunnel (Tailscale/WireGuard). Never
  plaintext HTTP over the internet.

### 2.3 App

- **PWA first** (reuse the React UI, `isTauri()` already handles web mode):
  read-only dashboard — balances, staking health, swaps, notifications.
- **Native later** if push notifications / secure-enclave signing are needed.
- **Spending**: prefer **on-device signing** (secure enclave / hardware) over
  sending keys or unlock passphrases to the node. If remote control is enabled,
  require re-auth per action and show the exact tx to confirm.

### 2.4 Notifications

- Reuse `docs/notifications-design.md`: the node pushes payment/staking/swap
  alerts to the paired device (via the app's push channel or a WebSocket while
  foregrounded). Swap deadlines are the priority.

## 3. Adversarial review

- **R1 — never expose unlock/send without strong auth.** Default the mobile path
  to **read-only**; `control` is opt-in, scoped, expiring, and TLS-pinned. This
  is the whole risk of the feature.
- **R2 — the API key alone is insufficient remotely.** A static header key over
  the internet is replayable if leaked; require TLS + pinning and short-lived
  tokens.
- **R3 — keys never leave the device.** Don't send a seed/mnemonic to the node;
  sign on-device where possible (pairs with `hardware-wallet-signing-design.md`).
- **R4 — pairing is a trust bootstrap.** The QR secret must be single-use and
  short-lived; a stolen QR must not grant lasting access.
- **R5 — revocation.** Lost phone → revoke the token; the node must support it
  without restart.
- **R6 — DoS.** A public-facing RPC invites abuse; rate-limit and keep the
  default localhost-only (remote is opt-in).
- **R7 — no consensus change.** Auth + app only.

## 4. Test plan

- Read token can read; cannot unlock/send (403).
- Control token requires TLS + pinning; plaintext remote is refused.
- Pairing secret is single-use/expiring; replay rejected.
- Token revocation takes effect immediately.
- Lost-device flow: revoke → device loses access.
- No key material crosses the wire (assert on the request/response shapes).

## 5. Non-goals

- Not a hosted relay/service (a separate ops decision).
- Not changing the localhost default (remote is opt-in).
- Not part of the current soak window.

## 6. References

- `vtorrent-rpc/src/server.rs:41` `require_api_key`, `:94` auth layer.
- `vtorrent-daemon/src/config.rs:22` `--rpc-addr` (localhost default).
- `vtorrent-ui/src/hooks/useNode.tsx` `isTauri()` (web vs desktop).
- `docs/notifications-design.md`, `docs/watch-only-wallets-design.md`,
  `docs/hardware-wallet-signing-design.md`.
