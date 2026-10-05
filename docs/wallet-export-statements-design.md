# Wallet Export & Statements — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: there is **no way to export** wallet history — no CSV/JSON statement,
no accounting view. Users (and their accountants) need a record of every
transaction: date, direction, counterparty, amount, fee, and the **earnings**
(staking rewards, torrent incentives, swap P&L). This is the natural follow-on to
`docs/earnings-view-design.md` and a real requirement for anyone treating VTR as
an asset.

## 1. What exists

- `get_transactions` (`handlers/wallet.rs:594`) returns a JSON list (currently
  change-address-only — see `docs/wallet-organization-design.md` R1).
- `get_staking_rewards` (`handlers/staking.rs:199`), torrent `incentive_summary`,
  swap status.
- No export, no CSV, no statement.

## 2. Design

### 2.1 Export formats

- **CSV** (spreadsheet/accounting): one row per tx —
  `date, txid, direction, counterparty, amount_vtr, fee_vtr, confirmations,
  block_height, category, note`.
  - `category`: `send | receive | stake_reward | torrent_earned | torrent_paid |
    swap | legacy_claim`.
  - `counterparty`: contact/name if known (`docs/wallet-organization-design.md`),
    else the address.
- **JSON** (machine-readable): the same data, nested (for tools).
- **Date range**: `?from=&to=` (by block height or timestamp).

### 2.2 Statements (period summary)

- A **statement** for a period (month/quarter/year): opening balance, closing
  balance, total in, total out, total fees, and **earnings by category**
  (staking, torrent, swap). This is what an accountant wants.
- Reuse the earnings aggregator (`docs/earnings-view-design.md`) for the
  earnings breakdown.

### 2.3 Cost basis / gains (optional, careful)

- A **cost-basis / realised-gains** report is jurisdiction-specific and legally
  sensitive. If offered, it must be **clearly labelled as informational**, use a
  documented method (FIFO), and disclaim tax advice. Recommend deferring or
  keeping it minimal; the raw CSV is the safe deliverable.

### 2.4 Privacy

- The export contains the full financial history — treat it as sensitive: write
  with 0600, warn about cloud/plaintext destinations, and never log it.
- Notes (`docs/wallet-organization-design.md`) are local-only; include them in
  the export only if the user opts in.

### 2.5 UI

- An **Export** action on the history/earnings pages (CSV/JSON, date range), and a
  **Statements** view (period summary).

## 3. Adversarial review

- **R1 — the export must be complete.** Exporting from the change-address-only
  history (`handlers/wallet.rs:594`) would silently omit txs. Fix the history
  first (`docs/wallet-organization-design.md` R1); test that the export includes
  every address's txs.
- **R2 — units.** VTR vs satoshis must be unambiguous in the CSV (a `amount_vtr`
  column with 8 dp, or a satoshi column labelled). Don't mix (the classic bug).
- **R3 — tax claims.** Any gains/tax report must be labelled informational and
  disclaimed; a wrong tax figure is a liability. Prefer raw data + a documented
  method.
- **R4 — privacy.** The export is a full financial record; 0600, warn, never log.
- **R5 — determinism.** The same range exports identically (sorted by height,
  then txid) so it's reproducible/auditable.
- **R6 — no consensus change.** Wallet/RPC/UI only.

## 4. Test plan

- Export includes txs to/from **all** wallet addresses (not just change).
- CSV columns correct; VTR/satoshi units unambiguous; totals balance.
- Statement opening + in − out − fees = closing.
- Deterministic ordering; same range → identical bytes.
- Export file is 0600; notes included only if opted in.
- No consensus/chain change.

## 5. Non-goals

- Not a tax-filing product (informational only).
- Not a hosted accounting service.
- Not part of the current soak window.

## 6. References

- `vtorrent-rpc/src/handlers/wallet.rs:594` `get_transactions`.
- `vtorrent-rpc/src/handlers/staking.rs:199` rewards; torrent `incentive_summary`.
- `docs/earnings-view-design.md`, `docs/wallet-organization-design.md`.
