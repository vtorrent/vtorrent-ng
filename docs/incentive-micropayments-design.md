# Torrent Incentive Micropayments — Design

Status: DRAFT (no VTR consensus change; payment layer)
Scope: `vtorrent-torrent` (incentive), `vtorrent-wallet-service` (payment),
`vtorrent-node` (payment channel/aggregation).
Motivation: the incentive pays peers **on-chain, one transaction per peer per
settlement** — every `PAYMENT_INTERVAL_SECS` (300 s) or `MIN_PAYMENT_BYTES`
(10 MB) (`incentive.rs:13,14`), `build_incentive_payment` builds a real VTR tx
(`main.rs:767`). At scale this is a fee/UTXO/mempool disaster: N peers × frequent
tiny payments. This design makes micropayments **aggregate and net**, so the
chain sees few, meaningful transactions.

## 1. What exists

- `PeerBandwidthAccount` accrues `earned`/`owed` per peer (`incentive.rs:74,85`).
- Settlement every 5 min / 10 MB → `PaymentDue` → `build_incentive_payment` → one
  on-chain tx per peer (`main.rs:767-785`).
- `docs/incentive-verification-design.md` makes the amounts *verifiable* (signed
  receipts) but not *scalable*.

## 2. Design

### 2.1 Netting (bilateral)

- If A and B both seed to each other, they each owe the other. **Net** the
  bilateral balance and pay only the difference — often zero. This alone removes
  most payments (peers in a swarm are usually mutual).
- Track a **running bilateral balance** per peer; settle only when |balance|
  exceeds a threshold (bytes or value).

### 2.2 Aggregation (batch)

- **Batch** payments to multiple peers into a **single transaction with multiple
  outputs** (one output per payee). One tx fee instead of N.
- Raise the settlement threshold (e.g. ≥ 1 VTR or ≥ 1 GB) so tiny accruals
  accumulate before paying; keep the per-peer ledger in memory/persisted.

### 2.3 Payment channels (the scalable end state)

- For frequent, high-volume peers, open a **unidirectional payment channel**
  (fund once on-chain; exchange signed balance updates off-chain; settle on
  close). The chain sees 2 txs (open/close) for arbitrarily many micropayments.
- This is the proper micropayment primitive; netting + batching are the pragmatic
  first steps, channels the end state.
- **Trust**: a channel is trust-minimized (the payee holds a signed claim); the
  payer can't unilaterally close below the last signed balance.

### 2.4 Settlement policy

- Settle when: bilateral net ≥ threshold, **or** channel capacity exhausted, **or**
  the peer disconnects (final settlement), **or** a time bound (so a peer isn't
  left unpaid indefinitely).
- Persist unsettled balances so a restart doesn't lose them.

### 2.5 Interaction with verification

- Netting/batching/channels all consume the **verified** amounts from
  `docs/incentive-verification-design.md`; they change *how* payments are made,
  not *what* is owed. Keep the two layers separate.

## 3. Adversarial review

- **R1 — netting must be exact.** A netting bug under/over-pays and breaks the
  incentive. Use integer satoshi math; test mutual-seeding scenarios.
- **R2 — batching changes the fee model.** One tx with N outputs has a different
  fee than N txs; the payer bears it. Cap N per tx (size limits) and document.
- **R3 — channels are a trust/UX shift.** A channel locks funds; the payee must
  trust the payer's funding and the payer must trust the payee not to vanish
  mid-channel. Define close/settlement and timeout.
- **R4 — unsettled balances on restart.** Persist the ledger, or a restart loses
  a peer's earnings (and trust). Test restart.
- **R5 — don't pay for unverified bytes.** Settlement consumes verified receipts
  only (ties to the verification design); netting/batching must not bypass it.
- **R6 — no VTR consensus change.** Payment layer only; the chain sees normal txs
  (or channel open/close).
- **R7 — Sybil.** Cheap identities still farm tiny payments; the verification +
  reputation layers gate this. Don't rely on payment mechanics for Sybil defense.

## 4. Test plan

- Netting: mutual seeding nets to zero; asymmetric nets to the difference.
- Batching: N payees in one tx; each output correct; fee sane; size bounded.
- Channel: open → many off-chain updates → close settles the last balance; a
  payer can't close below the last signed balance.
- Restart: unsettled balances persist and settle after reload.
- Settlement consumes only verified amounts.
- No VTR consensus change.

## 5. Non-goals

- Not a Lightning-style routed network (bilateral channels only).
- Not changing the VTR/GB pricing.
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/incentive.rs:13,14,74,85,95,103` settlement cadence + accounting.
- `vtorrent-wallet-service/src/lib.rs:52` `build_incentive_payment`.
- `vtorrent-daemon/src/main.rs:755-815` payment channel + settlement loop.
- `docs/incentive-verification-design.md` (verified amounts),
  `docs/torrent-reputation-design.md` (Sybil).
