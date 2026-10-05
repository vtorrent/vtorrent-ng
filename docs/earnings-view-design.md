# Unified Earnings View — Design

Status: DRAFT (no consensus change; UI + one RPC aggregator)
Scope: `vtorrent-rpc` (aggregation endpoint), `vtorrent-ui` (new page).
Motivation: vTorrent's pitch is *earning* — staking rewards, torrent seeding
incentives, and swap P&L — but there is no single place that shows it. The data
exists across three subsystems; this surfaces it in one view with a claim action.

## 1. What already exists (grounding)

| Source | Endpoint / field | Notes |
|---|---|---|
| Staking rewards | `GET /api/v1/staking/rewards` (`handlers/staking.rs:199`) | scans recent blocks for coinstakes; `StakingRewardItem` |
| Torrent incentives | `GET /api/v1/torrent/sessions` (`handlers/torrent.rs:12`) | `vtr_earned_satoshis`, `vtr_paid_satoshis` per session (`incentive_summary`) |
| Swap P&L | `GET /api/v1/swap/:id/status`, `/swap/reconcile` | `SwapStatus`; realised on claim/refund |
| Wallet | `GET /api/v1/wallet/balance`, `/wallet/transactions` | balances + history |

So the pieces are all present; the gap is **aggregation + presentation + a
single claim action**.

## 2. Design

### 2.1 RPC: `GET /api/v1/earnings/summary`

One authenticated endpoint returning a rolled-up, time-windowed view:

```json
{
  "window": "30d",
  "staking": { "rewards_vtr": "…", "blocks": 123, "claimable_vtr": "…" },
  "torrents": { "earned_vtr": "…", "paid_vtr": "…", "sessions": 4,
                "uploaded_bytes": 123456, "downloaded_bytes": 234567 },
  "swaps":    { "realised_pnl_vtr": "…", "open": 1, "completed": 3 },
  "total_earned_vtr": "…",
  "as_of_height": 31372
}
```

- Reuses the existing handlers internally (no new chain scanning logic beyond
  what `staking/rewards` already does); sums torrent `incentive_summary` across
  sessions; nets swap realised P&L from completed swaps.
- **Windowed**: `?window=24h|7d|30d|all`, default 30 d. Staking rewards are
  already height-bounded; torrent/swap sums are filtered by timestamp.
- **Claimable vs earned**: staking rewards are already in the wallet (coinstake
  outputs); "claimable" is a UI concept for torrent/swap accruals if those are
  escrowed — confirm per subsystem (§4).

### 2.2 UI: "Earnings" page

- A new nav entry (alongside Dashboard/Torrents/Trade/Staking).
- **Headline**: total earned (VTR) over the selected window, with a sparkline.
- **Three cards**: Staking (rewards, blocks, est. annualised), Torrents (earned,
  paid, ratio, bytes), Swaps (realised P&L, open/completed).
- **Activity table**: recent earning events (stake rewards, seeding payouts,
  swap completions) with a **Claim** action where applicable.
- Reuses existing components (HealthStrip style, the wallet hook).

## 3. Why this is the right quick win

- **No consensus change**, no chain-format change — pure RPC + UI.
- It makes the product's core value legible, which matters for mainnet
  onboarding and for the explorer/faucet story.
- It exercises the three subsystems together, surfacing inconsistencies (e.g.
  units, satoshi-vs-VTR, escrow semantics) before they harden.

## 4. Open questions (resolve before implementing)

- **Units**: torrent incentives are in **satoshis** (`vtr_earned_satoshis`);
  staking rewards are VTR. Pick one canonical unit (VTR, 8 dp) and convert at
  the boundary; document it.
- **Escrow semantics**: are torrent/swap earnings *already paid* to the wallet,
  or *accrued and claimable*? The claim action only makes sense if accrued.
  Verify in `vtorrent-torrent` (incentive payout path) and the swap flow.
- **Double counting**: a swap that pays VTR to the wallet and a torrent payout
  are distinct; ensure the aggregator doesn't count wallet balance changes *and*
  subsystem accruals twice.
- **Cost**: `staking/rewards` scans blocks; bound the window and cache per tip
  (the same per-tip caching pattern as `perf/stake-utxo-cache`).
- **Auth**: reuse the existing wallet-auth middleware; no new key material.

## 5. Adversarial review

- **R1 — don't invent a second source of truth.** The summary must *sum* the
  existing subsystem numbers, not recompute them differently, or the page will
  disagree with the per-subsystem pages. Reuse the handlers.
- **R2 — units bug is the likely failure.** satoshi/VTR mixing is the classic
  error here (see the `evm-token-decimals` class of bugs); centralise conversion
  and test it.
- **R3 — "claimable" must be real.** If torrent/swap earnings are already in the
  wallet, a Claim button is misleading. Gate the action on actual escrow.
- **R4 — windowing vs chain scan cost.** An `all` window over a long chain is
  expensive; cap it (e.g. last N blocks) or make it explicitly best-effort.
- **R5 — no consensus/security surface.** This is read-only aggregation + a UI
  page; keep it that way (no new signing paths).

## 6. Test plan

- Aggregator unit tests: sums match the underlying handlers for a fixture;
  window filtering; unit conversion (sat→VTR) exact.
- No double counting across wallet balance and subsystem accruals.
- UI: renders with zero earnings, partial data, and a locked wallet.

## 7. Non-goals

- Not a tax/accounting export (a natural follow-on).
- Not changing any earning mechanism.
- Not part of the current soak window.

## 8. References

- `vtorrent-rpc/src/handlers/staking.rs:199` `get_staking_rewards`.
- `vtorrent-rpc/src/handlers/torrent.rs:12` `list_torrent_sessions`
  (`vtr_earned_satoshis`/`vtr_paid_satoshis`).
- `vtorrent-rpc/src/handlers/swap.rs` swap status/reconcile.
- `vtorrent-torrent/src/session.rs:61` `TorrentSession`, `:129` `incentive_summary`;
  `vtorrent-torrent/src/incentive.rs:117` `IncentiveSummary`.
- `vtorrent-ui/src/components/Layout.tsx:13` nav; `vtorrent-ui/src/pages/`.
