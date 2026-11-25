# Pass-15 remediation — BTC-state repair and force-remove

**Date:** 2026-11-25
**Code candidate:** `be78829`
**Image:** `vtorrent/node:be78829`, ID
`sha256:fa0856a025bba41c5a9d54140c3471fb67d732a9259e603d2fb069a853815f99`
**Extracted binary:**
`58462cc05ab1877264736734699d974d1c9e7396ea5ae988a626e3b504fede5f`
**Window:** `2026-11-25T01:19:30Z` to `2026-12-02T01:19:30Z`
(`review-be78829`)

This pass closes the remaining pass-14 functional findings. The pass-14 report
remains the review record; this is the remediation and evidence update.

## Fixed

| Finding | Fix |
|---|---|
| Pre-pass-13 imported wallets cannot start (missing BTC state) | `repair_btc_state` requires the wallet passphrase/TOTP and an explicit no-prior-history attestation, refuses existing state and legacy network-only state, then creates mainnet+regtest state. Settings exposes the one-time action. |
| Disputed/agreed debt has no force path | `force_remove_torrent` is available at `DELETE /api/v1/torrent/:id/force` and as an authenticated Tauri command. It removes the session and cancels serving, leaving the ledger debt recorded for separate settlement. Normal removal still refuses. |
| Desktop legacy BTC guard missed network-only state names | `ensure_no_legacy_btc_wallet` now checks `btc_utxos-mainnet.json`, `-testnet.json`, and `-regtest.json` as well as `btc_utxos.json`. |
| Test coverage for the above | Tauri repair create/refuse-overwrite and RPC refused-normal → forced-removal regressions added. |

## Verification

- Full workspace suite: **57 groups, 1,555 passed, 0 failed, 13 ignored**.
- Both E2Es on the image binary: seeder **2.12 s**, incentive **26.20 s**.
- Focused Clippy, UI lint/tsc, and UI tests pass.
- Two clean `--no-cache` builds produced the identical binary
  `58462cc0…`; image IDs `fa0856a0…` (deployed) and `4216ec79…` (repro) differ
  only in metadata.
- New window `review-be78829` started at 01:19:30Z: height 4, tips agree,
  staking 2/3, Prometheus 3/3, 3/3 samples, zero down.
- Canonical CI for `be78829` is run `45524422138` (success). The first desktop
  matrix run `45525095359` on `v2.0.0-rc2` failed only the Intel macOS job because
  the arm64 runner lacked `x86_64-apple-darwin`; CI-only `f85cddd` adds an
  explicit `rustup target add`. The rerun `v2.0.0-rc3`, run `45540365425`, is
  **green on all four desktop platforms** (Linux, Windows, Intel macOS, Apple
  Silicon macOS) and every other job. No release was published.

## Candidate provenance

The exact daemon image and window are `be78829` / `review-be78829`. The later
`f85cddd` commit only adds the desktop matrix target install in
`.github/workflows/build.yml`; no daemon or UI source changed. The expected
`be78829..HEAD` non-doc freeze diff is that single CI file.

## Remaining

- Independent human security review and the full 168-hour window.
- Production soak clock decision after the Docker restart.
- Real legacy `wallet.dat` differential, signing authorization, and the
  post-launch height-10,000 checkpoint.
