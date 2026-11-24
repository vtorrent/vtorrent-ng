# AI supplementary security review — pass 11: dependency audit

**Date:** 2026-11-23
**Reviewed head:** `d376506e4cf7bdf7f493353c4c93d3ed96bdad0d` (pass 10)
**Docs head reviewed:** `d991b2b1e3140389776a55251ab6aa401de11b12`
**Fix commit:** `e0919364bd3409e43a81016fc25b4300f80f71d6`
**Image:** `vtorrent/node:e091936`, ID
`sha256:7449a432d9cc4760fdcc4ac4e4f8bea249c13376035fed24452d2174ad82a589`;
extracted binary
`b7d24f475700e38893266528690f0ffc78604e47ba279981e7b1f68e88030055`
**Window:** `2026-11-23T14:30:53Z` to `2026-11-30T14:30:53Z` (in progress)

**Scope.** The canonical CI run failed only the `Cargo Audit` job; the Rust
tests, Clippy, benchmarks, frontend, and Machete jobs passed. A local audit of
the tracked `release/Cargo.lock` found two vulnerabilities:

- `cookie 0.18.2` — **RUSTSEC-2026-0594**, private cookie nonces can repeat in
  processes that fork; patched in `0.18.3`.
- `url 2.5.8` — **RUSTSEC-2026-0592**, stack overflow in `Url::origin()` on
  nested `blob:` URLs; patched in `2.5.9`.

## Fixed

- Updated `release/Cargo.lock` to `cookie 0.18.3` and `url 2.5.9`; the
  `url` update also brings `form_urlencoded 1.2.3`.
- `cargo audit --file release/Cargo.lock` now exits 0 with no vulnerabilities.
  Five allowed warnings remain: `bincode` (accepted and documented),
  `chrono`, `proc-macro-error`, `glib`, and `rand`; all are transitive and do
  not fail the audit.
- Locked metadata, formatting, and Clippy pass.

## Live result

The exact-image `review-e091936` transfer was served by node2 (12,582,912
bytes, matching SHA-1). node1 did not register because its wallet was locked
and the empty-address fallback still returns the pass-10 raw validation error;
the pass-8 split remains the live multi-peer proof.

- node3 downloaded the 12 MiB, then **184 DELETE attempts returned 400** with
  “Session has unpaid or in-flight incentive payments; retry after settlement”
  while the 585,937-sat debt was outstanding.
- After settlement, DELETE returned **200** at 14:38:23Z and removed session
  `9f4df57e-6299-4e2d-8d03-b1d91c6bae3c`.
- Payment `2497a30e3d6f2a5c3ae4d62eb671f9f2410e825478e49c510cfebd091a61b475`
  confirmed. The 400 was the session-level guard; the ledger-file backstop
  remains unit-tested for the pass-9 zero-account case.
- Health: heights 10/10/10, same tip `a9204ca4ee17...`, restarts 0/0/0,
  Prometheus 3/3.

## Deferred / accepted

| Item | Status |
|---|---|
| Five `cargo audit` warnings | `bincode` is the documented accepted risk; `chrono`, `proc-macro-error`, `glib`, and `rand` are transitive and should be assessed in the review package. They do not fail the audit. |
| Node1 locked-wallet/empty-address error | The live transfer remains single-seeder; return a clear wallet-locked/no-change-address error and add a desktop add-torrent E2E. The pass-8 split remains the live multi-peer proof. |
| Live ledger-backstop case | The live 400 was the session guard; the ledger-file guard is unit-tested. A future live test should clear the session account after settlement and confirm the file guard still blocks. |
| Incentive payment fee and stake age | Corrected: the live payment paid the **1,000-sat** fee floor, not zero. Input `85a4…:1` was the 200 VTR stake output (200 VTR + 1,934 sats reward); outputs were 585,937 sats to node2 and 19,999,414,997 sats change, leaving exactly 1,000 sats fee (20,000,001,934 − 585,937 − 19,999,414,997). Spending that output reset the only stake coin's age. The pass-13 structural reserve prevents a payment from spending the last structural stake outpoint. |
| Desktop settle/write-off UX | The removal guard prevents silent loss but can retain a session indefinitely on a desktop with no settlement loop. |
| Independent human review and exact-image 168 h | The pass-10 window is retired; the pass-11 window must run its own 168 hours. |

## Verification

- `cargo audit --file release/Cargo.lock`: rc=0, 0 vulnerabilities, 5 allowed
  warnings.
- `cargo metadata --locked`, `cargo fmt --all -- --check`, Clippy: pass.
- Both ignored E2Es on the image binary: seeder 2.26 s; incentive 26.31 s,
  paying 585,937 sats once.
- Full workspace suite: **1,528 passed, 0 failed, 13 ignored** (56 groups,
  rc=0).
- Live transfer/payment/removal: node2 served the full 12 MiB; 184 DELETE
  attempts returned 400 before settlement and 200 after it.
- Evidence: `/mnt/temp/vtorrent-ng-evidence/soak-e091936/`.
- The `review-d376506` fleet was retired at height 129; its window does not
  count. The production `vtr-node*` soak was not touched.

## Artefacts

- Pass-10 review: `docs/2026-11-23_ai-supplementary-review-pass10.md`
  (window retired).
- Pass-9 review: `docs/2026-11-23_ai-supplementary-review-pass9.md`.
- Review package: `docs/security-review-request-2026-10-27.md` and
  `docs/security-review-cover-note.md`.
- The independent human review and the full exact-image window remain gates.
  This AI pass is a supplement, not independent review.
