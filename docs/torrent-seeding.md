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
  --torrent-announce-port 22630 \   # only if NAT maps a different port
  --torrent-max-connections 32
```

A completed torrent is registered automatically. `GET /api/v1/torrent/settings`
reports `seeding: true` and `seeded_torrents`. `DELETE /api/v1/torrent/:id`
removes a session and stops serving it immediately.

Unlock the wallet (`POST /api/v1/wallet/unlock`) before serving: the seeder
signs a cumulative `BandwidthReceipt` per connection with the hot wallet, and a
locked wallet can only upload bytes no VTorrent peer can verify or pay for.

## What the seeder enforces

| Limit | Value |
|---|---|
| Concurrent inbound connections | `--torrent-max-connections` (default 32) |
| Largest block served | 32 KiB |
| Outstanding requests per connection | 64 |
| Duplicate request window | 10 s |
| Idle connection timeout | 300 s |
| Receipt cadence | when the reported total advances, at most every 30 s |
| Tracker re-announce | every 30 minutes |

It also applies the shared upload limiter, the session seed-ratio target, and
the same address policy as the downloader (loopback, private, and link-local
targets are refused unless explicitly permitted for local testing).

## Fail-closed behaviour

- An info-hash that is not registered is refused before any disk read.
- Every piece is SHA-1 verified against the metainfo before its first block is
  sent; a piece that no longer matches is never served.
- A peer that does not advertise BEP-10 (no `ut_vtr_receipt`) is refused: it
  could only consume bandwidth unpaid.
- Receipts from the downloading peer are verified (`peer` names our address,
  `signer` is the peer, windows and settlement ids are bounded) and recorded in
  the same incentive account and journal as the download path, so the existing
  settlement and payment loop covers bytes we uploaded.

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
2. On the first node, place a completed torrent in
   `/tmp/vtr-release-fleet/downloads/<name>/` with the `.vtorrent-torrent`
   marker (`v1:<info-hash>`) and a fully set
   `downloads/<info-hash>.vtorrent` resume bitfield.
3. Run an HTTP tracker that returns the first node's container address and
   port.
4. `POST /api/v1/torrent/add` on the first node and poll
   `/api/v1/torrent/settings` until `seeded_torrents` is 1.
5. `POST /api/v1/torrent/add` with the second node's wallet address and poll
   its session until `downloaded_bytes` equals the torrent size. Compare the
   file's SHA-256/SHA-1 with the source.
6. Confirm the first node's `uploaded_bytes` and `vtr_earned_satoshis`, and the
   second node's `vtr_paid_satoshis` once settlement has run.

On 2026-11-22 this moved 12 MiB between two fleet nodes in **1 second** with a
matching SHA-1, full upload credit on the seeder, and a 585,937-sat payment
that was admitted, confirmed at height 4, and finalized at height 9 without a
stuck-payment warning.

## Suite results

- Workspace: **1,512 passed, 0 failed, 13 ignored** across 56 groups.
- Torrent lib: 135 passed, including the four seeder tests.
- RPC: 215 passed. Desktop: 34 passed.
- Both ignored E2Es pass on one release binary: the inbound seeder serves
  6 MiB over 24 verified pieces and signs a receipt covering all of them, and
  the download path still pays a seeder exactly 585,937 satoshis.
