# DHT (BEP-5) — Design

Date: 2026-08-19
Status: Approved

## Goal

Add trackerless peer discovery via the BitTorrent Kademlia DHT (BEP-5), so the
torrent client can find peers without any tracker. This is the second of two
sub-projects (the first was UDP tracker/BEP-15).

## Scope

This sub-project delivers:

- An async DHT message codec (get_peers, find_node, announce_peer).
- A `DhtClient` with bootstrap, iterative `get_peers`, and `announce_peer`.
- Integration into `run_engine` as a fallback when no trackers are available.

## Decisions

| Topic | Decision |
|---|---|
| Approach | New async DHT client in `vtorrent-torrent` |
| Depth | Full BEP-5 (iterative lookup + announce) |
| Integration | Fallback when no trackers (or all trackers fail) |

## Architecture

### 1. DHT message codec (`dht.rs`, new module)

Async bencode message build/parse:

- `get_peers` query/response (with `values` for peers or `nodes` for closer
  nodes, plus `token`).
- `find_node` query/response.
- `announce_peer` query (with `token`, `port`, `implied_port`).
- Reuse the compact node/peer parsing approach from `vtorrent-p2p` (26-byte
  nodes, 6-byte peers).

### 2. `DhtClient`

- `bootstrap()` — ping/find_node the public routers
  (`router.bittorrent.com:6881`, `dht.transmissionbt.com:6881`,
  `router.utorrent.com:6881`).
- `get_peers(info_hash)` — iterative lookup: query closest nodes, recurse via
  `find_node` toward the target, collect peers.
- `announce_peer(info_hash, port)` — announce ourselves to the closest nodes
  using their returned tokens.
- Async (`tokio::net::UdpSocket`), transaction-ID matching, timeout.

### 3. Integration

- `run_engine` falls back to DHT when no trackers (or all trackers fail):
  bootstrap, iterative `get_peers`, then download from discovered peers.

## Error handling

`TorrentError` variants for DHT I/O, timeout, and malformed responses.

## Testing

- Unit tests for get_peers/find_node/announce_peer message build/parse.
- Unit tests for compact node/peer parsing.
- An integration test with a mock DHT node (get_peers returns peers).
