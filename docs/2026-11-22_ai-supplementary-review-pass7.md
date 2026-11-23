# AI supplementary security review — pass 7

**Date:** 2026-11-22
**Review head:** `d5e80e19ea3b1a7df835a049629a408cf388c2b2`
**Fixes committed:** `17db693b2a0470db47e9693dd07dbe024542f404`
**Scope:** the delta `2fdd9aa..d5e80e1` — inbound torrent serving, the engine's
pre-extension bitfield fix, the staking/mempool-spend filter, RPC/desktop
wiring, and the fleet script — plus a full-file review of the touched code and
the settlement/journal path it depends on.

**Method.** Four independent reviewer agents covered separate surfaces
(inbound seeder and peer wire; engine and scheduler; staking and payments;
RPC/desktop/operations) and returned severity-ranked findings with file/line
references. The author reviewed the receipts/announce/lifecycle paths directly
and triaged every finding, discarding duplicates and false positives. All code
changes below were made after triage; the verification section lists the
coverage, and pass 8 adds the missing regressions and corrects two
overstatements. No reviewer saw
or edited the running fleet.

This is an AI-assisted supplement. It does **not** satisfy the independent human
security-review gate, and the 168-hour exact-image soak remains open.

## Fixed

### Critical

1. **Multi-peer downloads could never complete a piece.** Each peer task kept a
   private `PieceAssembler`, but `next_block` leased blocks of the same global
   piece to every task. With two seeders, each assembler held a subset, no
   piece verified, and after the 90 s lease timeout the cycle repeated. One
   extra tracker peer could keep a torrent at zero progress forever. This
   predates pass 7 but was hidden by the single-seeder E2E extensions.
   - `scheduler.rs` now assigns each partially downloaded piece to exactly one
     task (`piece_owner`), continues that owner's blocks, and releases ownership
     on completion, failure, exit, or lease expiry. `PieceOwnership` releases a
     task's pieces when it exits.
   - A peer is only leased pieces it advertised (`peer_has_piece`), not pieces
     any peer happens to have.
   - Received blocks keep their lease (so they are not re-requested) but no
     longer count as in-flight; the pipeline depth is recomputed from the
     scheduler every iteration, and expired/cleared leases cannot wedge it.
   - Verification and disk-write failures now clear the piece so it can be
     re-fetched instead of poisoning it for 90 s.
   - New scheduler tests: single ownership, peer availability, release, and
     received-versus-in-flight accounting.

### High

2. **The seeder accepted a receipt flood.** Every `Extended` frame with the
   peer's `ut_vtr_receipt` id ran a bincode decode, ECDSA verify, session write
   lock and a journal append with `sync_data`. A peer could force one fsync per
   frame by changing the epoch. The download path already had a 30 s throttle;
   the seeder now applies the same per-connection limit before decoding.

3. **Every 16 KiB block request read the whole piece.** A 1-byte request was
   valid, so one connection could force a full piece read per request (up to
   `MAX_PIECE_LENGTH`, 16 MiB) and 32 connections could saturate disk I/O. The
   original duplicate window was also a no-op (`Instant::now()` never survives
   `retain(deadline > now)`).
   - Added `read_block_from_disk`, which reads only the requested range after
     the piece has been verified once. The seeder and the engine's upload path
     both use it.
   - Restored a real 10 s duplicate-request window, bounded to 4096 entries per
     connection, so replayed requests are not re-read or re-counted.

4. **Piece-sized inbound frames allowed hundreds of MiB of attacker-chosen
   buffering.** The seeder sized its frame cap from the torrent's piece length
   (the engine copied the same pattern). Both now cap established connections
   at `MAX_INBOUND_FRAME` (256 KiB), independent of piece length. The largest
   legitimate payload is a 128 KiB bitfield, extension messages are a few KiB,
   and blocks are 16 KiB.

5. **The seeder accepted receipts for a different torrent and journaled under
   the attacker's info-hash.** With a bidirectional peer, fake-hash receipts
   could duplicate the account's unpaid debt into the journal, and `merge_journal`
   turns each record into a payable ledger debt at restart. The seeder now
   requires `receipt.info_hash == the connection's torrent` and journals under
   the connection's hash.

6. **Deleting a session did not stop open connections; eviction and the
   completion race left stale serving.** `serve_connection` held an `Arc` and
   never re-checked the registry or session, so a peer could keep downloading
   after `DELETE`. A completion that raced a delete could also re-register a
   removed torrent.
   - Each `SeedTorrent` has a cancellation token; `unregister` cancels it and
     the serve loop selects on it and the global token.
   - Before serving each block, the seeder checks that the session still
     exists; an evicted session stops being served.
   - RPC and Tauri check completeness, register, then re-check the session and
     unregister if it disappeared. Duplicate info-hashes are rejected so one
     registry entry cannot alias two sessions.
   - The Tauri remove path unregisters. It was initially given the RPC's
     outstanding-incentive guard; pass 8 removed that guard because the
     desktop has no settlement/payment loop and the guard only dead-ended
     removal.

7. **Magnet downloads registered the pre-engine stub metainfo.** `add_torrent`
   captured the stub (zero pieces, possibly zero size) before `run_engine`; for
   a magnet the real BEP-9 metainfo only arrives inside the engine. The seeder
   then advertised an empty torrent while `seeded_torrents` said 1, and a failed
   magnet (size 0) passed `is_complete` trivially. Registration now reads the
   session's current metainfo and requires non-zero size, non-zero piece count,
   and a complete piece-hash list.

8. **A pending payment could halt block production on a single-UTXO staker.**
   The staking filter removed the only eligible coin, the node stopped staking,
   and no other node could confirm the payment. The payment builder now
   protects the last staking-eligible UTXO: if the payment would spend every
   available staking-eligible coin, it fails with a clear error and the debt is
   retried later. A staking node that also pays incentives must keep a second
   spendable coin. The staking loop also re-checks the selected outpoint under
   the chain+mempool locks immediately before `add_block`, closing the
   check-then-act window that could still let a payment be evicted by our own
   coinstake.

### Medium / low

9. **A disputed receipt was never cleared.** It blocked settlement and
   `DELETE` forever, even though a dispute is not a liability (nothing is paid
   without agreement). A disputed receipt is now dropped with a warning and
   journaled, so a later cumulative receipt can settle.

10. **An attacker could permanently fill a session's incentive account map**
    with valid addresses and 1-byte transfers; once at 10,000, new peers were
    untracked and billing stopped. Accounts with no receipt, no debt, and less
    than `MIN_PAYMENT_BYTES` are now evictable. Added a 10,000-account cap test.

11. **The final receipt window was systematically unpaid.** The seeder sends
    receipts at most every 30 s; the downloader closed 10 s after completion.
    The post-completion read grace is now 35 s, covering the sender's cadence.

12. **An early `ut_vtr` address was dropped and the handshake scan could be
    stretched.** The discovery phase now records addresses seen before the
    extension handshake and uses one 30 s deadline instead of ten 180 s
    per-message deadlines.

13. **Tracker announce/discovery defects.** A torrent registered after startup
    was not announced until the next 30-minute tick; every announce was
    `completed`; the engine and seeder shared the fixed peer id `[0x2d;20]`;
    the seeder could advertise port 0. Announces now scan every 60 s, send
    `started` once and no event after, use a per-process random peer id, and
    fall back to the bound port.

14. **`seeding: true` was reported before the listener bound.** The daemon now
    binds first and fails startup on error, so the RPC status cannot lie.

15. **Wallet address validation.** A seeding session with an empty or malformed
    `wallet_address` produced receipts no peer could verify. The RPC rejects
    that combination; non-seeding sessions keep the existing behaviour.

16. **Peer bitfields cost one byte per piece per peer.** They are now packed
    bits, so 200 peers on a 1M-piece torrent cost ~25 MB instead of ~200 MB.

17. **Docs corrected:** the 10 s duplicate window and 64-outstanding rows now
    match the code (the latter removed), the BEP-10 paragraph no longer implies
    that BEP-10 alone is payable, the restart requirement is explicit, the
    fleet procedure uses `docker cp` and the API-key header, and the broken
    multi-line example was fixed.

## Deferred, accepted, or documented

| Item | Status |
|---|---|
| Automatic invalidation of a payment whose input is confirmed-spent by another tx | Not implemented. The demonstrated self-conflict is fixed; the on-call runbook documents escalation. |
| Session persistence across daemon restart | Not implemented. Torrents must be re-added to resume serving; documented in `docs/torrent-seeding.md`. |
| Free-riding by a custom BEP-10 client that never sends a receipt | Inherent to voluntary post-hoc payment. Docs state it; upload limits and seed-ratio caps bound it. |
| `seeded_torrents` may count an entry whose session was evicted | Serving stops (session check), but the count is stale until removal/restart. Low. |
| Budget reservation lost on a crash between journaling and intent write | Low; a restart may consume up to one payment of the cap. Logged for future work. |
| Confirmed-payment idempotency across journal/ledger at startup | Low crash-window issue; not changed in this pass. |
| Seeder `verified` cache never revalidates a locally modified file | Local write access is required; documented as a trust assumption. |
| A determined peer can still inflate the seeder's claim with offset-shifted re-requests and force a dispute | The 10 s exact-request window blocks the trivial replay; a full served-byte union is future work. Payment is voluntary. |

## Verification

- Workspace: **1,518 passed, 0 failed, 13 ignored** across 56 groups (rc=0).
- `torrent` lib: 140 tests, including the new scheduler/session/disk-range
  regressions.
- Image `vtorrent/node:17db693`, ID
  `sha256:76b6b3f96015147b763b25788640f105eb6600b33b2f7c5feaf60e5d155dd9ba`.
  Extracted binary:
  `3dfe526952a3b850abe21b7516ad10076ebaafd8b2aedc980411d3764f611d73`.
- Both ignored E2Es pass on the image binary: the seeder serves 6 MiB and
  signs a receipt; the downloader pays 585,937 satoshis once.
- Live **two-seeder** transfer on the replacement fleet: 12 MiB in 1 second
  with a matching SHA-1; node1 served 4,980,736 bytes and node2 7,602,176
  bytes, so every piece completed in exactly one owning task. node2 earned
  708,007 sats; node3 paid it 354,003 sats from a 0.5 VTR non-staking coin
  (payment `bd3add87...`, confirmed at height 4, final at height 9) while
  staking blocks at heights 5–7. Evidence:
  `/mnt/temp/vtorrent-ng-evidence/soak-17db693/transfer-two-peer/`.
- Window: 2026-11-22T17:28:51Z to 2026-11-29T17:28:51Z; the `d5e80e1` fleet
  had its last clean, tip-agreeing observation at height 102 and was retired
  while the final snapshot showed heights 117/117/118 (a block in flight), with
  zero restarts; it is archived as superseded.

## Artefacts

- Reviewed head `d5e80e1`; fixes committed as `17db693`.
- Prior pass: `docs/2026-11-21_ai-supplementary-review-pass6.md`.
- Review request and cover note are repointed to the new head once the image
  and replacement window are published.

## Corrections after pass 8

Pass 8 reviewed these fixes and found several follow-ons. This section records
where the text above overstated the result; the fixes are in
`docs/2026-11-23_ai-supplementary-review-pass8.md`.

- "Covered by tests" was too strong. The pass-7 delta added scheduler, eviction,
  disk-range, and staking-helper tests, but no automated multi-peer E2E and no
  tests for the receipt flood, frame cap, receipt binding, magnet registration,
  DELETE-vs-open-connection, or the staking/payment reserve. Pass 8 adds the
  missing regressions where practical and documents the manual coverage.
- The Tauri remove guard was a desktop regression, not a fix: the desktop runs
  no settlement/payment loop, so a stored receipt would block removal forever.
  Pass 8 removed the guard (the unregister remains).
- The 35 s final-receipt grace raced the receiver's 30 s window. Pass 8 uses a
  25 s receiver window so a 30 s sender cadence is not dropped by timer drift.
- The 10,000-account eviction predicate used lifetime bytes, so settled peers
  were immortal. Pass 8 uses the current settlement window and added a
  settled-account test.
- The "peer availability" scheduler test asked a peer that had the piece, so it
  passed on the pre-fix code. Pass 8 asks a peer that lacks the piece.
- The pass-7 ownership could be pinned by one received block; pass 8 adds a
  progress deadline, reclaim, and per-peer cooldown.
- `d5e80e1` was retired while the final snapshot showed heights 117/117/118
  (a block in flight); 102 was the last clean, tip-agreeing observation.
- `docs/rpc-api.md`'s torrent-add schema and the DELETE refusal text were wrong;
  both are corrected in pass 8, as is the runbook's quoted DELETE error.

