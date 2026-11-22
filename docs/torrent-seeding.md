# Inbound torrent seeding

vTorrent-NG both **downloads and pays** (`vtorrent-torrent::engine`) and
**serves** (`vtorrent-torrent::seed`). This note covers the serving side.

## Enabling it

Seeding is opt-in and off by default, because publishing a listening port and
making the node reachable by arbitrary clients is an operator decision.

```bash
vtorrent-daemon \
  --torrent-listen \
  --torrent-listen-addr 0.0.0.0:22630 \
  --torrent-max-connections 32

# Add only when NAT maps a different public port:
#   --torrent-announce-port 22630
```

A completed torrent is registered automatically. `GET /api/v1/torrent/settings`
reports `seeding: true` and `seeded_torrents`. `DELETE /api/v1/torrent/:id`
removes a session, cancels its open connections, and stops serving it.

On a staking node, keep a second spendable coin. The payment builder refuses to
spend the last staking-eligible UTXO: doing so would stop the node from staking
until the payment confirmed. Fund a small separate amount (or split the stake)
before enabling automatic incentive payments.

Unlock the wallet (`POST /api/v1/wallet/unlock`) before serving: the seeder
signs a cumulative `BandwidthReceipt` per connection with the hot wallet, and a
locked wallet can only upload bytes no VTorrent peer can verify or pay for.

## What the seeder enforces

| Limit | Value |
|---|---|
| Concurrent inbound connections | `--torrent-max-connections` (default 32) |
| Largest block served | 32 KiB |
| Largest inbound frame | 256 KiB |
| Idle connection timeout | 300 s |
| Peer receipt verification | at most every 30 s per connection |
| Our receipt cadence | when the reported total advances, at most every 30 s |
| Tracker re-announce | every 30 minutes, checked every 60 s |

It also applies the shared upload limiter, the session seed-ratio target, and
the same address policy as the downloader (loopback, private, and link-local
targets are refused unless explicitly permitted for local testing).

Requests are handled serially per connection and answered from the requested
byte range. A piece is read and SHA-1 verified in full on its first request;
later blocks read only their own range, so a 16 KiB request cannot force a
whole multi-megabyte piece read.

## Fail-closed behaviour

- An info-hash that is not registered is refused before any disk read.
- Every piece is SHA-1 verified against the metainfo before its first block is
  sent; a piece that no longer matches is never served. Verification results
  are cached for the process lifetime, so a file modified after verification is
  not re-checked (protect the download directory).
- A peer that does not advertise BEP-10 is refused. BEP-10 support alone does
  not pay: a client that never sends a `ut_vtr_receipt` address can still
  download, so the payment is voluntary after the fact. Set an upload limit and
  treat `vtr_earned_satoshis` as a measure, not a guarantee.
- Receipts from the downloading peer are verified (`peer` names our address,
  `signer` is the peer, `info_hash` must match this torrent, windows and
  settlement ids are bounded) and recorded in the same incentive account and
  journal as the download path. Our earnings are recorded for settlement
  visibility; the downloading peer's payment loop is what actually pays them.

## Restarting

Torrent sessions are in-memory. Finished data and its resume bitfield remain on
disk, but the seeder starts empty after a daemon restart; re-add each torrent
with `POST /api/v1/torrent/add` to serve it again. A production seed node should
include that step in its restart procedure.

## Verification

`vtorrent-daemon/tests/torrent_seeder_e2e.rs` is an ignored, manual test that
runs the exact candidate binary with `--torrent-listen` in a network namespace,
places a completed torrent in its download directory, and dials it from an
independent client. It asserts that all pieces verify, that the daemon credits
exactly the uploaded byte count, that it signs a receipt covering those bytes,
and that an unknown info-hash is refused.

`vtorrent-daemon/tests/torrent_incentive_e2e.rs` covers the other direction:
the daemon downloads from a mock seeder that announces its bitfield
immediately after the wire handshake, then settles the signed receipt and pays
exactly once across a restart.

### Two fleet nodes, real transfer

1. Start the fleet with `SEED_TORRENT=1`; every node listens on container port
   22630.
2. On the first node, place a completed torrent in its container data dir:
   create `/tmp/vtr-release-fleet/downloads/<name>/`, copy `<name>` and the
   `.vtorrent-torrent` marker (`v1:<info-hash>`) into it, and copy a fully set
   `downloads/<info-hash>.vtorrent` resume bitfield. Use `docker cp` or
   `docker exec`; the paths are container-local.
3. Run an HTTP tracker that returns the first node's container address and
   port.
4. `POST /api/v1/torrent/add` on the first node (with
   `-H "X-Api-Key: $(cat <key>)"` when the RPC key is enabled) and poll
   `/api/v1/torrent/settings` until `seeded_torrents` is 1.
5. `POST /api/v1/torrent/add` with the second node's wallet address and poll
   its session until `downloaded_bytes` equals the torrent size. Compare the
   file's SHA-256/SHA-1 with the source.
6. Confirm the first node's `uploaded_bytes` and `vtr_earned_satoshis`, and the
   second node's `vtr_paid_satoshis` once settlement has run. A staking
   downloader needs a second spendable coin to pay.

On 2026-11-22 a single seeder moved 12 MiB in **1 second** with a matching
SHA-1, full upload credit, and a 585,937-sat payment confirmed at height 4 and
final at height 9.

On 2026-11-23 a **two-seeder** run moved the same torrent in 1 second: node1
served 4,980,736 bytes and node2 7,602,176 bytes, proving each piece was
assembled by one owning task. node3 paid node2 354,003 sats from a 0.5 VTR
non-staking coin while continuing to stake through heights 5–7.

## Suite results

- Workspace: **1,518 passed, 0 failed, 13 ignored** across 56 groups.
- Torrent lib: 140 passed, including the seeder and scheduler regressions.
- RPC: 215 passed. Desktop: 34 passed.
- Both ignored E2Es pass on one release binary: the inbound seeder serves
  6 MiB over 24 verified pieces and signs a receipt covering all of them, and
  the download path still pays a seeder exactly 585,937 satoshis.
