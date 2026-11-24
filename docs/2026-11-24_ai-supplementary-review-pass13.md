# AI supplementary security review — pass 13: pass-12 remediation

**Date:** 2026-11-24
**Code candidate:** `80f9eef02ec81b848d212deb5073ef477e18d6dc`
**Docs head:** the commit that publishes this report (pass-13 review package)
**Image:** `vtorrent/node:80f9eef`, ID
`sha256:637002a7da23217d7cd276b2b5dfaf74b9e16280abb122a5707297f7ff56dd84`
**Extracted binary:** `37e9d585ca450968d585aa6c48f7fec2fa0bde918bce582cdc9074429cd56f90`
**Window:** `2026-11-24T13:33:00Z` to `2026-12-01T13:33:00Z` (`review-80f9eef`).
The original `08:18:28Z` clock was interrupted by a host Docker snap
auto-refresh at `13:18:37Z`; the fleet was restarted, staking re-enabled, and
the 168-hour clock restarted. See
`/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/WINDOW-RESTART.md`.

**Scope.** This pass fixes the four pass-12 criticals and the concrete
follow-ups: the automatic and manual stake reserve, the live-journal removal
guard and pending-debt eviction, legacy-import BTC state/default-key binding,
the seeder IPv4-mapped key and prune race, the partial-piece watchdog,
resume-bit distrust, sub-minimum staking status, and the review package's
code-freeze/evidence/release gates. The independent human review and the
168-hour exact-image soak remain release gates; this AI pass is supplementary.

## Fixed

| Finding | Fix | Commit |
|---|---|---|
| Incentive selector spends the only/immature stake coin | Structural `is_stakeable_with` reserve, prefer non-stake coins, preserve the largest stake outpoint, fail closed on the last one; applied to automatic payments, RPC/Tauri sends, and swap HTLC funding; cold-stake funding is an explicit restake exemption | `3891ff5`, `123673d`, `80f9eef` |
| Pending debt / journal not consulted by removal; eviction bypass | Pending owed is a removal liability; the live journal is consulted; eviction respects outstanding incentives; the guard runs outside the session lock with a post-guard debt re-check | `83f1b08`, `f772993`, `0960fa7` |
| Payout address mismatch / Tauri cancels before the guard | Payout address bound to the unlocked hot wallet; removal guard runs before cancel; blocking guard | `f14bcf0`, `b96091c` |
| Legacy import creates no BTC state and binds the UI to a non-signing key | Create attested BTC state before publishing the wallet; default address is the first imported legacy address; BTC seed stays on the first non-imported key; start failure surfaced | `4402f7a` |
| Seeder/network hardening | IPv4-mapped keys, prune cutoff, partial-piece watchdog, resume bits untrusted, sub-minimum staking status | `0af5546` |
| Two-peer evidence | Deterministic disjoint-bitfield two-peer split with real assembler and SHA-1 | `001c05e` |
| Review package / release gates | Whole-tree code-freeze pathspec, tracked-lock recipe and desktop prerequisites, five-warning reachability table, 1,000-sat fee correction, range/down-sample coverage + watchdog + alert hook, CI image build, signing environment/tag ancestry, rollback-drill status, launch-runbook refresh | docs commit |

## Verification

- Workspace suite: **57 groups, 1,544 passed, 0 failed, 13 ignored**
  (`cargo test --workspace --all-features`, release lock).
- `cargo fmt --all -- --check`, Clippy `-D warnings`, frontend lint/tests/build:
  **pass** (Clippy on the changed crates with `--all-targets --all-features`;
  frontend 19 files / 45 tests and the production build).
- `cargo audit --file release/Cargo.lock`: 0 vulnerabilities, 5 allowed
  warnings; reachability table in the review request.
- Two clean `--no-cache` builds produced the **identical daemon binary**
  `37e9d585…`. Their Docker image IDs differ only in build metadata
  (`sha256:637002a7…` deployed, `sha256:da36f55f…` repro).
- E2Es on the extracted image binary: seeder **2.15 s**, incentive **26.29 s**,
  paying 585,937 sats once; both pass.
- Real-bitcoind BTC SPV/HTLC suite on a disposable regtest container (release
  build, `VTORRENT_TEST_BTC_CONTAINER` selected explicitly): all four tests
  pass — the multi-node HTLC swap settled (VTR funding `2e03ceff…`, BTC funding
  `985261bf…`, BTC claim `660d9421…`, VTR claim `c4adc742…`), a reorged-out
  funding output was dropped and the balance returned to zero, two independent
  daemons agreed on a tip and reorg with the funding re-included, and an
  uncovered reorg across restart kept a confirmed send held.
- Live fleet `review-80f9eef`: three nodes at height 3 with agreeing tips,
  staking on nodes 2/3, restarts 0/0/0, Prometheus 3/3 at the +0h observation.
  At +1h they were at height 55 with the same tip, RSS 154/169/169 MiB, and
  zero down samples. The **first scheduled cron observation** ran at 11:25Z
  (+3h): heights 184/184/184, 111/114 range samples, zero down samples, and the
  watchdog heartbeat updated — closing the prior missing-first-observation gap.
  At `13:18:37Z` a host **Docker snap auto-refresh** restarted the daemon. The
  candidate containers (no restart policy) exited; they were restarted at
  `13:31Z` at height 295 with the same tip, wallets unlocked, staking resumed,
  and Prometheus 3/3. The 168-hour clock was restarted at `13:33Z`. The
  production `vtr-node*` containers restarted under their own policy and
  replayed the store; they were healthy again at 13:58Z (height 102815, tips
  agree, connections 2/1/1). They were not modified by any drill command, but
  the restart broke their continuous uptime and is recorded for their own
  sign-off.
  A live 12 MiB transfer then paid **585,937 sats** from node3's 0.5 VTR
  non-stakeable coin (`4e268baa…:0`), not the 200 VTR stake output
  (`eebf9d9ed5…:1`, still unspent), fee 1,000 sats. Before finality DELETE
  returned 400 from the journal/session liability; at 6 confirmations the
  journal showed `unpaid=0` and DELETE returned 200. See
  `/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/live-payment/PROOF.md`.
- Canonical CI run **45357625083** (`cd0e975`, docs-only child of `80f9eef`):
  **all jobs success** — Rust tests (all-features + default), format/Clippy,
  audit, Machete, benchmark gate, frontend, and the new no-cache Docker image
  job. The tag-only desktop matrix was correctly skipped on a push. The local
  suite, audit, image builds, both E2Es, and the live payment/removal proof are
  green.

## Remaining gates

- Independent human security review: not yet commissioned.
- Exact-image 168-hour window: **in progress** from the restarted clock
  `2026-11-24T13:33:00Z` to `2026-12-01T13:33:00Z`; not release evidence until
  it completes and is signed off. The earlier 08:18 clock is invalid due to the
  Docker snap refresh.
- Real legacy `wallet.dat` differential and multi-key DEX/swap/claim E2E:
  open.
- Active swap/HTLC validation: the real-bitcoind swap and reorg suite is green;
  a mainnet-scale adversarial swap soak remains open.
- Production signing key/certificate: not generated; authorization required.
- Height-10,000 checkpoint publication: after launch.
- Release-artifact rollback drill: **incompatible output proven.** Beta.3
  accepted a P2CS output and P2CS coinstake at height 10; a fresh candidate
  data dir produced genesis + height 1. A data-carrier OP_RETURN at height 11
  was rejected by beta.3 with a UTXO-root mismatch and the store truncated to
  height 10. Governance/state rent are not in the launch batch. The remaining
  operational item is a multi-node fleet reorg during rollback. See
  `/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/rollback-drill/DRILL.md`.

## Evidence

- `/mnt/temp/vtorrent-ng-evidence/soak-80f9eef/` — deployment, window,
  observations, E2E logs, workspace log, canonical CI/audit logs, image IDs,
  binary digest, and the review artifact/patch.
- `docs/2026-11-24_ai-supplementary-review-pass12.md` — the findings this pass
  remediates.
