# Handoff — 2026-11-25 — pass-15 candidate

Read this first, then `AGENTS.md`, `docs/2026-11-24_ai-supplementary-review-pass14.md`,
`docs/2026-11-25_ai-supplementary-review-pass15.md`,
`docs/mainnet-readiness.md`, and `docs/mainnet-launch-runbook.md`.

This handoff is a point-in-time state (2026-11-25T04:52Z). Re-query the live
runs/fleet before acting.

## Current candidate and window

- **Code candidate:** `be78829` (`be7882966afc3457ecec71380d6c4b58b0933d5a`)
- **Docs tip:** `24bd240` (plus the handoff commit)
- **Post-candidate CI-only child:** `f85cddd` — adds an explicit
  `rustup target add` in `.github/workflows/build.yml`. No daemon/UI source
  changed. This is the **only** expected non-doc diff after `be78829`.
- **Image:** `vtorrent/node:be78829`, ID
  `sha256:fa0856a025bba41c5a9d54140c3471fb67d732a9259e603d2fb069a853815f99`
- **Extracted binary:**
  `58462cc05ab1877264736734699d974d1c9e7396ea5ae988a626e3b504fede5f`
- **Window:** `review-be78829`, `2026-11-25T01:19:30Z` → `2026-12-02T01:19:30Z`
- **Fleet:** `review-be78829`, RPC `23951–23953`, Prometheus `23973`,
  secrets `target/review-be78829-secrets/`, evidence
  `/mnt/temp/vtorrent-ng-evidence/soak-be78829/`
- At 04:52Z: height 212, tips agree, staking on nodes 2/3, Prometheus 3/3;
  last observation 01:20:29Z; next cron 05:25Z.
- Suite: **57 groups, 1,555 passed, 0 failed, 13 ignored**
- E2Es on the image binary: seeder 2.12 s, incentive 26.20 s
- Two clean builds: identical binary; image IDs `fa0856a0…` (deployed) and
  `4216ec79…` (repro) differ only in Docker metadata.

**Do not change daemon/UI code or restart the window unless a release gate
fails.** Any code change invalidates the exact-image window.

## In-flight CI

- `45540365425` — tag `v2.0.0-rc3` (on `f85cddd`): desktop matrix + all gates.
- `45540352575` — main `f85cddd` (CI target fix).
- `45524422138` — main `be78829`: **success**.
- `45525095359` — tag `v2.0.0-rc2` on `be78829`: **failure**, only the Intel
  macOS job (arm64 runner lacked `x86_64-apple-darwin`); Windows, Apple Silicon
  macOS, and Linux passed. `f85cddd` fixes it; rc3 is the rerun.
- `45510052112` — stale `v2.0.0-rc1`/`a0724dd`: failure; superseded, tag
  deleted, no release created.

Watch with:

```bash
gh run view 45540365425 --json status,conclusion,jobs
gh run view 45540352575 --json status,conclusion,jobs
```

## Release gates and next actions

1. **Finish the rc3 desktop matrix.** If green, archive its job logs under
   `soak-be78829/ci-desktop/`, confirm no draft release was published, update
   `docs/2026-11-25_ai-supplementary-review-pass15.md`/`docs/soak-log.md`, and
   commit docs with `[skip ci]`. If a platform fails, diagnose; the CI target
   install is the known Intel macOS fix.
2. **Commission the independent human review.** The package is
   `docs/security-review-cover-note.md` + `docs/security-review-request-2026-10-27.md`;
   target is `be78829` / image `fa0856a0…` / window `review-be78829`. The
   package documents the expected freeze diff (`.github/workflows/build.yml`
   only) and the residual risks. Sending it needs the user.
3. **Keep the 168-hour window running** to `2026-12-02T01:19:30Z`. The cron
   observes every 6 hours, the watchdog hourly, and the helper checks
   Prometheus `query_range` for down samples/coverage gaps. Do not touch the
   fleet or change code.
4. **Production soak decision.** A host Docker snap auto-refresh at
   2026-11-24T13:18:37Z restarted `vtr-node*`; the chain replayed and is
   healthy, but its continuous uptime is broken. The production daily cron was
   restored to the original window start `VTR_SOAK_WINDOW_START=1792550874`
   (`2026-10-21T02:47:54Z`). The user must decide whether to restart the
   production soak clock or record an exception. Do not touch the containers.
5. **Optional swap rerun on `be78829`.** The four real-bitcoind tests and
   60/60-cycle soak passed on `a0724dd`; the pass-15 changes are Tauri repair +
   RPC force-remove only. A fresh run is nice-to-have, not a known gap.
6. **Remaining evidence:** real legacy `wallet.dat` differential needs a real
   file from the user. Production signing key/certificate is **not** generated
   and needs explicit authorization. Height-10,000 checkpoint is post-launch.

## Environment quirks (important)

- **Docker snap refreshes are held:** `sudo snap refresh --hold docker`. Do not
  unhold during the window. The snap refresh caused the 2026-11-24 restart.
- **`/tmp` is a 31 GB tmpfs with ~1 M inodes.** Full daemon test suites exhaust
  it (`ENOSPC`). Run tests with
  `TMPDIR=/mnt/temp/pass15-tests` (992 GB free). Old `/tmp` test dirs were
  cleaned; do **not** blindly `rm -rf /tmp/opencode` — it contains prior work.
- **Push as `vtorrent`.** `gh auth switch --user vtorrent` may not supply the
  credential helper; use:
  ```bash
  export GH_TOKEN=$(gh auth token --user vtorrent)
  git -c credential.helper='!f() { echo username=vtorrent; echo password=$GH_TOKEN; }; f' push origin main
  ```
- **Cron (user):**
  - production: `10 16 * * *` `VTR_SOAK_WINDOW_START=1792550874`
  - candidate: `25 */6 * * *` `SOAK_NAME=review-be78829`
    `SOAK_WINDOW_START=1795569570`
  - watchdog: `5 * * * *` `SOAK_MAX_AGE_HOURS=8`
- **Old windows retired and must not count:** `review-e091936`,
  `review-80f9eef`, `review-a0724dd`. Their evidence is archived under
  `/mnt/temp/vtorrent-ng-evidence/`.
- **Do not touch:** `checkpoint.md`, `graft/`, `research/` (pre-existing
  untracked user work).

## Verification commands

```bash
# Candidate/fleet status
KEY=$(cat target/review-be78829-secrets/api-key)
for n in 1 2 3; do curl -fsS -H "X-Api-Key: $KEY" \
  http://127.0.0.1:$((23950+n))/api/v1/info; done
scripts/soak-status.sh                 # production

# Full suite (use /mnt temp)
TMPDIR=/mnt/temp/pass15-tests cargo test --workspace --all-features

# E2Es on the exact image binary
E=/mnt/temp/vtorrent-ng-evidence/soak-be78829
VTORRENT_E2E_DAEMON_BIN=$E/vtorrent-daemon \
VTORRENT_E2E_DAEMON_SHA256=58462cc05ab1877264736734699d974d1c9e7396ea5ae988a626e3b504fede5f \
cargo test -p vtorrent-daemon --test torrent_seeder_e2e -- --ignored --nocapture
# same for torrent_incentive_e2e
```

## Evidence and docs

- `docs/2026-11-24_ai-supplementary-review-pass14.md` — four-reviewer review;
  critical manual/quote reserve bypasses, payout/import/debt/desktop findings.
- `docs/2026-11-25_ai-supplementary-review-pass15.md` — repair/force fixes,
  current image/window, CI/desktop status.
- `docs/2026-11-24_ai-supplementary-review-pass12.md` — prior findings.
- `docs/superpowers/plans/2026-11-24-pass13-remediation.md` — pass-13 plan.
- `/mnt/temp/vtorrent-ng-evidence/soak-be78829/` — candidate evidence:
  deployment, window, observations, suite, E2Es, builds, CI logs.
- `/mnt/temp/vtorrent-ng-evidence/soak-a0724dd/` — prior swap/desktop/rollback
  evidence (60/60 soak, four real-bitcoind tests).
- `/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/` — pass-13 evidence, rollback
  drill, real-bitcoind suite, live payment proof.

## Known residuals / decisions

- **Force remove leaves the ledger debt recorded.** It removes the session and
  cancels serving; settlement/write-off of the ledger debt is separate.
- **BTC-state repair is attestation-based.** It refuses existing/legacy state;
  the operator must attest no prior BTC history.
- **Desktop matrix** still needs the rc3 result.
- **Independent human review** is not commissioned.
- **Production soak clock** is a user decision after the Docker restart.
- **Signing** is not authorized; do not generate a production key.
- **bincode** is pinned to `=1.3.3`; the unmaintained advisory is accepted and
  documented.
- **Genesis/consensus parameters are frozen**; do not alter them.
