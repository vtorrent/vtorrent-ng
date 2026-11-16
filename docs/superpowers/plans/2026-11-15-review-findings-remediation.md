# Security Review Findings Remediation Plan

**Goal:** Reduce the identified BTC recovery, attestor key, durability, and reviewer-handoff risks without overstating evidence or changing consensus.

**Approach:** Trace each failure boundary and add regression coverage before making targeted changes. Preserve legacy wallet bytes and refuse unsafe operation where history cannot establish safety; never claim missing historical data can be reconstructed.

## Constraints

- Preserve genesis, consensus, legacy data, and unrelated working-tree files; do not disturb the active soak.
- Independent human review, real power-loss evidence, and a 168-hour soak of the exact shipping image remain release gates.
- Do not push or publish without establishing the intended remote and candidate; do not represent a local remote-tracking ref as live remote state.

## Tasks

- [ ] Trace all legacy BTC load, migration, send, swap, rescan and persistence paths, including already-upgraded state. Design and test a durable fail-closed boundary without treating a scan or operator assertion as proof of missing send history.
- [ ] Reproduce multi-process attestor first-use contention; implement cross-process safe, durable key publication with private permissions and tests for contenders, preexisting files and failures.
- [ ] Trace BTC background persistence callers and inject failures at relevant boundaries; address unsafe behavior and document platform-specific evidence and restrictions. Do not claim that simulated failures prove power-loss behavior.
- [ ] Verify candidate availability on the live remote without altering it; update reviewer handoff and provenance only after changes settle.
- [ ] Run focused tests, formatting and relevant lint, then broader feasible verification. Report exact evidence and any remaining blockers; retarget review only to a committed candidate with accurate provenance.

## Investigation notes

- Existing schema-2 files may have been converted from older files without retaining their origin. A guard on the schema number alone cannot protect those files; do not claim that it resolves the legacy-history problem.
- Ruling: refuse all pre-v3 wallet-state files, including v2, while preserving bytes; fresh wallet state is v3. This blocks otherwise safe existing wallets and requires a separately reviewed recovery path. It does not detect deleted or manually altered state, prove historical ownership, or reconstruct missing transactions.
- The attestor regression tests reproduced a multi-process failure and reuse of an existing world-readable temporary file. Exclusive temporary files and no-clobber publication passed both targeted regressions; platform and crash guarantees still need review.
- The existing-key fast path formerly returned before a concurrent publisher synced the parent directory. On Unix it now syncs the directory before returning; an injected sync-failure test failed before the change and passes after it. This is not a real power-loss test, and the non-Unix durability guarantee remains unproven.
- Version-2 wallet files now fail to open in the working tree. Loading and saving one through the UTXO inspection API retains version 2 and does not authorize wallet opening. Missing state files also fail closed, including for genuinely new wallets, as explicitly authorized by the user. No approved initialization or legacy recovery flow exists yet.
- The daemon now has an explicit `--btc-attest-new-seed` startup assertion for genuinely new seeds. With it, missing state creates one v3 seed-scoped file; without it, missing state fails closed. The option does not weaken the pre-v3 or future-version guards. Desktop initialization intentionally remains blocked until its own explicit UX exists.
- Attested first creation now uses exclusive temporary creation and a no-clobber link. A stale `utxos.tmp` symlink previously redirected the write to its target and then made the wallet path that symlink; the new test failed before the fix and passes after it. The ordinary `UtxoSet::save` path still opens its fixed temporary path with truncate, so a planted symlink there remains a local-filesystem hazard for already-initialized wallets.
- `scan_utxos_bip158` and `scan_utxos_bip158_multi` now propagate batch-save failures rather than reporting success. The regression failed before the change and passes afterward; completion is recorded only after the save succeeds.
- A unit test simulates the scan's pending-send-to-undo transition and injects failures before rename and at parent-directory sync. Both retained the signed pending send and kept the input unselectable after reopening. This does not exercise a live peer scan, later writes, non-Unix, or real power loss; other background save paths still need review.
- `persist_scan_progress` previously advanced the in-memory cursor before saving and returned no error. It now restores the previous in-memory set and completion marker if saving fails, and `set_scan_progress` reports failure to the RPC loop, which counts the error and stops the pass rather than recording success. Tests cover a pre-rename failure and an injected post-rename directory-sync failure followed by a successful retry and reopen. Real power loss and end-to-end network retry remain untested.
- `finish_recovered_scan` previously cleared recovery state before a best-effort save. It now retains hidden/recovery state on a failed save and reports failure to the RPC loop. A pre-rename failure regression failed before the change and passes after it. Post-rename uncertainty and real restart/power-loss semantics still need testing.
- Daemon startup now writes a configured BTC birthday and cursor together and fails startup on save failure. A test verifies pre-rename failure preserves the prior values and a successful save reopens with both values; real power-loss behavior is unproven.
- The original full-suite run with the v3 guard failed `daemon_starts_after_archive_with_the_seed_scoped_file`, whose expected behavior contradicted the new policy. The test now asserts rejection plus exact preservation of both staged and archived bytes; all 15 staging tests passed.
- On 2026-11-15, `git ls-remote --heads origin main` reported `b51db24ad3a8583699e9ba0b118de7bf804af9cf`, which does not include the locally named review candidate. Do not send the current clone-and-checkout instructions as though the candidate were published.
