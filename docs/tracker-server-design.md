# Self-Hosted Tracker Server — Design

Status: DRAFT (no consensus change; torrent + daemon)
Scope: `vtorrent-torrent` (new server side), `vtorrent-daemon` (optional service).
Motivation: the torrent crate implements tracker **clients** (HTTP + UDP
announce, `tracker.rs`/`udp.rs`) but there is **no tracker server**. A
self-hosted tracker gives fast, live peer discovery for a swarm — complementing
the on-chain registry (permanent catalog, `docs/torrent-discovery-design.md`) and
the DHT (decentralized, no server). Running one is a natural service for a seed
node.

## 1. What exists

- HTTP tracker client (`HttpTracker`, `tracker.rs:78`) with SSRF guards
  (`validate_tracker_url`, `resolve_tracker_url`).
- UDP tracker client (`UdpTracker`, `udp.rs:190`; connect/announce/scrape).
- No server: nothing accepts announces or returns peer lists.

## 2. Design

### 2.1 HTTP tracker (BEP-3)

- `GET /announce?info_hash=…&peer_id=…&port=…&uploaded=…&downloaded=…&left=…&event=…`
  → bencoded `{ interval, peers }` (compact `peers` = 6 bytes/peer).
- Track per-`info_hash` peer sets: `{ peer_id, ip, port, last_seen, left }`.
- **Events**: `started` / `stopped` / `completed` / (none) update the set.
- `GET /scrape` → per-info_hash `{ complete, downloaded, incomplete }`.

### 2.2 UDP tracker (BEP-15)

- `connect` → `connection_id` (time-limited, HMAC of the client IP + a secret);
  `announce` → peer list; `scrape` → counts.
- Stateless connection ids (HMAC) so no per-client state is needed for connect.

### 2.3 Peer set management

- **Expiry**: drop peers not seen within `2 × interval`.
- **Caps**: bound peers per info_hash and total info_hashes (memory DoS).
- **Compact responses**: 6 bytes/peer (IPv4) to keep responses small.
- **IPv6**: BEP-7 `peers6` (optional).

### 2.4 Security

- **No SSRF on the server side** (it doesn't fetch URLs), but:
  - **Rate-limit** announces per IP (the client already has a rate limiter
    pattern, `vtorrent-rpc/src/ratelimit.rs`).
  - **Don't trust the announced `ip`** unless the request is from a trusted
    proxy; default to the **source IP** (anti-spoofing), with an opt-in
    `X-Forwarded-For` behind a known proxy.
  - **Bound** memory (peers, info_hashes) and response size.
- **Privacy**: a tracker sees who is in a swarm; run it as an explicit service
  with a stated policy.

### 2.5 Deployment

- An **optional** daemon service (`--tracker-listen`), off by default. A seed node
  can run one for its swarms.
- **Announce URL** published with torrents (in the metainfo `announce` /
  `announce-list`), alongside the on-chain registry and DHT.

## 3. Adversarial review

- **R1 — don't trust announced IPs.** A client can claim any IP; default to the
  source IP or you enable spoofing/reflection. Only honour `X-Forwarded-For`
  behind a trusted proxy.
- **R2 — memory DoS.** Unbounded info_hashes/peers is a trivial DoS; cap both and
  expire aggressively.
- **R3 — rate-limit.** A tracker is an amplification target; rate-limit per IP and
  bound response size.
- **R4 — UDP connection-id must be stateless + authenticated.** An HMAC of
  (IP, time, secret) avoids per-client state and spoofed announces; a naive
  counter is forgeable.
- **R5 — privacy.** The tracker sees swarm membership; state the policy and make
  it opt-in (a seed-node service, not a default).
- **R6 — no consensus change.** Torrent service only.

## 4. Test plan

- HTTP announce: started/stopped/completed update the peer set; compact response
  decodes to the right peers.
- UDP connect/announce/scrape round-trip; a forged connection-id is rejected.
- Expiry drops stale peers; caps bound memory.
- Announced IP is ignored in favour of the source IP (spoof test).
- Rate limiting and response-size bounds.
- No consensus/chain change.

## 5. Non-goals

- Not a public tracker service (ops decision; opt-in).
- Not a replacement for the on-chain registry or DHT (complementary).
- Not part of the current soak window.

## 6. References

- `vtorrent-torrent/src/tracker.rs:78` `HttpTracker`, SSRF guards.
- `vtorrent-torrent/src/udp.rs:190` `UdpTracker` (connect/announce/scrape).
- `vtorrent-rpc/src/ratelimit.rs` (rate-limit pattern).
- `docs/torrent-discovery-design.md` (registry + DHT).
