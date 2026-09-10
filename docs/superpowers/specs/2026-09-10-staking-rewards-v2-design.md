# Staking Reward History (v2) — Design (2026-09-10)

## Goal
Give the staking dashboard real per-block reward history: backfill via a new
read-only RPC plus live WS rows, replacing the v1 placeholder (hardcoded
`rewardSats: 0`, "v2 endpoint pending" label, Tauri dead-click).

## Non-goals
- No consensus, staking-engine, or key-handling changes.
- No fleet redeploy; soak nodes stay on `c9d00a6` until post-09-16 sign-off.
- No unbounded scans; no new persisted state.

## Backend
`GET /api/v1/staking/rewards?limit=20&address=` (public route, like
`staking/status`):
- `limit` clamped 1–100, default 20. Optional `address` filters server-side.
- Single chain lock; walk back from tip; per height load full block via
  `chain.get_block_at_height`.
- Per block: find `TxType::Coinstake`; `reward_sats` = sum of coinstake output
  values — identical definition to the `StakingReward` WS event
  (`vtorrent-node/src/node/staking_loop.rs`), so backfill and live rows agree.
- Staker address decoded best-effort from reward outputs; `null` when
  undecodable. Blocks without a coinstake are skipped (not PoS).
- Reorg-safe by construction (reads active chain at request time).

Files: `StakingRewardsResponse` (+ item type) in `vtorrent-rpc/src/models.rs`,
handler in `vtorrent-rpc/src/handlers/staking.rs`, route in
`vtorrent-rpc/src/server.rs`, Tauri `get_staking_rewards` command in
`vtorrent-tauri/src/commands/staking.rs` (+ registration).

## Frontend
`RewardHistory` keeps its lazy opt-in button but calls one
`staking/rewards?limit=20` (web) / `get_staking_rewards` (Tauri):
- Renders rows: height, time, reward, staker address.
- Daily avg computed from real `rewardSats`; placeholder label reverts to
  a computed "Daily avg" line.
- Live WS `staking_reward` rows prepended (existing `useStakingStatus` flow).
- Error banner pattern reused from the v1 fix (covers Tauri/RPC failures).
- Resolves the tracked v1 follow-ups for this component; poller-dedupe and
  `DetailRow`/`test-alias` cleanups stay separate.

## Testing
- Rust unit: coinstake-sum helper (marker output handling), limit clamp
  (0/101/default), address filter match/mismatch.
- RPC integration: route returns recent rewards shape; empty when above tip.
- UI: vitest real-average case; `pnpm lint`; `cargo clippy/test`.
