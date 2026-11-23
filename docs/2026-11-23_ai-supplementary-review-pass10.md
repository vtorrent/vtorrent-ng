# AI supplementary security review — pass 10

**Date:** 2026-11-23
**Reviewed head:** `22056d4fc6a22731636ad05c457c42c1632c54f4` (pass 9)
**Fix commit:** `d376506e4cf7bdf7f493353c4c93d3ed96bdad0d`
**Image:** `vtorrent/node:d376506`, ID
`sha256:131f4639db614b4c7a3a17575e061f31f98bbe83ebf864188872a7c7f6358718`;
extracted binary
`3c7e965360030bec92cb14920f51d76267602d5ee33f49900603b764085813ac`
**Window:** `2026-11-23T12:21:34Z` to `2026-11-30T12:21:34Z` (in progress)

**Scope.** The pass-9 live transfer reproduced a manual-removal residual: a
downloader session could be DELETEd after its receipt settled into the
daemon's durable incentive ledger, clearing the payer's debt while the seeder
still reported the earned credit. The desktop remove path had no guard at
all. This pass closes that path and re-runs the live removal case.

## Fixed

- **The RPC removal guard is ledger-aware.** The daemon installs a
  `TorrentRemovalGuard` on the shared RPC state. Before a session is removed,
  the daemon reads the durable incentive ledger and refuses removal when the
  same info hash still has an unpaid, pending, or in-flight debt at or above
  the payment minimum. A missing ledger file is harmless; an unreadable ledger
  fails closed. Dust below `MIN_PAYMENT_SATS` remains removable.
- **The desktop/Tauri removal path refuses a receipt-backed debt.** The
  desktop has no settlement loop, so a stored receipt is retained instead of
  silently discarded. The user sees a clear "settle before removal" error and
  the session's accounting remains visible.
- **Regression tests.** The RPC handler test fails before the guard is wired
  and passes after it; the ledger tests prove an outstanding debt blocks the
  same info hash, clears after payment, and leaves sub-minimum dust removable;
  the desktop tests prove a payable account blocks removal while dust remains
  removable.

## Live result

The live `review-d376506` transfer was served by node2 (12,582,912 bytes,
matching SHA-1); node1 did not register as a seeder because its wallet was
locked and the empty-address fallback returned a raw validation error. The
removal path itself was exercised against node3:

- node3 downloaded the 12 MiB, then **164 DELETE attempts returned 400** with
  “Session has unpaid or in-flight incentive payments; retry after
  settlement” while the 585,937-sat debt was outstanding.
- The payment was broadcast at 12:25:26Z and confirmed at height 7. After
  settlement, DELETE returned **200** at 12:30:53Z and removed the session.
- The node2 wallet received **585,937 sats**; node3 staked 7 blocks at the
  capture; the 200 VTR principal was preserved in the payment change, and the
  0.5 VTR faucet coin remained unspent.
- The 400 message was the **session-level** guard, not the ledger-file guard:
  the session account still carried `vtr_pending_owed_satoshis = 585937`, so
  the live ledger backstop was not independently exercised. The ledger guard
  is covered by the unit test for the pass-9 zero-session-account case.

Health at capture: heights 12/12/12, same tip
`ed7d712a71ecdd349199ab27b2147c980701c34dc5c9de66fc20176e026b755e`, restarts
0/0/0, Prometheus 3/3, window 2026-11-23T12:21:34Z → 2026-11-30T12:21:34Z.

## Deferred / accepted

| Item | Status |
|---|---|
| Live ledger-backstop case | The live run's session guard caught first. The ledger-file guard is unit-tested for the zero-account case; a future live test should clear the session account after settlement and confirm the file guard still blocks. |
| Node1 seeder registration | The node1 wallet was locked; `wallet/utxos` returned 400, the empty-address fallback produced “Invalid address: Expected 25 bytes, got 0”, and the seeder did not register. The pass-8 split remains the live multi-peer proof. Return a clear wallet-locked/no-change-address error and add a desktop add-torrent E2E. |
| Desktop settlement UX after a blocked removal | The guard prevents silent loss, but the desktop has no automatic settlement loop and no explicit "settle/write off" action. The next UX step is to add a settlement or explicit write-off path; until then the session and its debt remain visible. |
| Ledger read window | The guard reads the saved ledger file on removal; the in-memory session guard remains the first line. A shared in-memory ledger handle would remove the file-read window. |
| Manual or disputed removal outside the RPC/Tauri paths | Any future direct `SessionManager::remove_session` caller must go through the same guard. |

## Verification

- Focused tests: RPC handler guard 1 passed (it failed before the guard call);
  ledger 8 passed, including the dust case; desktop 2 passed.
- Clippy/fmt: clean.
- Workspace: **1,528 passed, 0 failed, 13 ignored** across 56 groups (rc=0).
- Both ignored E2Es pass on the image binary: seeder 2.16 s; incentive
  26.32 s, paying 585,937 sats once.
- Live removal: 164 attempts, 400 until settlement, 200 after confirmation.
- Evidence: `/mnt/temp/vtorrent-ng-evidence/soak-d376506/`, including
  `transfer-two-peer/delete-attempts.log`, `payment-tx.json`,
  `node3-staking.json`, and `health.txt`.
- The `review-22056d4` fleet was retired at height 347 (tips agree, restarts
  0); its window does not count. The production `vtr-node*` soak was not
  touched.

## Artefacts

- Pass-9 review: `docs/2026-11-23_ai-supplementary-review-pass9.md`.
- Review package: `docs/security-review-request-2026-10-27.md` and
  `docs/security-review-cover-note.md`.
- The independent human review and the full exact-image window remain gates.
  This AI pass is a supplement, not independent review.
