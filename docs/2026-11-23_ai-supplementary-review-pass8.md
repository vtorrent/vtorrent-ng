# AI supplementary security review — pass 8

**Date:** 2026-11-23
**Review head:** `17db693b2a0470db47e9693dd07dbe024542f404`
**Fixes committed:** `ef1d1a1fb43008f031687cb0a6ccda6e64395d17`
**Scope:** the pass-7 fixes themselves — scheduler piece ownership and lease
accounting, the seeder's receipt/range/cancel/announce changes, the
staking/payment protection, and the RPC/desktop lifecycle guards.

**Method.** Four independent reviewer agents covered the scheduler/engine, the
seeder, payments/staking/RPC, and the tests/docs/ops delta. The author reviewed
the new invariants directly and triaged every finding. Changes are covered by
deterministic tests where practical; the manual E2Es and fleet runs cover the
rest, and the remaining gaps are listed below. The running fleet was never
modified by a reviewer.

This is an AI-assisted supplement. It does **not** satisfy the independent human
security-review gate, and the 168-hour exact-image soak remains open.

## Fixed

### Critical

1. **A block rejected by the assembler was marked received and lost forever.**
   Pass 7 marked the lease satisfied before the per-task assembler cap, the
   shared budget, and `add_block`. Any rejection left the lease `received` with
   no stored copy; `next_block` skips leased offsets and received leases never
   expire, so the piece could never complete and no other task could take it.
   The lease is now marked only after `add_block` accepts the block, and the
   cap/budget paths release the lease immediately so it can be retried.

### High

2. **One received block pinned a piece to a choked peer forever.** Received
   leases were retained (correctly) but ownership only disappeared on task
   exit, so a peer that delivered one block and then stopped answering kept the
   piece. A stalled owner was also the only task allowed to serve it.
   - Each ownership entry records the last accepted block. After 180 s without
     progress the scheduler reclaims the piece, removes its leases and owner,
     records a 60 s per-peer cooldown, and queues the piece for the owning task.
   - The task drains its reclaimed pieces, drops the partial assembler, and
     releases the assembly budget. Another peer can then take the piece; the
     stalled peer is not immediately handed it back.
   - New test: a stale owner is reclaimed and cooled down.

3. **A peer task with no work could delay finalization for 180 s.**
   The completion check ran only after a received message; `KeepAlive` fell into
   `Ok(_) => continue`, and a silent peer sent nothing at all. A completed task
   waited for the 180 s receive timeout before returning, so `run_engine` did
   not finalize and the completed torrent was not registered with the seeder in
   time (observed live when a second seeder node stayed `Connecting` while the
   first served the whole torrent). Completion is now checked at the top of the
   loop, and KeepAlive still falls through.

4. **A corrupt peer was retried forever.** A SHA-1 failure cleared the piece
   and immediately re-selected it, often from the same peer. The engine now
   counts corrupt pieces per task and drops the peer after three, releasing its
   pieces and budget instead of looping on the same data.

### Medium

5. **Receipt verification was throttled per connection, not per peer.** A
   reconnect reset the 30 s window, so one peer could force an ECDSA verify and
   a journal fsync per connection indefinitely. The throttle is now a bounded
   per-address map in `SeederState`, shared across that peer's connections.

6. **BEP-10 extension IDs were used in the wrong direction.** A peer sends to
   us on the IDs *we* advertise; we send on the IDs *it* advertises. The seeder
   was sending its address on its own id and dispatching receipts on the peer's
   id; the engine was waiting for the peer's id and dispatching receipts on it.
   Our own clients advertise `ut_vtr=1, ut_vtr_receipt=2`, so the fleet worked,
   but a third-party with different IDs could not pay or be paid.
   - Added `OUR_UT_VTR_ID`/`OUR_RECEIPT_ID`; inbound address/receipt dispatch
     uses our IDs, outbound sends use the peer's advertised IDs.
   - The seeder waits for the peer's extension handshake before sending its
     address, so it can use the peer's id.
   - The engine parses the peer's address by payload rather than matching the
     peer's outbound id.

7. **A zero-value receipt could pin a session forever.** A peer could serve it
   one block, send a self-signed receipt claiming nothing, and
   `has_outstanding_incentives` treated the stored receipt as debt; DELETE then
   returned 400 forever. Zero-total receipts are now ignored, and the guard
   counts a stored receipt only when it agrees to a positive amount.

8. **Sub-minimum debt blocked deletion and eviction forever.** The payment loop
   refuses amounts below `MIN_PAYMENT_SATS` and never wrote off the remainder,
   so a 47-sat debt made the session permanently unremovable. The settlement
   loop now writes off sub-minimum residual debt (journaled) and the lifecycle
   guard treats it as settled.

9. **Incentive-account eviction fossilised on lifetime bytes.** The 10,000-entry
   map used cumulative `total_bytes_*`, so every peer that ever moved 10 MB was
   immortal and new peers stopped being tracked. Eviction now uses the current
   settlement window plus settlement state. Added a regression with a
   long-lived, settled account.

10. **The non-staking payment preference used a value heuristic.** It could
    reject a payment the non-staking coins could cover (largest-first selects
    the stake coin) and could fail when the non-stake sum was just below the
    heuristic. It now builds from non-staking coins first and falls back to all
    coins only on `InsufficientFunds`, applying the reserve check to the actual
    inputs.

11. **Session eviction/re-registration left a stale seeder entry.**
    `register` was `or_insert_with`, so re-adding an evicted torrent was a
    no-op against the old session id; the node advertised a dead seed and
    served nothing. `register` now replaces a differing entry and cancels its
    token; `unregister_session` removes only the caller's session, so a
    concurrent re-add is not removed.

12. **Magnet metadata lost the magnet's trackers.** BEP-9 returns only the info
    dict, so replacing the stub metainfo dropped its `announce`/`announce_list`;
    the completed magnet was registered but never announced. Those fields are
    preserved across the metadata fetch.

13. **Magnet metadata had no overall deadline.** A malicious peer could stall a
    magnet in `Connecting` for hours through ten 180 s reads per piece. The
    metadata exchange is now capped at 60 s over at most 8 peers and uses the
    256 KiB inbound frame cap.

14. **A remote `Cancel` could orphan a received block.** `clear_block` removed
    a lease even if its block was already stored, which could drop ownership
    and let another task re-lease a piece whose data the first task still held.
    Received leases now survive a peer's Cancel.

15. **Per-IP connection flooding.** One source could hold every seeder slot with
    silent handshakes. A bounded per-IP slot cap (8) now complements the global
    connection limit, and slots are released by a Drop guard on every exit.

16. **The duplicate-request window was inserted before a successful send.**
    Read/send failures and seed-ratio refusals left a 10 s tombstone, so a
    legitimate retry was dropped. The entry is now inserted only after a
    successful send, `Cancel` clears it, and the key includes the length.

17. **The advertised bitfield depended on the resume file.** A missing or
    unwritable resume file made the seeder advertise nothing and no peer could
    request anything. Registration only happens for a complete session, so the
    bitfield now advertises all pieces; each is still re-hashed on first serve.

18. **A corrupt piece forced a whole-piece read per block request.**
    Verification cached only success. Failures are now cached for 60 s, so a
    corrupt or missing piece cannot be used to amplify disk reads.

19. **Failed/incomplete engine runs stayed `Downloading` forever.**
    `Downloading` is non-terminal, so 64 unreachable adds exhausted the active
    session cap and made real completed sessions eviction targets. An engine
    that returns incomplete now marks the session `Stopped` (terminal).

20. **RPC wallet-address trimming/validation and the desktop removal guard.**
    The RPC now trims once and stores the validated value; a leecher cannot pass
    an empty address. The desktop has no settlement/payment loop, so its remove
    path no longer applies the daemon's incentive guard (which could only
    dead-end) while still unregistering the seeder entry.

21. **The ignored E2Es could silently test old binaries.** Both defaulted to
    pre-fix daemon paths and treated the candidate hash as optional. They now
    require `VTORRENT_E2E_DAEMON_BIN` and `VTORRENT_E2E_DAEMON_SHA256` and fail
    otherwise; the operator command is in `docs/torrent-seeding.md`.

22. **The "peer availability" scheduler test passed on the pre-fix code.** It
    asked a peer that had the piece. It now asks the peer that lacks it and
    asserts the other piece is leased. The seeder registry replacement and
    session-keyed unregister also have a direct test.

23. **The final-receipt grace still raced the receiver window.** The receiver
    dropped receipts within 30 s of the previous *arrival* while the sender
    sends every 30 s from *send* time; drift could drop the final window even
    inside the 35 s grace. The receiver window is now 25 s.

24. **Docs and evidence corrections.** The RPC add schema now shows
    `source`/`source_type`/`wallet_address`; DELETE's outstanding-incentive
    refusal, the runbook's exact error text, the E2E env requirement, the
    in-network tracker requirement, the `d5e80e1` retirement height, and the
    desktop-seeding limitation are documented. The pass-7 report carries a
    corrections section for its overstated coverage and Tauri-guard claim.

25. **The seeder's duplicate window and bitfield derive from the wrong state.**
    The dedupe entry existed before a successful send and ignored `Cancel`; the
    advertised bitfield came from a resume file that may be missing. The entry
    is inserted only after a successful send (keyed by piece/begin/length) and
    cleared on `Cancel`; a complete registered session advertises all pieces,
    with each still verified before its first block.

## Deferred / accepted

| Item | Status |
|---|---|
| Shared assembly budget is charged for the whole piece on first block | Mitigated by the 180 s stall reclaim and per-IP cap; charging received bytes is future work. |
| Receipt epoch/counters are per connection, not per seeder account | Our engine deduplicates tracker peers so one seeder is one connection; overlapping third-party connections can still underpay. |
| Engine announces port 6881 with `left=total` while the seeder announces the real port | The seeder's prompt announce is authoritative; the dead tracker entry remains a discoverability inefficiency. |
| A deterministically unpayable incentive (last staking UTXO) retries every 5 s | The runbook says to fund a second coin; backoff/status surfacing is future work. |
| Crash between budget reservation and intent write leaks the reservation | Low; no signed transaction exists, but the cap erodes. |
| Dispute clearing conflates "disputed" and "stale below baseline" in its log | Accounting is sound; log distinction is future work. |
| Reserve policy is not applied to manual send/DEX/swap/cold-stake funding | Those paths can still spend the last stake coin; the runbook warns. |
| Removed sessions do not write journal tombstones | Journal growth only; no payment liability. |
| Stale seeder registry entries after eviction without re-add | Serving stops via the session check; the announce task and `seeded_torrents` can still count it until removal/restart. Re-registration now replaces the entry. |
| Announce failures are recorded as announced; no `stopped` announce; serial 30 s/8-tracker loop | A failed first announce waits 30 minutes; removed torrents rely on the tracker's expiry. |
| `build_incentive_payment` convenience API does not apply the staking reserve | No production caller; the daemon uses `prepare_incentive_payment`. Mark it for removal. |
| No automated two-peer E2E; the multi-peer assembly proof is the manual fleet run | The scheduler ownership/cancel/stall tests cover the invariants; the live two-seeder transfer is evidence, not a CI test. |

## Verification

- Workspace: **1,523 passed, 0 failed, 13 ignored** across 56 groups (rc=0).
- `torrent` lib: 145 tests including the new stall, cancel-of-received,
  registry replacement, settled-account eviction, and dust regressions.
- Image `vtorrent/node:ef1d1a1`, ID
  `sha256:187ca4716ca47e7ab561be296d8ad1a1e4ba4819340030658027c0d905b16c94`.
  Extracted binary:
  `81ef84e97e39e6a829791d2757da2baacdacc177ee785864533f38f6988593b5`.
- Both ignored torrent E2Es pass on the image binary: the seeder serves 6 MiB
  and signs a receipt; the downloader pays 585,937 satoshis once.
- Live **two-seeder** transfer on `review-ef1d1a1`: both seeders registered
  before the download (node2 after 36 s, confirming the completion-check fix);
  node3 downloaded 12 MiB in 1 second from both peers, split 4,456,448 bytes
  from node1 and 8,126,464 from node2, with a matching SHA-1. node2 earned
  756,835 sats; node3 paid it **378,417 sats** from a 0.5 VTR non-staking coin
  (payment `6f47d4f2...`, confirmed at height 4, final at height 9) while
  staking blocks at heights 4, 6, 7, and 8. Evidence:
  `/mnt/temp/vtorrent-ng-evidence/soak-ef1d1a1/transfer-two-peer/`.
- Window: 2026-11-23T03:05:42Z to 2026-11-30T03:05:42Z.

## Artefacts

- Reviewed head `17db693`; pass-8 fixes committed as `ef1d1a1`.
- Prior pass: `docs/2026-11-22_ai-supplementary-review-pass7.md`.
- The `review-17db693` and `review-16f9a32` windows are superseded by these
  fixes and archived; neither counts toward the 168 hours. The current window
  is `review-ef1d1a1`.
