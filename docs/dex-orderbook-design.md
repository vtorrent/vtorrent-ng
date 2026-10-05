# DEX Order Book & Matching — Design

Status: DRAFT (no consensus change; node order book + RPC + UI)
Scope: `vtorrent-node` (`SwapOrderBook`), `vtorrent-rpc`, UI.
Motivation: the order book is a flat `Vec<SwapOrder>` (`atomic_swap.rs:813`)
with `list_open_orders` unsorted, eviction by expiry, and a direct
maker→taker match. There is **no price-time priority, no partial fills, and no
depth aggregation**, so the UI can't show a real book and takers can't reason
about the best price. This makes the DEX usable.

## 1. What exists

- `SwapOrder` (`atomic_swap.rs:414`): `order_id`, `maker_address`,
  `maker_btc_address`, `vtr_amount`, `target_asset`, `target_amount`,
  `hash_lock`, `expiry`, `status`.
- `OrderStatus` (`:499`): `Open → Funding → Matched → InProgress → Completed |
  Cancelled`.
- `SwapOrderBook` (`:813`): `add_order`, `evict` (by expiry), `list_open_orders`
  (unsorted), `cancel_order`, `get_order`, `restore_order`.
- `OrderAnnouncement` (`:446`) for P2P gossip (excludes secrets).

## 2. Design

### 2.1 Price-time priority

- Maintain the book **sorted** by price (rate = `target_amount / vtr_amount`,
  exact integer/rational comparison — no floats), then by `created_at`
  (time priority) for equal price.
- `list_open_orders` returns the sorted book; add `best_bid`/`best_ask`.
- **Rate comparison must be exact**: compare `a.target * b.vtr` vs
  `b.target * a.vtr` (u128) to avoid rounding.

### 2.2 Order types

- **Limit** (current): a fixed rate.
- **Market** (optional): "fill at the best available" — the taker accepts the
  best open order(s) up to an amount. Implemented at match time, not stored as a
  resting order.
- **Partial fills**: an order can be filled in parts. Since each swap is an
  independent HTLC, a partial fill is a **smaller swap against the same order**;
  track `filled_vtr` / `remaining_vtr` and keep the order `Open` until fully
  filled or expired. This is the main model change.

### 2.3 Depth aggregation

- Aggregate open orders into **price levels** (rate → total VTR available) for
  the UI's depth chart / ladder. Read-only projection over the sorted book.

### 2.4 Matching

- **Direct match** (current): a taker picks an order and the HTLC flow runs.
- Keep matching **off-chain** (the chain only sees the HTLCs); the book is a
  gossip/announcement layer. No on-chain matching engine.
- **Anti-front-running**: the maker's HTLC is funded after matching; the
  hash-lock/preimage flow already prevents a matcher from stealing. Document that
  the book is public and orders are visible (no hidden orders).

### 2.5 UI

- A real **order book**: bids/asks ladder, depth, spread, last price.
- **Place order** (limit), **take** (click a level → guided swap wizard,
  `docs/atomic-swap-ux-design.md`), and **my orders** with fill status.

## 3. Adversarial review

- **R1 — exact rate comparison.** Floats round and mis-order the book; use
  integer cross-multiplication. Test with adversarial rates.
- **R2 — partial fills vs HTLC atomicity.** Each partial fill is a separate
  swap; the order must not be double-filled. Track `remaining_vtr` and settle
  atomically per fill; test concurrent takers.
- **R3 — order book is public.** Resting orders reveal intent; there are no
  hidden orders. State it (privacy).
- **R4 — eviction must not drop funded orders.** `evict` (`:825`) removes by
  expiry; a `Matched`/`InProgress` order must never be evicted (funds at risk).
  Verify the status filter.
- **R5 — no consensus change.** Book + matching are off-chain; the chain sees
  HTLCs.
- **R6 — DoS.** A public book with unbounded orders needs the existing
  `MAX_ORDERS` + eviction; keep it and rate-limit gossip.
- **R7 — price manipulation.** A thin book is easy to manipulate; the UI should
  show depth and warn on thin liquidity.

## 4. Test plan

- Book sorts by price then time; `best_bid`/`best_ask` correct; exact rate
  comparison (no float error).
- Partial fill: two takers fill half each; order stays open until complete;
  no double-fill.
- Depth aggregation sums levels correctly.
- Eviction never removes `Matched`/`InProgress` orders.
- Market order fills against the best level(s).
- No consensus/chain change.

## 5. Non-goals

- Not an on-chain matching engine.
- Not hidden orders / dark pool.
- Not part of the current soak window.

## 6. References

- `vtorrent-node/src/atomic_swap.rs:414,446,499,813` `SwapOrder`,
  `OrderAnnouncement`, `OrderStatus`, `SwapOrderBook`.
- `vtorrent-rpc/src/handlers/dex.rs` order/match handlers.
- `docs/atomic-swap-ux-design.md` (take flow).
