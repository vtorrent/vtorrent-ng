# AI supplementary security review — pass 6

**Date:** 2026-11-21
**Scope:** tree after `10c4671` plus the torrent E2E fixes
**Status:** AI-only. This is not the independent human review and does not
satisfy the mainnet security gate.

The six-reviewer dispatch was cut short by the model provider's weekly usage
limit after the incentive-persistence area completed. That area's findings were
verified and fixed; the remaining areas were reviewed directly, driven by the
two-process torrent E2E and targeted source analysis rather than a second model
pass. Re-run the full dispatch when quota is available.

## Fixed

| # | Severity | Area | Defect | Fix |
|---|---|---|---|---|
| 75 | Critical | Torrent peer wire | `PeerConnection::recv` discarded bytes read past the first decoded frame, so coalesced frames desynchronized the stream and no piece was ever credited after the first burst. | Persistent per-connection buffer with a cursor. Regression test `test_peer_connection_recv_preserves_coalesced_messages`. |
| 76 | High | Torrent storage | The daemon configured `<data-dir>/downloads` but never created it, so every verified piece failed its disk write. | `POST /api/v1/torrent/add` creates the directory before the session exists and fails closed. |
| 77 | High | Incentive receipts | A peer task stopped reading at completion, so the peer's final signed receipt was never recorded and the last window was unpayable. | A 10-second post-completion grace keeps reading the final receipt. |
| 78 | High | Torrent scheduler | Block leases and rarest-first counters were keyed by the remote's self-reported 20-byte id, so two peers could collide and one peer could satisfy another's lease. | Leases carry an engine-assigned unique task id; a peer may only satisfy its own lease with the exact requested length. |
| 79 | High | Torrent accounting | `in_flight` was decremented before the block was validated, and any length was accepted. | Ownership and length are validated before the in-flight count or credit changes. |
| 80 | Medium | Torrent memory | 200 peer tasks × unbounded partial pieces × 16 MiB frames was unbounded in aggregate. | Per-connection frame caps, a per-peer assembler byte budget, and a shared 64 MiB assembly budget with reservation/release; a drop guard returns a task's reservations on cancellation or error. |
| 81 | Medium | Torrent prelude | A metainfo could list ~1,024 trackers, each attempted with a 30-second timeout; the DHT fallback could run for many minutes. Tracker connect/handshake/send had no timeout. | Tracker list and magnet trackers bounded at 32, announce and DHT bounded by wall clock, all peer-wire I/O bounded. |
| 82 | Medium | Torrent sessions | Finished sessions were never removed; the cap counted only active ones, so every metainfo stayed resident forever. | `MAX_RETAINED_SESSIONS` bounds the retained set; the oldest finished session is evicted. |
| 83 | Medium | Incentive migration | Legacy `incentive-live.json` debts were merged with fresh ids, no dedup, and the file was deleted before the save succeeded; a stored intent could confirm a different debt. | Startup fails closed while the legacy file exists and documents the manual verification. |
| 84 | Medium | Journal compaction | Once live records exceeded 4,096, every mutation rewrote the whole journal (O(n) + two fsyncs), starving settlement. | Compaction triggers on dead lines or real file growth, never on the live count. |
| 85 | Medium | Incentive payments | Payments selected chain UTXOs without excluding mempool spends, so a second queued payment conflicted with the first until it confirmed. | Mempool-spent outpoints are filtered in the incentive, ordinary send, cold-stake, and desktop send paths. The desktop send test now funds two independent coins because its second send correctly skips the first send's mempool-spent input. |
| 86 | Low | Budget durability | A failed budget append was ignored and never retried. | `journal_set_budget` reports success; a failure rolls the batch back. |
| 87 | Low | Journal contract | A rejected stale write returned `Ok`, so callers marked it durable. | `upsert` returns `Ok(false)` for a rejected stale write; callers distinguish it. |
| 88 | Low | Receipt epoch | An epoch change settled at a peer-chosen future `window_end`. | Settles at `min(now, window_end)`. |
| 89 | Low | Mint endpoint | The faucet was a public route gated only by the regtest flag. | It now sits on the API-key-protected router. |

## Not fixed

| Risk | Why it remains |
|---|---|
| No inbound torrent seeding listener. | The engine downloads and pays peers; serving pieces to peers is still unimplemented. The E2E uses a protocol-faithful seeder process, not a second daemon. |
| DHT/bootstrap peer discovery is process-wide, not per-torrent policy. | Accepted: it is the same discovery path the node already uses; tracker targets remain SSRF-filtered. |
| The faucet still mints in regtest only. | Regtest requires loopback RPC today; the API key is defense in depth. |

## Verification

- Two-process torrent E2E on the final release binary
  (`sha256:628596ea…`): 12 MiB verified, receipt settled, one 585,937-sat
  payment, graceful restart recovered the same pending txid without repayment.
- Torrent unit suite: 131 passed, including the new coalesced-frame, lease
  ownership, retained-session, and compaction regressions.
- RPC suite: 214 passed, including the faucet API-key gate.
- Clippy `-D warnings` clean on torrent, RPC, Tauri, daemon, wallet-service.
- Full workspace: **1,508 passed, 0 failed, 12 ignored** across 55 groups.
- Desktop UI: 45 tests, `pnpm lint` clean.
