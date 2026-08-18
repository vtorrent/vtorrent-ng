# Torrent Download Engine — Design

Date: 2026-08-18
Status: Approved

## Goal

Make the vTorrent BitTorrent client genuinely functional by building the
download/upload engine: parse piece hashes, announce to trackers, connect to
peers, transfer and verify pieces, write to disk, and seed. This is the first
sub-project of the BitTorrent integration roadmap.

## Scope

This sub-project delivers:

- Piece-hash parsing in `Metainfo` (the `pieces` string is currently ignored).
- A per-session tokio engine that announces, connects, downloads, verifies,
  writes to disk, and seeds.
- A configurable download directory.
- Wiring `add_torrent`/`remove_torrent` to spawn/cancel the engine.

Out of scope (later sub-projects): magnet metadata fetch (BEP-9/10), UDP
tracker (BEP-15), and incentive payment wiring to the wallet.

## Decisions

| Topic | Decision |
|---|---|
| Engine scope | Full engine (download + seed) |
| Storage | Configurable directory (default `<data_dir>/downloads`) |
| Wiring | Per-session tokio task, cancelled via `CancellationToken` |

## Architecture

### 1. Piece-hash parsing (`metainfo.rs`)

Add a `pieces: Vec<[u8; 20]>` field to `Metainfo` and extract the `pieces`
string from the info dict. Each 20-byte chunk is one piece's SHA1. Validate
that `pieces.len() == piece_count * 20`.

### 2. Download engine (`engine.rs`, new module)

A per-session tokio task with this lifecycle:

- **Announce**: call `HttpTracker::announce` (Started) to get peers; populate
  `session.peers`.
- **Connect**: open `TcpStream` to each peer, perform the handshake using the
  existing `PeerMessage` codec.
- **State machine**: use `PeerChokeState` (choke/unchoke/interest) to
  coordinate.
- **Download**: request blocks (16 KiB), assemble pieces, verify SHA1 against
  `metainfo.pieces`, write verified pieces to disk.
- **Seed**: after completion, serve pieces to interested peers, updating
  `record_upload`.
- **Progress**: update `session.state`, `bytes_downloaded`, `bytes_uploaded`,
  `download_speed`, `upload_speed` in place.
- **Cancellation**: `remove_torrent` cancels via a
  `tokio_util::sync::CancellationToken`.

### 3. Storage

A configurable download directory (default `<data_dir>/downloads`), files
written as `<name>/<path>`. The daemon passes the directory into the engine.

### 4. Wiring

- `add_torrent` spawns the engine task (instead of just inserting a `Queued`
  session).
- `remove_torrent` cancels the task.
- `list_torrent_sessions` reflects real progress/speed/peers (already reads the
  session fields).

## Error handling

`TorrentError` variants for engine failures (connect, handshake, piece
verification, disk I/O). A failed session transitions to `SessionState::Error`
with a message.

## Testing

- Unit tests for piece-hash parsing (valid and malformed `pieces` strings).
- Unit tests for the engine's piece-assembly and SHA1 verification (using a
  small in-memory torrent).
- Unit tests for the choke/interest state machine.
- Integration tests for `add_torrent`/`remove_torrent` spawning/cancelling.
