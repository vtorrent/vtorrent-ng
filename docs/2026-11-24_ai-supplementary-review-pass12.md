# AI supplementary security review — pass 12

**Date:** 2026-11-24
**Review head:** code `e0919364bd3409e43a81016fc25b4300f80f71d6`;
docs `af7d33986285903647877dd6cb15f71b7482f352`
**Method:** four independent read-only reviewers with separate scopes (ledger/
removal, dependency/CI/release, torrent engine/seeder, wallet/RPC/Tauri/BTC/
evidence). The configured model table was unavailable and the harness only
allows a model when explicitly requested, so all four used the default model
with independent contexts. No code or infrastructure was changed during the
review.

**Verdict: not release-ready.** The pass-10 removal guard is incomplete for
live journal debts, the seeder has an unauthenticated request amplification
path, the incentive selector can spend the main stake coin, and legacy import
can report success while leaving the desktop node unable to start.

## Act On

1. **Unauthenticated 1-byte/unaligned block requests amplify disk/syscall
   work.** `seed.rs:865-937` and `engine.rs:1251-1357` accept any 1..32 KiB
   length with no `begin` alignment. Each request performs layout/canonicalize/
   symlink/open/seek/read. `served_requests` only suppresses exact keys; the
   per-peer outstanding cap is dead; the rate limiter runs after the read.
   Enforce block geometry (`begin % 16_384 == 0`, exact length, final short
   block), reject before disk, remove/replace the dead counter, add a test.
2. **The removal guard is fail-open for live journal debts.** The daemon reads
   `incentive-ledger.json`, a startup/settlement snapshot; live receipt state is
   in `incentive-journal.jsonl` and the session accounts. The pass-11 ledger
   files were empty; all 184 live 400s were the session guard. The pass-9
   failure mode is not covered live. Also, `has_outstanding_incentives` ignores
   un-receipted `pending_owed`, and `SessionManager::add_session` eviction
   bypasses the guard. Share/consult the journal, include pending owed, route
   eviction through the guard, add a daemon-level test.
3. **The incentive selector spends the main stake coin.** `available_stakeable`
   requires current age eligibility; an immature 200 VTR coin is classified as
   non-stake and spent. The live payment did this 21 s after the stake while a
   0.5 VTR coin was sufficient. On mainnet this resets the only stake coin’s
   age for six hours. Reserve structural stakeability regardless of age, prefer
   truly non-stakeable coins, fail closed, add immature/single/multiple tests.
4. **Legacy `wallet.dat` import creates no BTC state, and the UI binds to a
   non-signing address.** `import_legacy_wallet` never calls
   `create_initial_btc_states`; `start_node` fails, and the UI swallows it.
   The UI uses the first imported address while `authorize_wallet` returns the
   generated default WIF, breaking DEX/swap/claim for imported wallets. Create
   the attested BTC state or fail clearly; return/persist the default address;
   add import→start and multi-key DEX/swap tests.
5. **The review package’s code-freeze/evidence checks are invalid.** The
   `git diff --name-only e091936..HEAD -- '*.rs' '*.toml' '*.ts' '*.tsx'` guard
   is blind to `release/Cargo.lock`, UI lock/config, Dockerfile, workflows, and
   toolchain files. The reviewer recipe tests a fresh graph instead of
   `release/Cargo.lock`, omits GTK/WebKit prerequisites, and the package has no
   saved canonical CI/audit output. The live “zero-fee” claim is false (the fee
   was 1,000 sats), and the live ledger backstop was never exercised. Fix the
   guard/recipe, archive the CI evidence, correct the claims.

## Consider

- **Desktop removal:** Tauri cancels the engine before the guard; a refused
  removal strands the session. The locked-wallet empty-address error is raw; a
  valid-but-wrong address is accepted though the seeder signs with the hot key.
  Add command-level and desktop add-torrent tests.
- **Dust asymmetry:** a positive sub-minimum receipt pins a desktop session,
  while the ledger treats sub-minimum unpaid debt as removable;
  `write_off_dust` has no production caller. Align thresholds/prune.
- **Guard I/O:** synchronous ledger read/parse under the sessions write lock
  blocks the executor; the error returns the absolute path. Use a shared
  in-memory guard and a generic error.
- **Guard configuration:** installed when payments are disabled; missing ledger
  is fail-open; no operator clear/write-off.
- **IPv4-mapped IPv6:** `::ffff:a.b.c.d` all share the `::` per-IP bucket.
- **`retain_sessions` race:** a session added between the announce snapshot and
  prune can be unregistered.
- **Watchdog:** only whole verified pieces count as progress; a slow peer
  filling a large piece can be stopped after 15 minutes.
- **Trusted resume bits:** the seeder serves resume-marked pieces without
  first-request hashing, contrary to the fail-closed docs.
- **Soak coverage:** one point-in-time sample per day; no `query_range`, gaps,
  down samples, or cron-failure alert; the first scheduled observation was
  missing.
- **Reproducibility:** local cached image; no second `--no-cache` build, lock
  SHA, clean-status record, or CI image build.
- **Five warnings:** only `bincode` is recorded accepted; `chrono`,
  `proc-macro-error`, `glib`, and `rand` lack a reachability table; CI does not
  deny warnings.
- **Signing workflow:** signs assets attached to any published release; no
  protected environment, tag ancestry, or same-run attestation.
- **Rollback drill:** the new-genesis/revert/reorg path is not exercised; the
  completed drill is only a binary swap.
- **Staking status:** below-minimum UTXOs are reported eligible with an
  expected reward.
- **Launch runbook:** still names `70ea7b9`; the exact image is `e091936`, and
  the desktop `Build` was skipped.
- **Manual spend paths:** manual send/DEX/swap/cold-stake still do not apply
  the stake reserve.
- **Two-peer evidence:** the pass-8 split remains the only live multi-peer
  proof; add a deterministic two-peer harness.

## Noted

- `cssparser-macros` moves `syn 3.0.9 → 2.0.119`; requirement `>=2,<4`, both
  versions remain, no build break; desktop-only.
- CI audit is 0.22.2 vs local 0.23.0; save the canonical output.
- Canonical push CI skips the desktop `Build` (tag-only).
- The `dd98176` readiness target and `70ea7b9` launch runbook are stale.

## Dismissed

- **“The live single-seeder proves a scheduler/seeder regression.”** Node2
  registered and served the full transfer; node1 failed in the add-path
  wallet/no-wallet check. The pass-8 split predates the pass-9 ownership fix.
  The missing deterministic two-peer test is an evidence gap, not a regression.
- **“The zero-fee payment is a code/policy bug.”** The live payment paid the
  1,000-sat floor; the claim is a docs/artifact error. Close the follow-up.
- **“The `syn` change breaks a dependency requirement.”** The requirement is
  `>=2,<4`; both versions remain; no build failure.
- **“The bincode advisory is a new blocker.”** It remains the accepted, pinned
  `=1.3.3` risk with compensating controls.

## Agreement Map

The strongest signals are the **stake-reserve age filter**, the **Tauri
cancel-before-guard**, and the **false zero-fee claim**, each raised
independently by A, C, and D. The four single-reviewer criticals are in distinct
domains — A’s live-ledger fail-open, B’s code-freeze blind spot, C’s upload
amplification, D’s legacy-import failure — each with a concrete path and
evidence. The same-model limitation means no cross-model consensus on those
criticals, but the independent scopes and evidence justify taking all four
seriously. Do not send the review package to the independent human reviewer
until the code-freeze/recipe/zero-fee/ledger-test corrections are made.
