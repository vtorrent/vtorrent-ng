# Webhooks & Integrations — Design

Status: DRAFT (no consensus change; daemon + RPC)
Scope: `vtorrent-daemon` (event delivery), `vtorrent-rpc` (config/management).
Motivation: node events (`NodeEvent`: `NewBlock`, `TxConfirmed`, …) are bridged to
the RPC WebSocket (`main.rs:371`) but there is **no outbound delivery** — an
operator can't wire "on incoming payment, call my service" or "on swap deadline,
page me." `reqwest` is already a dependency. Webhooks make the node
**programmable** for operators, exchanges, and integrations.

## 1. What exists

- `NodeEvent` (`vtorrent-node/src/events.rs`) → RPC WS broadcaster
  (`main.rs:371-384`).
- `reqwest` (`Cargo.toml:90`) available in the daemon.
- No webhook config, no outbound HTTP, no retry/backoff.

## 2. Design

### 2.1 Subscriptions

- A **subscription**: `{ id, url, secret, event_filter, active }`.
  - `event_filter`: which categories/events (e.g. `payment.received`,
    `swap.deadline`, `staking.stopped`, `block.new`).
  - `secret`: an HMAC key to sign deliveries.
- Managed via RPC (`POST /api/v1/webhooks`, list/delete) and/or a config file.

### 2.2 Delivery

- On a matching event, **POST** a JSON payload:
  `{ id, event, data, timestamp, delivery_id }`.
- **Sign** the body with `HMAC-SHA256(secret, body)` in a header
  (`X-VTR-Signature`), so the receiver can verify authenticity and reject
  spoofed deliveries.
- **Retry** with exponential backoff on non-2xx/timeout, bounded attempts, then
  mark failed (and surface via `docs/notifications-design.md`).
- **Idempotency**: include a `delivery_id` so receivers can dedupe retries.

### 2.3 Delivery guarantees

- **At-least-once**, not exactly-once (retries can duplicate) — receivers must
  dedupe on `delivery_id`. State this.
- **Bounded queue**: if a receiver is slow, the queue is bounded and drops
  oldest (or pauses the subscription) — never unbounded memory.
- **Off the hot path**: delivery runs in a background task; a slow webhook must
  never stall block processing (same principle as the store bridge).

### 2.4 Security

- **SSRF**: a webhook URL is operator-supplied, but still validate it (reject
  non-HTTP(S), and by default reject private/loopback targets unless explicitly
  allowed — reuse the tracker's SSRF guard pattern,
  `vtorrent-torrent/src/tracker.rs:118`).
- **Secrets**: the HMAC secret is stored encrypted/0600; never logged.
- **Payload privacy**: payloads may contain addresses/amounts; document what's
  sent and make it opt-in per subscription.

### 2.5 Use cases

- **Payments**: notify an exchange/service on `payment.received`.
- **Ops**: page on `staking.stopped`, `swap.deadline`, `node.no_peers`.
- **Indexers**: stream `block.new` / `tx.confirmed` to an external indexer.

## 3. Adversarial review

- **R1 — sign deliveries.** Without HMAC, a receiver can't distinguish a real
  node delivery from an attacker's POST. Sign and document verification.
- **R2 — at-least-once, so dedupe.** Retries duplicate; receivers must dedupe on
  `delivery_id`. Don't claim exactly-once.
- **R3 — never block the event loop.** Delivery is async and bounded; a slow/hung
  receiver must not stall the node (the store bridge already learned this).
- **R4 — SSRF.** Validate webhook URLs; a malicious config could turn the node
  into an SSRF client. Reuse the tracker guard.
- **R5 — bounded queue.** A dead receiver must not grow memory; bound and drop
  (or pause) with a clear policy.
- **R6 — secret handling.** HMAC secrets are sensitive; store securely, never log.
- **R7 — no consensus change.** Daemon/RPC only.

## 4. Test plan

- Delivery on a matching event; signature verifies; a tampered body fails.
- Retry with backoff on failure; bounded attempts; `delivery_id` stable across
  retries.
- Slow/hung receiver doesn't stall the node; queue stays bounded.
- SSRF: a private/loopback URL is rejected unless explicitly allowed.
- Secret never appears in logs.
- No consensus/chain change.

## 5. Non-goals

- Not a message queue / not Kafka.
- Not exactly-once delivery.
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/events.rs` `NodeEvent`; `vtorrent-daemon/src/main.rs:371-384`
  event bridge.
- `Cargo.toml:90` `reqwest`.
- `vtorrent-torrent/src/tracker.rs:118` SSRF guard pattern.
- `docs/notifications-design.md` (delivery failures), `docs/mobile-companion-design.md`.
