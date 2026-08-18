# Tauri Command Gap + Incentive Settlement — Design

Date: 2026-08-18
Status: Approved

## Goal

Fix the desktop app's missing `add_torrent`/`remove_torrent` Tauri commands and
wire the incentive settlement loop so earned/paid totals update over time. This
is the final sub-project of the BitTorrent integration roadmap.

## Scope

This sub-project delivers:

- `add_torrent` and `remove_torrent` Tauri commands (the desktop app currently
  cannot add/remove torrents).
- A periodic incentive settlement task that calls `settle()` on due accounts.

Out of scope: actual VTR payment transactions (the torrent protocol has no
VTR-address exchange mechanism yet).

## Decisions

| Topic | Decision |
|---|---|
| Incentive scope | Settlement accounting only (no payment tx) |

## Architecture

### 1. Tauri commands (`vtorrent-tauri`)

- `add_torrent` — mirrors the RPC `add_torrent` handler: parse magnet/.torrent,
  create a `TorrentSession`, add it, and spawn the engine task.
- `remove_torrent` — cancels the engine task and removes the session.
- Register both in `main.rs`.

### 2. Incentive settlement loop (`vtorrent-daemon`)

- A periodic task (every `PAYMENT_INTERVAL_SECS` = 300s) that iterates sessions
  and calls `settle()` on each `PeerBandwidthAccount` that `needs_settlement()`.
- Updates `total_earned_satoshis` / `total_paid_satoshis`, already exposed via
  `get_torrent_sessions` and the RPC `TorrentSessionResponse`.

## Error handling

`TauriError` variants for parse/add/remove failures.

## Testing

- Unit tests for the Tauri command happy path (add then list).
- Unit tests for the settlement loop (an account with enough bytes settles).
