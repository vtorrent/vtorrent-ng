# AI supplementary security review — pass 14

**Date:** 2026-11-24
**Code candidate reviewed:** `80f9eef02ec81b848d212deb5073ef477e18d6dc`
**Remediation commit:** `a0724dd`
**New image:** `vtorrent/node:a0724dd`, ID `sha256:569c59327deb126387ebde1decd96bce2689418283862c2162c6eca26803e658`
**Extracted binary:** `d89ba0886eeab563b253c6a66d7512ca1e2d94688736a61816e95f2c08c26e3c`
**New window:** `2026-11-24T17:14:00Z` to `2026-12-01T17:14:00Z` (`review-a0724dd`)
**Validation:** full suite 57 groups, 1,553 passed, 0 failed, 13 ignored;
two clean builds produced the same image ID and binary; both E2Es pass on the
image binary (2.12 s seeder, 26.28 s incentive); canonical CI run
`45445768245` passed every job, with the tag-only desktop matrix skipped on the
push.
**Docs tip reviewed:** `05b125d` (plus later docs-only commits)
**Reviewers:** four independent read-only passes — consensus/wallet/BTC,
torrent/incentives/seeder, desktop/RPC/UI, and release/evidence.
**Verdict:** **not release-ready.** The pass-12 criticals are materially
addressed, but pass 13 introduced one reserve bypass and left several
important payout, debt, desktop, import, and evidence gaps.

The `review-80f9eef` window is **not release evidence** for a fixed candidate;
any code fix requires a new image, E2Es, and a restarted 168-hour window.

## Critical

| # | Area | Finding | Evidence | Fix |
|---|---|---|---|---|
| C1 | Manual send reserve | The structural set is taken from the full chain while the send candidate UTXOs exclude mempool spends. A mempool-spent structural outpoint is absent from `utxos`, so `build_with_reserve`'s “all structural outpoints selected” check is always false and the remaining stake coin can be spent. Same pattern in Tauri send. | `vtorrent-rpc/src/handlers/wallet.rs:393-428`; `vtorrent-tauri/src/commands/wallet.rs:700-724`; `vtorrent-wallet-service/src/lib.rs:133-169` | Intersect `structural_stakeable` with the actual candidate UTXOs inside `build_with_reserve`/`select_reserved_utxo`; add a mempool-spent-largest test. |
| C2 | Evidence | The live DELETE 400 was the **session** guard message, not the journal guard. `PROOF.md`/`soak-log.md` claim both. The journal guard code exists but was not exercised live. | `live-payment/delete-response.json`; `vtorrent-rpc/src/handlers/torrent.rs:289-291`; `vtorrent-daemon/src/main.rs:1317-1339` | Relabel the proof as the session guard; add a journal-only live/daemon test (or mark the journal backstop as unit-tested only). |

## Important

### Code / fund safety

| # | Area | Finding | Evidence | Fix |
|---|---|---|---|---|
| I1 | Quote send reserve | A quote built while `staking_address` is `None` (or for a different address) stores a signed tx; `send_vtr` consumes it without rechecking the current structural set. Starting staking before send can spend the only stake coin. | `vtorrent-tauri/src/commands/wallet.rs:714-735,834-872` | Store the reserved outpoints/staking address in `PreparedSend` and revalidate at send, or rebuild with the reserve; add a quote→start-staking→send test. |
| I2 | Torrent payout | `wallet/import` sets `wallet_change_address` with `wallet_wif = None`. `add_torrent` checks only the address; the engine then caches `receipt_key = None` forever. | `vtorrent-rpc/src/handlers/wallet.rs:263-267`; `vtorrent-rpc/src/handlers/torrent.rs:149-191`; `vtorrent-torrent/src/engine.rs:571-574` | Require `is_wallet_unlocked()` and a live WIF at add; snapshot address+WIF under one lock; test import→add (locked) and unlock→add. |
| I3 | Removal debt | `blocks_removal` counts un-receipted raw `bytes_downloaded`. Settlement is receipt-gated, so a peer that never sends a receipt, or a disputed receipt, can pin removal forever. Desktop has no settlement loop. At 10k accounts, `account_for`/`evict_stale_accounts` can evict the same debt because they ignore `blocks_removal`. | `vtorrent-torrent/src/incentive.rs:202-209`; `session.rs:185-284,295-298`; `vtorrent-tauri/src/commands/torrent.rs:302-308` | Use one time-bounded liability predicate in removal **and** eviction; add a bounded receipt grace and an explicit force-remove/write-off with audit. |
| I4 | Seeder credit | `blocks_removal` checks what we owe, not `calculate_earned`/`bytes_uploaded`. DELETE cancels the seeder connection without a final receipt flush; earned-but-unreceipted credit can be lost. | `incentive.rs:202-209`; `vtorrent-rpc/src/handlers/torrent.rs:319-329`; `seed.rs:664-674` | Send a final cumulative receipt before cancel, or block removal/write off unsettled earned credit above the threshold; test seeder removal. |
| I5 | Desktop staking | The staking page uses `keys[0]` (the generated primary) while the default after legacy import is the first imported key. `start_staking` signs with the default WIF, so the selected address is not owned by the signing key. | `vtorrent-ui/src/pages/StakingPage.tsx:62-74`; `vtorrent-tauri/src/commands/wallet.rs:442-448`; `vtorrent-tauri/src/state.rs:70-80`; `commands/staking.rs:49-90` | Default to `useWallet().defaultAddress`; derive/validate the WIF for the selected key; add import→staking test. |
| I6 | Pre-pass-13 import | Existing imported wallets have no BTC state. `start_node` fails; `unlock`/`createWallet` only `console.warn`, so the dashboard runs while the node is offline. Re-import is refused because the wallet path exists. | `vtorrent-tauri/src/commands/node.rs:265-299`; `vtorrent-btc/src/utxo.rs:1307-1315`; `vtorrent-ui/src/hooks/useWallet.tsx:327-364` | Surface start failures on open/unlock like import; add a one-time attested BTC-state repair/migration or a documented re-import path. |
| I7 | Legacy multi-mkey | The extractor stops at the first master key that decrypts any ckey; ckeys under other master keys are silently skipped although the parser supports up to 64. | `vtorrent-migrate/src/extractor.rs:142-203` | Try every decrypted master key for every ckey; fail or warn with an explicit undecrypted count; add a multi-mkey fixture. |
| I8 | Desktop lock | `lock_wallet` leaves `state.node` populated; `open_wallet` refuses while the node exists; there is no stop-node command. Unlock after Lock fails with a misleading passphrase error. | `commands/wallet.rs:305-311,342-357`; `vtorrent-tauri/src/main.rs` | Allow `open_wallet` to replace the wallet while the node runs (and resync/stop staking), or implement a real node stop; add lock→unlock test. |

### Evidence / release gates

| # | Finding | Fix |
|---|---|---|
| I9 | CI job logs were zero-byte; the Docker image ID/lock SHA were not archived. (Fetched during this review; now present.) | Reference `ci/job-*-.log` and the Docker image ID/lock SHA from the evidence README. |
| I10 | The 60-cycle fleet soak ran the host release binary `cb8be38b…`, not the image binary `37e9d585…`. | Rerun with the image binary (env override/replace target binary) or relabel the summary as a host-build soak; record the digest. |
| I11 | The pass-13 desktop matrix was never run; the documented `workflow_dispatch` from `main` would create/update a draft release tagged `main` because `tauri-action` uses `github.ref_name`. | Add a `release_tag` dispatch input/guard, or dispatch on a temporary tag; archive the four desktop jobs and artifacts. |
| I12 | Tag ancestry only requires the release tag to be an ancestor of `main`; it does not require descent from the reviewed candidate. `build-desktop` has `contents: write` outside the protected environment. | Require descent from the candidate and protect release creation/signing. |
| I13 | The evidence README still names the invalid 08:18Z window; the soak log still says the production soak was “not touched”; the heartbeat is seeded with the window start, not an observation; the live “before” UTXO snapshots were taken after the payment. | Update the README to 13:33Z; qualify the production line; write the heartbeat only on a successful observation; relabel or recapture the pre-send snapshots. |
| I14 | Stale/contradictory docs: `6a49727`/pass-9 windows “in progress,” rollback `DRILL.md` says multi-node reorg remains open, the cover note says no prerequisites beyond Rust, and the consensus-batch document says OP_RETURN is both a no-op and needs a fresh genesis. | Reconcile before sending the human-review package. |

## Minor

- `quote_vtr` signs without passphrase/TOTP; the prepared tx is not returned today, but the signing invariant is violated.
- Multi-key sends pass all WIFs while `from_address`/change is the default; DEX/swap/claim use only the default WIF.
- `preview_payment` ignores the reserve while claiming to match `send_vtr`.
- Staking start/stop ignores its passphrase/TOTP fields; stop has no auth.
- `vtr_claim_external` accepts a raw taker WIF on mainnet unlike `vtr_claim`.
- `add_torrent` does not enforce unlock expiry; `lock_wallet` retains the BTC seed.
- The reserve-less `build_incentive_payment` helper remains.
- Cold-stake funding accepts sub-minimum amounts and can spend the only hot stake coin.
- Sub-minimum staking buckets are computed but not exposed; the UI average uses `total/eligible_utxos`.
- A missing ledger is fail-open; the desktop BTC guard misses network-only state names.
- The reserve error is `InsufficientFunds` instead of `StakeReserved` when only the preserved coin is sufficient.
- Engine upload dedupe is unbounded/O(n); the first request per piece can force a full-piece read before the limiter.
- Regression tests do not exercise the real reserve helpers, the import command, the daemon journal guard, or `staking_snapshot`; the two-peer test does not assert peer bitfield selection.

## Confirmed strengths

- Structural, age-independent reserve is shared by automatic payments, RPC/Tauri sends, and swap funding; cold-stake is explicitly exempted.
- Canonical block geometry is enforced at both upload paths.
- Live journal + ledger removal guard, pending-debt predicate, post-guard recheck, and outside-lock guard.
- Seeder IPv4-mapped key, prune cutoff, partial-block watchdog, and resume-bit distrust.
- Legacy import creates BTC state before publishing and keeps the BTC seed on the generated key; wallet default rebinding.
- Real-bitcoind four-test suite and 60/60-cycle soak are green; the live 0.5 VTR reserve payment proof is valid.
- Code freeze `80f9eef..HEAD` is empty; CI run `45357625083` is green for the code child.

## Fix status (2026-11-24)

Applied and committed as `a0724dd`; image and E2Es are green and the new
window is running:

- C1: `build_with_reserve` and `select_reserved_utxo` now intersect the
  structural set with the candidate UTXOs; regression tests cover a
  mempool-spent structural outpoint, the single-reserve fail-closed case, and
  preserving the largest of multiple available stake coins.
- I1: Tauri `send_vtr` rechecks the current structural set against a quote's
  fixed inputs and rejects a quote that would spend the entire reserve.
- I2: RPC and Tauri torrent add require an unlocked hot wallet and a loaded
  signing WIF; RPC regression test added.
- I3/I4: un-receipted raw debt and earned upload credit now block removal only
  for a one-hour grace after the last activity; receipt-backed debt remains
  indefinite; eviction uses the same time-aware predicate. A force/write-off
  path is still recommended for disputed agreed debt.
- I5: desktop staking defaults to the backend default address and signs with
  the selected key's WIF; `Wallet::wif_for_address` test added.
- I7: legacy extraction tries every master key and refuses a partial
  extraction; multi-mkey and partial-import tests added.
- I8: `open_wallet` can reopen the same wallet while the node runs, resyncing
  the RPC wallet and stopping staking; switching to a different wallet still
  requires a stopped node.
- I6: unlock/create now surface `start_node` failures instead of logging them.
- Evidence: all CI job logs were fetched; the live proof is relabeled as the
  session guard; the evidence README now uses the 13:33Z window; the 60-cycle
  summary records the host binary digest; production recovery is archived.

Still pending before a new candidate:

- One-time attested BTC-state repair for pre-pass-13 imported wallets (the
  start failure is surfaced, but there is no in-app repair yet).
- A force-remove/write-off surface for disputed agreed debt.
- The desktop matrix on a tag and the workflow dispatch/tag-ancestry fixes.
- A fresh real-bitcoind four-test run and 60-cycle soak against `a0724dd`; the
  recorded swap evidence used `80f9eef` source (the pass-14 changes affect
  wallet reserve/selection, so this rerun is still advisable).
- The pass-14 report is the review record; the new window is not release
  evidence until it completes and is signed off.

## Suggested pass-14 order

1. Fix C1 (reserve intersection) and I1 (quote revalidation) with tests; this is the code blocker.
2. Fix I2/I8 payout and desktop lifecycle; add import→add and lock→unlock tests.
3. Decide and implement the I3/I4 debt policy (bounded grace + force-remove/write-off + final seeder receipt).
4. Fix I5/I6/I7 (staking default, pre-pass-13 startup, multi-mkey import).
5. Add daemon-level journal-guard, reserve, import, and staking-snapshot tests.
6. Correct the evidence/release claims (C2, I9–I14), rerun the exact-image soak, build the desktop matrix, rebuild the image, rerun E2Es, and start a new window.
