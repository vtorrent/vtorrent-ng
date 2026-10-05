# Atomic-Swap / DEX UX — Design

Status: DRAFT (no consensus change; UI + RPC ergonomics)
Scope: `vtorrent-ui` (Trade page), `vtorrent-rpc` (swap ergonomics).
Motivation: the swap engine is real (7-state HTLC machine across VTR + BTC) but
the UI is a raw form — the user pastes an order id, a taker address, a **WIF**,
a BTC refund address, and a preimage. That is unsafe and unusable for anyone but
the developer. This makes cross-chain swaps a guided, safe flow.

## 1. Problem (grounded)

`TradePage.tsx` exposes a manual "swap" tab with `takerWif`, `preimage`,
`swapOrderId`, etc. as free-text inputs. The engine underneath is a proper
lifecycle — `SwapStatus` (`atomic_swap.rs:516`): `Funding → VtrFunded →
BtcFunding → BtcFunded → Settling → Claimed | Refunded` — with a BTC expiry
(`btc_expiry`) that is **money at risk** if missed. None of that is surfaced.

Specific hazards:
- **WIF in the browser.** The UI must never take a raw private key.
- **Deadline blindness.** A missed `btc_expiry` can strand funds; there is no
  countdown.
- **Opaque state.** The user can't see which chain is funded or what to do next.
- **Manual recovery.** A stalled swap needs the right action (claim vs refund)
  at the right time.

## 2. Design

### 2.1 Guided swap wizard

Replace the raw form with a wizard:
1. **Pick** an order from the book (or paste a swap link).
2. **Review** terms: amounts, rate, both HTLC expiries, fees.
3. **Fund** — one click; the wallet signs (no WIF in the UI).
4. **Track** — a live tracker (below).

### 2.2 Live swap tracker

- A **state-machine visual**: the 7 `SwapStatus` states as a progress rail, with
  the current state, both chains' funding/claim txids, and confirmations.
- **Deadline countdowns**: VTR and BTC HTLC expiries, colour-coded, with an
  escalating warning as `btc_expiry` approaches.
- **One-click actions** contextual to state: *Fund BTC*, *Claim*, *Refund* —
  enabled only when valid, with a plain-language explanation of the consequence.
- **Recovery**: if a swap stalls, the tracker shows the safe action (refund
  before expiry) and why.

### 2.3 RPC ergonomics

- The UI should not need to construct raw swap params. Add a **prepare** endpoint
  that returns the exact unsigned funding/claim/refund txs + a summary, so the
  UI only confirms and the wallet signs.
- Reuse the existing automatic BTC monitoring (already in the daemon) to drive
  the tracker; the UI subscribes to swap status over the existing WS.

## 3. Adversarial review

- **R1 — never take a WIF in the UI.** Signing stays in the wallet backend
  (consistent with the project's "keys never reach the JS frontend" rule). The
  current `takerWif` field is a bug to remove, not a feature to style.
- **R2 — deadline safety is the whole point.** The countdown and the escalating
  warning must be impossible to miss; a swap near expiry should be visually
  dominant. Test that a swap at T-1h is unmistakable.
- **R3 — action gating.** *Claim*/*Refund* must be enabled only when the state
  and time allow; a wrong action can lose funds. Gate on the same predicates the
  engine enforces.
- **R4 — cross-chain confirmation ambiguity.** BTC funding "broadcast
  acceptance may still be unknown" (`SwapStatus::BtcFunding` doc). The UI must
  show *unconfirmed* distinctly from *confirmed*, never implying safety before
  confirmations.
- **R5 — recovery for stalled swaps.** A swap whose counterparty vanished must
  lead the user to the refund path before expiry, with the deadline front and
  centre.
- **R6 — no consensus change.** UI + RPC ergonomics over the existing engine.

## 4. Test plan

- Wizard builds a swap with no WIF input; wallet signs.
- Tracker renders each of the 7 states correctly with the right actions enabled.
- Deadline countdown escalates; a T-1h swap is visually dominant.
- Unconfirmed BTC funding is shown as unconfirmed, not funded.
- A stalled swap surfaces the refund path with the deadline.
- Claim/Refund disabled when the state/time forbids it.

## 5. Non-goals

- Not changing the swap protocol or HTLC timing.
- Not a full order-book redesign (the book tab is separate).
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/atomic_swap.rs:516` `SwapStatus`, `:535` `SwapState`.
- `vtorrent-rpc/src/handlers/swap.rs` fund/claim/refund handlers.
- `vtorrent-ui/src/pages/TradePage.tsx:42` (current raw swap form).
- `docs/atomic-swap-protocol.md` (protocol).
