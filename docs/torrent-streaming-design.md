# Torrent Streaming (Media Playback) — Design

Status: DRAFT (no consensus change; torrent engine + UI)
Scope: `vtorrent-torrent` (piece scheduling, file layout), `vtorrent-rpc`, UI.
Motivation: the torrent engine downloads pieces in scheduler order
(`scheduler.rs:104` `next_piece`) with no notion of "the user wants to watch this
file now." Streaming — play a video while it downloads — needs **sequential,
file-aware piece priority** and a local HTTP range server. This turns the torrent
feature into a media player, a major UX differentiator.

## 1. What exists

- `FileLayout` (`engine.rs:96`) + `piece_segment_ranges` (`:151`) map a piece to
  file byte ranges — the hard part is already there.
- `scheduler.rs:104` `next_piece` picks the next piece (no priority/sequential
  mode).
- `engine_disk.rs` reads/writes pieces (`read_piece_from_disk` `:117`).
- `Metainfo.files` (`metainfo.rs:25`) lists files.

## 2. Design

### 2.1 Sequential / streaming piece priority

- Add a **priority mode**: when streaming file F, request pieces in **file
  order** (sequential), with a small read-ahead window, instead of rarest-first.
- **Boundary pieces** (a piece spanning F and the next file) are needed too;
  `piece_segment_ranges` already computes the segments.
- Keep rarest-first for non-streamed files; streaming only reorders the
  requested set, not correctness.

### 2.2 Local range server

- Expose a **local HTTP server** that serves `GET /stream/<info_hash>/<file_index>`
  with **Range** support. It blocks (or returns 503 + `Retry-After`) until the
  requested byte range's pieces are downloaded, using the priority scheduler to
  fetch them.
- The player (browser `<video>` or mpv) points at this URL; seeking triggers a
  priority jump.

### 2.3 Playback

- **In-app**: a `<video>`/`<audio>` element against the range server.
- **External**: hand the URL to mpv/VLC.
- **Format caveat**: streaming works for **progressive** formats (MP4 with the
  moov atom at the front, WebM, MKV). MP4 with moov-at-end needs the tail first —
  detect and prioritize the tail (or remux). State this honestly.

### 2.4 UI

- A **Play** button on a torrent's file list; a player view; a "buffering"
  indicator tied to the priority scheduler.

## 3. Adversarial review

- **R1 — sequential ≠ correct.** Priority only reorders requests; piece
  verification (`PieceAssembler::verify`) and the completion logic must be
  unchanged. Test that a streamed download still verifies every piece.
- **R2 — the moov-at-end problem.** Many MP4s put the index at the end; naive
  sequential streaming stalls. Detect and fetch the tail first, or document the
  limitation and recommend faststart-remuxed files.
- **R3 — range server security.** The local HTTP server must be **localhost-only**
  (like the RPC) and path-safe (`sanitize_path` exists for disk paths; apply the
  same rigor to the URL). Don't expose it to the network.
- **R4 — blocking vs DoS.** A range request for a not-yet-downloaded region must
  not spin; bound the wait and return a clear status.
- **R5 — piece boundary.** A piece spanning two files must be fetched for either
  file's stream; don't miss boundary pieces.
- **R6 — no consensus change.** Torrent engine + local server + UI only.

## 4. Test plan

- Sequential mode requests pieces in file order; rarest-first still used
  elsewhere.
- Range server serves a downloaded range; blocks/503s for a missing range; a
  seek triggers a priority jump.
- Every streamed piece still verifies; completion is correct.
- moov-at-end detection fetches the tail.
- Range server is localhost-only and path-safe.
- Boundary pieces are fetched for both adjacent files.

## 5. Non-goals

- Not transcoding/remuxing (recommend faststart files or a follow-on).
- Not a full media library.
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/engine.rs:96,151` `FileLayout`, `piece_segment_ranges`.
- `vtorrent-torrent/src/scheduler.rs:104` `next_piece`.
- `vtorrent-torrent/src/engine_disk.rs:117` `read_piece_from_disk`, `:140`
  `sanitize_path`.
- `vtorrent-torrent/src/metainfo.rs:25` `Metainfo.files`.
