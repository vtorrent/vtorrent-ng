# Magnet Metadata Fetch (BEP-9/10) — Design

Date: 2026-08-18
Status: Approved

## Goal

Make magnet links actually downloadable by implementing the BEP-10 extension
protocol and BEP-9 `ut_metadata`, so the client can fetch the torrent info dict
from peers and reconstruct a full `Metainfo` (piece hashes, files, piece length).

## Scope

This sub-project delivers:

- BEP-10 extension protocol support (the `Extended` message and the extension
  bit in the handshake `reserved` field).
- BEP-9 `ut_metadata` message exchange (extension handshake, request, data,
  reject).
- A `fetch_metadata` engine pre-step that reassembles the info dict and parses
  it into a full `Metainfo`.

Out of scope: other BEP-10 extensions (e.g. `ut_pex`, `ut_comment`), and the
multi-peer scheduler.

## Decisions

| Topic | Decision |
|---|---|
| Metadata fetch | Full BEP-9/10 |
| Max metadata size | 16 MiB, requested in 16 KiB pieces |
| Integration | Engine pre-step before the download loop |

## Architecture

### 1. Extension protocol (`peer_wire.rs`)

- Add `PeerMessage::Extended { id: u8, payload: Vec<u8> }` (message id 20) with
  encode/decode.
- Set bit 20 of the handshake `reserved` field to advertise extension support,
  and check the peer's `reserved` field before using extensions.

### 2. `ut_metadata` (BEP-9)

- Extension handshake: a bencoded dict
  `{ "m": { "ut_metadata": <id> }, "metadata_size": <n> }` exchanged as
  `Extended { id: 0, ... }`.
- Request: `{ "msg_type": 0, "piece": <i> }`.
- Data: `{ "msg_type": 1, "piece": <i>, "total_size": <n> }` followed by the
  piece bytes.
- Reject: `{ "msg_type": 2, "piece": <i> }`.

### 3. `fetch_metadata` engine pre-step

- If `metainfo.pieces.is_empty()` (a magnet link), connect to peers, negotiate
  the extension, request the info dict in 16 KiB pieces, reassemble, and parse
  it via `Metainfo::from_bytes` (the info dict is a valid bencoded dict).
- Update the session's `metainfo` in place so the download loop can proceed.

### 4. Integration

- `run_engine` calls `fetch_metadata` before the download loop when the
  metainfo has no pieces.

## Error handling

`TorrentError` variants for extension negotiation and metadata reassembly
failures. A failed fetch leaves the session in `Error` state with a message.

## Testing

- Unit tests for `Extended` message encode/decode.
- Unit tests for the `ut_metadata` bencoded message encode/decode.
- Unit tests for metadata reassembly (split a known info dict into pieces,
  reassemble, parse).
