# Torrent Incentive Verification — Design

Status: DRAFT (no consensus change; torrent protocol + accounting)
Scope: `vtorrent-torrent` (peer wire, incentive), `vtorrent-wallet-service`
(payment), RPC/UI.
Motivation: the headline feature — **pay peers for bandwidth** — is currently
**unilateral and unverifiable**. A peer can claim uploads it never made, and a
payer cannot prove it paid for real data. This design makes the incentive
accounting **reciprocal and receipt-based**.

## 1. Problem (grounded)

`PeerBandwidthAccount` (`incentive.rs:19`) records `bytes_uploaded` /
`bytes_downloaded` **from our own counters only**, and `calculate_earned`
(`:74`) / `calculate_owed` (`:85`) price them at 1 VTR/GB and 0.5 VTR/GB. The
settlement loop (`main.rs:790`) then **pays real VTR on-chain** for those
self-reported bytes. There is:

- no proof a peer actually served the bytes,
- no agreement on the amount between the two parties,
- no way to dispute or audit a payment.

So a malicious peer can inflate `bytes_uploaded` (or a payer can under-report
`bytes_downloaded`) with nothing to check against. For a chain whose pitch is
"earn by seeding," that is the core economic weakness.

## 2. Design — reciprocal receipts over BEP-10

The peer wire already supports extension messages (`PeerMessage::Extended`,
BEP-10, `peer_wire.rs:41`). Use them for a lightweight **receipt** handshake:

### 2.1 Signed bandwidth receipts

- Each side periodically (per settlement window) sends a **receipt**: a signed
  statement of the bytes it observed in each direction over the window.
  ```
  Receipt {
    info_hash: [u8;20],
    window: (start, end),
    uploaded_by_me: u64,     // bytes I sent you
    downloaded_by_me: u64,   // bytes I received from you
    peer: <address>,
    sig: <signature over the above>,
  }
  ```
- The **payer** only pays when the receipt is **countersigned by the payee**
  (mutual agreement), or at least when the payee's own receipt **agrees within a
  tolerance**. Disagreement → no payment (or a reduced, disputed amount).
- Receipts are cheap (one message per window) and ride the existing extension
  channel.

### 2.2 What this buys

- **Bilateral agreement**: both parties sign the same numbers, so neither can
  unilaterally inflate.
- **Auditability**: receipts are evidence a payment was for real, agreed bytes.
- **Dispute handling**: a mismatch is detectable and can be surfaced (and the
  payment withheld) instead of silently over/under-paying.

### 2.3 What it does *not* buy (be honest)

- It does **not** prove the bytes were *useful* (a peer could upload junk). That
  needs a piece-level proof (see §4), which is heavier.
- It does **not** stop Sybil (many identities) — that's a separate reputation
  problem.

## 3. Settlement changes

- `needs_settlement` (`incentive.rs:95`) also requires a **matching receipt**
  (or the tolerance window elapsed with no dispute).
- `settle` (`:103`) records the **agreed** amount, not the self-reported one.
- The payment path (`wallet-service::build_incentive_payment`) is unchanged
  except the amount comes from the agreed receipt.
- Persist recent receipts (bounded) so a restart doesn't lose an in-flight
  settlement.

## 4. Optional stronger proof (piece-level)

For high-value transfers, a **piece-level proof**: the receiver signs the
`info_hash` + piece index + a hash of the received piece; the sender's receipt
cites the receiver's piece acknowledgements. This proves *useful* data moved,
at the cost of more messages. Gate behind a per-session "verified" mode; default
to the cheap receipt handshake.

## 5. Adversarial review

- **R1 — receipts must be signed by the party they bind.** A receipt is only
  meaningful if the *payee* signs the bytes *they received*, and the *payer*
  signs the bytes *they sent*; the payment uses the intersection/agreement. A
  self-signed "I uploaded 10 GB" is worthless.
- **R2 — replay / window binding.** Receipts must bind `info_hash`, peer
  address, and a window, and be single-use per settlement, or a peer replays an
  old receipt to get paid twice. Include a nonce/settlement id.
- **R3 — tolerance policy.** Exact agreement is unrealistic (lost packets,
  retransmits). Define a tolerance (e.g. ±5%) and the dispute outcome; document
  it. No tolerance → no one gets paid; too loose → inflation.
- **R4 — Sybil is out of scope but must be named.** Receipts don't stop a peer
  from farming many identities; that's reputation/identity, a separate design.
- **R5 — don't stall the data path.** Receipt exchange is per-window, not
  per-piece; keep it off the hot path and bounded.
- **R6 — no consensus change.** This is peer-protocol + accounting; the chain
  only sees the resulting payment tx.
- **R7 — back-compat.** Peers without the extension fall back to today's
  unilateral accounting (or no payment), negotiated via BEP-10 capability bits.

## 6. Test plan

- Receipt build/verify round-trip; tampered bytes or window rejected.
- Settlement requires a matching receipt; a mismatched receipt withholds
  payment; within-tolerance agrees.
- Replayed receipt is rejected (nonce/window).
- Peer without the extension falls back cleanly.
- Piece-level mode: a receiver's piece ack is required for the sender's receipt.

## 7. Non-goals

- Not a full reputation/Sybil system (named as R4, separate).
- Not changing the VTR/GB pricing.
- Not part of the current soak window.

## 8. References

- `vtorrent-torrent/src/incentive.rs:19,74,85,95,103` bandwidth accounting.
- `vtorrent-torrent/src/peer_wire.rs:41` `Extended` (BEP-10).
- `vtorrent-daemon/src/main.rs:755-815` payment channel + settlement loop.
- `vtorrent-torrent/src/payment.rs` `PaymentDue`/`PaymentSender`.
