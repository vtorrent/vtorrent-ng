# UDP Tracker (BEP-15) — Design

Date: 2026-08-19
Status: Approved

## Goal

Add UDP tracker support (BEP-15) so the torrent client can announce to UDP
trackers in addition to HTTP trackers. This is the first of two sub-projects
(the second is DHT/BEP-5).

## Scope

This sub-project delivers:

- UDP message codec: connect, announce, scrape, and error.
- A `UdpTracker` client with transaction-ID matching and timeout/retry.
- Integration into `run_engine` (try UDP trackers alongside HTTP trackers).

## Decisions

| Topic | Decision |
|---|---|
| Scope | Full BEP-15 (connect, announce, scrape, error) |

## Architecture

### 1. Message codec (`tracker.rs`)

Add UDP message types and encode/decode:

- **Connect** (action 0): request with a random 64-bit transaction ID; response
  carries a 64-bit connection ID.
- **Announce** (action 1): request with connection ID, info hash, peer ID,
  downloaded/left/uploaded, event, IP, key, num_want, port; response carries
  interval, leechers, seeders, and compact peers.
- **Scrape** (action 2): request with connection ID + info hashes; response
  carries seeders/completed/leechers per hash.
- **Error** (action 3): response with a message string.

### 2. `UdpTracker` client

- `connect()` — send connect, await the connection ID (with transaction-ID
  matching and timeout).
- `announce()` — send announce, await the response, parse compact peers into
  `TrackerPeer`.
- `scrape()` — send scrape, await the response.
- Uses a `tokio::net::UdpSocket`; matches responses by transaction ID; retries
  on timeout.

### 3. Integration

- `run_engine` tries UDP trackers (from `announce`/`announce-list` URLs with
  `udp://` scheme) in addition to HTTP trackers, falling back across both.

## Error handling

`TorrentError` variants for UDP I/O, timeout, and malformed responses.

## Testing

- Unit tests for connect/announce/scrape/error encode/decode (round-trip).
- Unit tests for compact-peer parsing from a UDP announce response.
