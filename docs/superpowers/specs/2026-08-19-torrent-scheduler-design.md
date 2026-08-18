# Torrent Piece Scheduler — Design

Date: 2026-08-19
Status: Approved

## Goal

Replace the single-piece smoke test in the torrent download engine with a real
multi-peer, multi-piece scheduler that downloads complete files: rarest-first
piece selection, 16 KiB block pipelining, choke/interest handling, endgame
mode, and resume support.

## Scope

This sub-project delivers:

- A `PieceTracker` tracking per-piece and per-peer availability.
- Rarest-first piece selection with 16 KiB block requests and pipelining.
- Choke/unchoke/interest handling.
- Endgame mode for the final pieces.
- Resume support via a persisted piece bitfield.
- Progress and speed updates.

Out of scope: UDP tracker, DHT, and incentive payment transactions.

## Decisions

| Topic | Decision |
|---|---|
| Scheduler scope | Full scheduler |
| Concurrency | Per-peer tokio tasks + shared `Arc<Mutex<SchedulerState>>` |
| Resume | Persisted piece bitfield sidecar |

## Architecture

### 1. Piece state tracking (`scheduler.rs`, new module)

A `PieceTracker` holding, per piece: `have` (we have it), `requested` (in
flight), and per-peer `have` bitfields. Plus a `BlockState` for in-flight 16 KiB
blocks.

### 2. Rarest-first selection

- On peer connect, exchange bitfields (`Bitfield`/`Have` messages).
- Pick the next piece to download as the one with the fewest peers holding it
  (rarest-first), among pieces we don't have and aren't already requested.
- Request pieces in 16 KiB blocks; a peer can have up to N blocks in flight
  (pipelining).

### 3. Choke/interest + endgame

- Send `Interested` when the peer has a piece we want; honor `Choke`/`Unchoke`.
- **Endgame mode**: when the last few pieces remain, request them from all
  peers to avoid a slow peer stalling completion.

### 4. Resume + progress

- Persist a piece bitfield in a `.vtorrent` sidecar next to the download dir;
  on start, load it and skip completed pieces.
- Update `bytes_downloaded`, `download_speed`, `upload_speed` in the session as
  blocks arrive.

### 5. Concurrency

- One tokio task per peer, coordinated via `Arc<Mutex<SchedulerState>>`; the
  existing `run_engine` spawns peer tasks and awaits them.

## Error handling

`TorrentError` variants for scheduler failures; a peer task that errors is
dropped without failing the whole session.

## Testing

- Unit tests for `PieceTracker` (bitfield merge, rarest-first selection).
- Unit tests for block pipelining and endgame selection.
- Unit tests for the resume bitfield save/load round-trip.
