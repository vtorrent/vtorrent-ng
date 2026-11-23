# AI supplementary security review — pass 9

**Date:** 2026-11-23
**Reviewed head:** `ef1d1a1fb43008f031687cb0a6ccda6e64395d17` (pass 8)
**Shipping code candidate:** `22056d4fc6a22731636ad05c457c42c1632c54f4`
**Image:** `vtorrent/node:22056d4`, ID
`sha256:390da969fab0e254f38cbc6bbb4bd85d76a5c5d4e376d6e174f994d66e147b9b`;
extracted binary
`b04131819f8c8062b47c803f3ed6df62facf277d041a8d0c15b425b3ae3ee177`
**Window:** `2026-11-23T06:26:05Z` to `2026-11-30T06:26:05Z` (in progress)
**Scope:** the pass-8 fixes: scheduler completion/stall/ownership, the seeder's
BEP-10/receipt/throttle/bitfield changes, payment/eviction/lifecycle, and the
tests/docs/evidence package.

**Method.** Four independent reviewer agents covered the scheduler/engine,
seeder, payments/lifecycle, and tests/docs/evidence. The author triaged every
finding and fixed the release-relevant ones. This is an AI-assisted supplement;
it does not satisfy the independent human security-review gate. The
`review-22056d4` exact-image window is running but is not complete.

## Fixed

### High

1. **The web/RPC add-torrent path was rejected.** The RPC began requiring a
   valid `wallet_address` on every add, but the only web UI caller sends `''`.
   The RPC now falls back to the unlocked hot wallet's change address when the
   field is empty; the Tauri command does the same and validates it. The
   address is trimmed once and stored in the form used for receipts.
2. **A silent peer delayed finalization by 178 s.** The pass-8 check at the top
   of the loop only helps a task that is still looping; a task blocked in
   `conn.recv` was not woken when another task completed the torrent. The live
   run showed node3 finalizing **178 s** after its last block. Each receive is
   now sliced to 10 s, so completion and cancellation are re-checked promptly,
   and a 15-minute no-verified-progress watchdog stops a keepalive-only or
   incomplete download. The pass-9 first torrent finalized ~33 s after the add,
   which is the 35 s final-receipt grace; the 178 s case is gone.
3. **The seeder receipt throttle was bypassable by rotating the peer address on
   one connection.** The throttle is now both per connection (25 s) and per
   address, only the first advertised address is accepted per connection, and
   the bounded map evicts its oldest entry instead of clearing (the clear was a
   global bypass).
4. **`Cancel` erased a served-block tombstone.** The seeder handles requests
   serially, so a `Cancel` necessarily follows a block already sent; deleting
   the dedupe entry is exactly what a replay needs. The tombstone now survives
   `Cancel`, and a `Cancel` is an ignored control frame.

### Medium

5. **Whole-piece verification was unmetered and not single-flight.** The
   engine's verified resume bitfield is now trusted at registration, so normal
   completed torrents serve from range reads without re-hashing; a piece not in
   that bitfield is verified under a four-permit semaphore, with a second cache
   check after acquiring the permit. A 1-byte first request can still trigger
   one whole-piece hash for an unverified piece, but concurrency is bounded.
6. **Incentive accounts with a large current window and no receipt never became
   evictable; dust and zero/disputed receipts also blocked eviction.** The
   eviction predicate now treats sub-minimum debt and zero/None-agreement
   receipts as evictable, and an idle account (one hour without activity) is
   evictable even if its current window never reset because no receipt arrived.
7. **The eager periodic dust write-off erased sub-minimum debt every 5 s.** It
   is removed; dust accumulates until it is payable or the account/session is
   removed. The lifecycle guard still ignores dust for removal.
8. **Evicted sessions leaked seeder registry entries and announces.** The
   announce scan prunes registry entries whose session no longer exists and
   cancels their tokens. `register_if_live` refuses to replace a live newer
   session's entry, closing the completion-vs-re-add interleaving.
9. **A failed magnet reported `Seeding`/100 % and spawned peer tasks.** A stub
   with no pieces, no piece count, or zero size now ends `Stopped` before the
   scheduler; `Seeding` requires a positive total size. The metadata loop
   filters peers before `take(8)` and bounds each connect by its deadline.
10. **The non-staking payment fallback could spend the main stake coin.** When
    a payment must use stakeable coins, the largest stakeable outpoint is kept
    as the reserve; a smaller stake coin may pay, otherwise the payment fails
    closed. The largest-first selector no longer silently drains the main
    stake.
11. **`clear_piece` was not owner-scoped and a false `mark_received` was
    ignored.** A task can no longer clear a piece reclaimed by another task; a
    block accepted after losing ownership drops its stale assembler instead of
    keeping it or writing it.
12. **Engine upload dedupe/counter bugs.** The downloader's upload path now
    inserts the 10 s tombstone only after a successful send, and the
    outstanding-request counter is decremented on the seed-ratio early return.
13. **A tracker-initialization failure left a session `Connecting` forever.**
    It now ends `Stopped`; a 15-minute no-verified-progress watchdog stops a
    keepalive-only/incomplete download.
14. **IPv6 hosts could rotate inside their /64.** The per-IP cap now aggregates
    IPv6 by /64.
15. **Early address/ID edges.** The engine skips the second address wait when
    the peer's address arrived early and range-checks the peer's `ut_vtr` id;
    the seeder marks its address sent only on a successful write.

## Deferred / accepted

| Item | Status |
|---|---|
| Manual or disputed removal can clear an unsettled account | The pass-9 live run reproduced it: node1's seeder session retained 1,171,875 sats of settled earned credit after the downloader's session was DELETEd, with 0 paid and no owed balance. The operator runbook warns against deleting a session with pending owed; durable removal protection is future work. |
| Desktop/no-settlement-loop account eviction and retained-session cap | The Tauri remove path works; automatic eviction of a positive receipt can still stall at the retained cap. A shared maintenance task is future work. |
| Ledger dust, lifetime budget persistence, exhausted-budget silence, confirmed-intent idempotency | Low crash-window/bookkeeping items; documented. |
| Per-connection receipt epoch/cumulative counters for overlapping connections | Our engine deduplicates tracker peers; third-party multi-connection seeders can underpay. |
| Engine advertises port 6881 with `left=total` while the seeder advertises the real port | Discoverability inefficiency, not correctness. |
| `StakeReserved` retries every 5 s; no backoff/status | Log churn on a single-UTXO staker; operator action is to fund a second coin. |
| Manual send/DEX/swap/cold-stake funding does not apply the stake reserve | Runbook warning; shared selector is future work. |
| Pass-9 live multi-peer split not repeated | The first task completed the whole torrent; the pass-8 split (4,456,448 + 8,126,464 bytes) remains the live proof. The scheduler ownership tests pass. A forced complementary-half test was rejected because the engine correctly refuses to register an incomplete seed session. A two-peer automated E2E remains future work. |
| Register-if-live, receipt-throttle, BEP-10-id, frame-cap, per-IP, and completion tests | The manual E2Es and live fleet cover the paths; targeted automated tests remain future work. |
| Observation file contamination / duplicate `+2h` labels | Evidence hygiene only. |

## Corrections to pass 8

Pass 8 claimed the completion fix was confirmed by node2's 36 s registration.
That 36 s was the 35 s final-receipt grace; the silent-peer case was not fixed
and the live node3 log finalized **178 s** after the last block. Pass 9 adds
10 s receive slices and the 15-minute progress watchdog. The pass-9 first
torrent finalized ~33 s after the add (the grace), not 178 s. The pass-8 report
has been annotated and marked superseded; the `review-ef1d1a1` fleet was
retired and its window does not count.

## Verification

- Workspace: **1,523 passed, 0 failed, 13 ignored** across 56 groups (rc=0).
- `torrent` lib: 145 tests; RPC 215; desktop 34; Clippy clean.
- Image `vtorrent/node:22056d4`, ID
  `sha256:390da969fab0e254f38cbc6bbb4bd85d76a5c5d4e376d6e174f994d66e147b9b`;
  extracted binary
  `b04131819f8c8062b47c803f3ed6df62facf277d041a8d0c15b425b3ae3ee177`.
- Both ignored E2Es pass on that image binary: the seeder serves 6 MiB over 24
  verified pieces and signs a receipt covering all of them; the download path
  settles a signed receipt and pays 585,937 satoshis once, with a restart that
  does not pay again.
- Live first torrent (`583508d8...`): node2 served all 12,582,912 bytes, node1
  0; node3 downloaded in ~1 s and finalized at 06:28:20Z. node2's session
  settled earned 1,171,875 sats; node3 paid **585,937 sats**, payment
  `f3b1bd0ac6bea750baf622feaab6b336ba6e582cddcd72f7b35adcff22acfe36`,
  confirmed at height 4. node2's transaction list shows the 585,937-sat
  receive; its balance is 200.006072 VTR including the 200 VTR allocation and
  stake rewards. node3 staked height 4 at 06:27:59Z while the payment was in
  flight; it later staked
  through height 17, with one height-11 block orphaned by a depth-1 reorg. At
  capture the status reported 10 blocks staked, 200 VTR immature and
  49,413,063 sats eligible; the 200 VTR principal was preserved.
- Live second torrent (`e44da8f2...`): node1 served all 12,582,912 bytes, node2
  0. The test manually DELETEd the first downloader session; that cleared the
  unpaid account. node1's seeder session still shows 1,171,875 sats of settled
  earned credit and 0 paid; node3 shows 0 paid and 0 owed. This is the
  documented removal residual, not a clean payment run.
- Health at 2026-11-23T06:41:12Z: heights 16/16/16, same tip `4b557b58...`,
  restarts 0/0/0, Prometheus 3/3, cron 1.
- Evidence: `/mnt/temp/vtorrent-ng-evidence/soak-22056d4/`, including
  `deployment.txt`, `e2e/`, `workspace-tests.log`, and
  `transfer-two-peer/TRANSFER.md`.
- The `review-ef1d1a1` fleet was retired at height 197 (tips agree, restarts
  0); its window is not release evidence. The production `vtr-node*` soak was
  not touched.

## Artefacts

- Reviewed head `ef1d1a1`; pass-9 fixes committed as `22056d4`.
- Prior pass: `docs/2026-11-23_ai-supplementary-review-pass8.md`.
- Review package: `docs/security-review-request-2026-10-27.md` and
  `docs/security-review-cover-note.md`.
- The `review-17db693`, `review-16f9a32`, and `review-ef1d1a1` windows are
  superseded and archived; none counts toward the 168 hours. The current
  window is `review-22056d4`, started 2026-11-23T06:26:05Z.
- The independent human review is not yet commissioned. The AI passes are
  supplements; the human review and the full exact-image window remain gates.
