# Staking Ops Dashboard — Design (2026-09-10)

## Goal
Extend the wallet staking UX into a full ops view covering health + rewards,
usable during the 7-day soak without touching the soak fleet.

## Non-goals
- No change to consensus, staking engine, key handling, or running fleet image.
- No duplicate of Grafana; link out instead.
- No backend change in v1.

## Architecture
Frontend-only composition in `vtorrent-ui`:
- Extend `StakingPage.tsx`, reuse `useStakingStatus` (WS `staking_reward` + 8s
  HTTP fallback) and `useNode`/`useWallet` hooks.
- Read-only RPCs: `/api/v1/info`, `/api/v1/staking/status`, `/api/v1/peers`,
  `/api/v1/mempool`, `/api/v1/blockchain/block/height/:height`.
- Tauri via `tauriInvoke`, web via `rpcGet`. No key material in JS (AGENTS.md).
- Dev in isolated `git worktree`/branch against local single-node regtest.
  Soak fleet (`vtorrent/node:c9d00a6`) untouched; no deploy until post 09-16
  sign-off. CI (`pnpm lint`, `cargo test`) stays green.

## Components
- `HealthStrip`: tip height/hash, sync%, peers, mempool, uptime.
- `RewardHistory`: last N stakes (height/time/reward), daily avg vs expected.
- `EligibilityTable`: per-UTXO value/age/maturity countdown; fallback to
  counts from `staking/status` + `wallet/utxos` when detail unavailable.
- `GrafanaLink`: deep-link to existing dashboard.
- Reuse `StatCard`/`DetailRow` styles.

## Data flow
- WS event → optimistic `blocksStaked+1`, `lastStakeTime=now`, then HTTP
  reconcile (existing `applyRewardEvent`).
- Health polls on existing cadence; reward list fetches last ~20 heights
  lazily on expand to avoid RPC spam.
- Failures degrade to cached values with inline banner; never block
  start/stop actions.

## Error handling / UX
- Distinguish `WalletLocked` vs no-address vs RPC-down in start flow.
- Full address copyable; hashes truncated with tooltip.
- Maturity note corrected to match backend (100 confirmations per current UI;
  verify against consensus 6h / fast-stake flags before merge).

## Testing
- Hook unit: WS patch + polling fallback.
- Component render with mocked `rpcGet`/`tauriInvoke`.
- `pnpm lint`; workspace `cargo test` unaffected (no backend change).
- Manual: local regtest single node; read-only queries against soak nodes only.

## v2 (deferred)
Backend reward-history endpoint + chart only if lazy block fetches prove too
chatty. Requires RPC + tests + Tauri command.
