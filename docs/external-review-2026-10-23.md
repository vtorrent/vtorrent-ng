# Independent external review — 2026-10-23

- Reviewer: external, source-based review (AI acting as an external reviewer).
- Candidate: `30896d6` (HEAD `ae396af`).
- Scope: the recent product features and the security-critical wallet, backup,
  send, swap, staking, activity, torrent, and settings paths.
- Method: source inspection, full workspace tests at `30896d6`, frontend tests,
  lint, build, and targeted Clippy. No dynamic exploit testing.
- Result: **2 High, 5 Medium, 8 Low**. No Critical. The candidate is materially
  stronger than `bab2456`, but the High and Medium findings should be resolved
  before mainnet.

## Findings

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | High | 2FA does not protect the encrypted wallet or backup at rest | Resolved by documenting the threat model and adding a backup warning; encryption remains passphrase-based |
| 2 | High | The reviewed send quote is not bound to the broadcast transaction | Fixed in `a7c4848` |
| 3 | Medium | Cold-stake delegate key is session-only; auto-resume uses the hot key | Fixed in `a7c4848` |
| 4 | Medium | Activity pending rows always report a zero fee and can duplicate confirmed rows | Fixed in `a7c4848` |
| 5 | Medium | Activity center does not implement reorged or failed states | Fixed in `1526456` |
| 6 | Medium | Torrent rate-limiter sleeps are not cancellation-aware | Fixed in `a7c4848` |
| 7 | Medium | Cold-stake spending-key safety is only a UI checkbox | Fixed in `a7c4848` |
| 8 | Low | RPC imported staking WIF is not zeroized | Fixed in `a7c4848` |
| 9 | Low | Backup writes are not symlink-safe and backup names can collide | Fixed in `a7c4848` |
| 10 | Low | 2FA confirmation persistence failure leaves inconsistent state | Fixed in `a7c4848` |
| 11 | Low | Layout reports “Fully synced” with zero peers | Fixed in `a7c4848` |
| 12 | Low | BTC peer setting is saved without validation | Fixed in `a7c4848` |
| 13 | Low | Seed-ratio target is global, but the UI describes it per session | Fixed in `a7c4848` |
| 14 | Low | Activity notification links can point outside the loaded window | Fixed in `a7c4848` |
| 15 | Low | Focus outline and icon labels are inconsistent | Fixed in `70ea7b9` |

## Remediation (2026-10-23)

`a7c4848` fixes the two High findings, the Medium findings 3, 4, 6, and 7, and
Low findings 8–14. `1526456` adds the persistent activity journal for
broadcast, confirmed, reorged, and failed states. `70ea7b9` completes the
icon-only control audit with accessible labels.

The 2FA finding is resolved as a threat-model and documentation issue: the
wallet and backup remain encrypted with the wallet passphrase, and the UI now
states that 2FA protects app unlock, not the backup. If 2FA-bound encryption is
required, that remains an open design change.

## Post-review delta (2026-10-24)

A live mainnet BTC SPV probe found a defect outside the original 15 findings:
a fresh mainnet filter-header chain requires agreement from two
compact-filter peers, but the sync loop connected one. The scan failed with
`compact-filter headers require agreement from 2 peers (have 1)`.

`1901bb9` fixes this by collecting up to two compact-filter peers (configured
peer plus DNS seeds, 5 s connect timeout), authenticating filter headers across
them, and separating authentication from the filter download so the multi-peer
scan does not re-authenticate with one peer. After the fix the probe synced
972,892 headers and scanned 100,001 BIP-158 blocks in 14m52s at
181,952–182,452 kB RSS with no errors. The full workspace suite at `1901bb9`
passed: 48 test groups, 1,100 passed, 0 failed, 7 ignored. See
`docs/btc-spv-soak-plan.md`.

`513efde` adds the follow-up hardening: filter-header agreement counts
network groups instead of socket addresses (including IPv4-mapped IPv6), the
configured peer is attempted first, and failed header or filter-download peers
are replaced or bypassed. The fresh probe on `513efde` synced 972,921 headers
and scanned 100,001 blocks at 182,072 kB RSS. The full workspace suite at
`513efde` passed 48 test groups, 1,105 tests, 0 failures, and 7 ignored tests.
`c465d1b` adds five-peer headroom and in-cycle scan retry; its probe synced
972,999 headers and scanned 100,001 blocks at 236,456 kB RSS, with the full
workspace suite passing 1,105 tests. `7ec3b2f` adds atomic BTC SPV health
metrics and a stale-scan alert, with `/metrics` verified during a live scan.
`2c381fa` persists compact-filter peers and tries them before DNS seeds.
The completed review candidate remains `70ea7b9`; an independent reviewer
should include the `1901bb9`, `513efde`, `c465d1b`, `7ec3b2f`, and `2c381fa`
deltas or review `2c381fa` directly.

## High findings

### 1. 2FA does not protect the encrypted wallet or backup at rest

`Wallet::load` decrypts the wallet file with the passphrase before checking the
OTP (`vtorrent-wallet/src/wallet.rs:203-213`). `decrypt_wallet` is public
(`vtorrent-wallet/src/encryption.rs:97`). The backup bundle is encrypted with
the same passphrase (`vtorrent-tauri/src/commands/backup.rs:303`) and contains
the encrypted wallet file (`backup.rs:139`).

An attacker who obtains the backup and the passphrase can decrypt the backup,
then decrypt the wallet file directly, and recover the keys and the OTP secret
without the OTP. 2FA is an app-level unlock gate, not an at-rest encryption
factor. If 2FA is expected to protect backups, this is a High finding.

**Fix:** derive the wallet and backup encryption key from the passphrase and the
OTP (or use a separate backup passphrase), or explicitly document that 2FA does
not protect the encrypted file or backup. Add a test that the backup cannot be
opened with the passphrase alone when 2FA is enabled.

### 2. The reviewed send quote is not bound to the broadcast transaction

`quote_vtr` builds a transaction and returns its fee and change
(`vtorrent-tauri/src/commands/wallet.rs:529-613`). `send_vtr` rebuilds a new
transaction (`wallet.rs:616-697`). The UI’s `handleSend` calls `sendVtr` with
only the address and amount (`vtorrent-ui/src/pages/DashboardPage.tsx:85-93`).

The final fee and change can differ from the reviewed quote if the UTXO set
changes. The user reviews one transaction and signs another. The UI note warns
that the fee can change, but the user cannot approve the actual signed
transaction.

**Fix:** prepare and store the signed transaction (or a quote ID) and broadcast
exactly that transaction, or re-quote and require confirmation if the fee
changed. Return the final fee with the txid.

## Medium findings

### 3. Cold-stake delegate key is session-only; auto-resume uses the hot key

`start_staking` accepts an imported staking WIF and marks the key imported
(`vtorrent-tauri/src/commands/staking.rs:32-72`), but `persist_staking_intent`
stores only the address (`staking.rs:71`). After a restart,
`resume_staking_if_requested` starts staking with `state.wallet_wif`
(`vtorrent-rpc/src/handlers/wallet.rs:676-683`).

A delegate node therefore resumes with the hot wallet key, which cannot sign
P2CS kernels for the imported staking address. The readiness card shows a
warning, but the node may silently fail to stake.

**Fix:** persist the imported staking key encrypted (or require re-import with a
clear session-only warning), and do not auto-resume a cold-stake intent with the
hot key.

### 4. Activity pending rows always report a zero fee and can duplicate confirmed rows

`wallet_activity` sets `fee_satoshis: 0` for pending transactions
(`vtorrent-node/src/activity.rs:80-89`) and merges confirmed and pending entries
without deduplicating by txid (`activity.rs:41-90`). A transaction that is still
in the mempool after confirmation can appear twice, and the pending details
drawer shows a zero fee.

**Fix:** compute the pending fee from input and output values, deduplicate by
txid preferring the confirmed row, and add tests for both cases.

### 5. Activity center does not implement reorged or failed states

`wallet_activity` only emits `"confirmed"` and `"pending"` (`activity.rs:47,82`).
A reorged or dropped transaction disappears without a record. The product
review promised pending, broadcast, confirmed, reorged, and failed states.

**Fix:** persist a local activity journal with broadcast/confirmed/reorged/failed
transitions, or explicitly scope the feature to pending/confirmed and update the
product claim.

### 6. Torrent rate-limiter sleeps are not cancellation-aware

`consume` calls `tokio::time::sleep` (`vtorrent-torrent/src/limits.rs:84`)
without the torrent cancellation token. The peer task awaits it
(`vtorrent-torrent/src/engine.rs:767`). Removing a torrent while a low rate is
set can leave the engine task sleeping for a long time.

**Fix:** pass the cancellation token into `consume` and `tokio::select!` on it,
or cap the sleep and loop.

### 7. Cold-stake spending-key safety is only a UI checkbox

`create_cold_stake` returns the spending WIF once
(`vtorrent-tauri/src/commands/wallet.rs:742-750`). `fund_cold_stake` checks only
the script registry (`wallet.rs:781-795`), not whether the spending key was
saved. A buggy or compromised frontend can fund a P2CS script without the
spending key being persisted, making the funds unspendable.

**Fix:** persist the spending key encrypted in the wallet, or require an explicit
backend confirmation token before funding.

## Low findings

### 8. RPC imported staking WIF is not zeroized

`StakingStartRequest.staking_wif` is a plain `Option<String>`
(`vtorrent-rpc/src/models.rs:266-271`). The handler clones it into `Zeroizing`
(`vtorrent-rpc/src/handlers/staking.rs:144-148`), but the request’s original
string is dropped without zeroization. Use `Option<Zeroizing<String>>`, as the
passphrase already does.

### 9. Backup writes are not symlink-safe and backup names can collide

`write_private` uses `create(true).truncate(true)`
(`vtorrent-tauri/src/commands/backup.rs:101-118`). The temp wallet path and the
backup filename are predictable (`backup.rs:313`). A same-user local attacker
can pre-create a symlink and cause an overwrite; two backups in the same second
overwrite each other. Use `create_new(true)` for temp files, add a random
suffix, and refuse an existing backup path.

### 10. 2FA confirmation persistence failure leaves inconsistent state

`confirm_pending_2fa` activates 2FA in memory and clears the pending secret
(`vtorrent-wallet/src/wallet.rs:475-491`). `verify_2fa` then calls
`persist_wallet` (`vtorrent-tauri/src/commands/wallet.rs:482-488`). If
persistence fails, the in-memory wallet has 2FA enabled but the file does not;
retrying verifies the active config and does not persist. Persist before
committing the in-memory activation, or roll back on failure.

### 11. Layout reports “Fully synced” with zero peers

`Layout.tsx:63,165-185` shows “Fully synced” whenever `syncing` is false, even
with `connections === 0`. The staking readiness card correctly says “Connect to
a peer.” Show “Waiting for peers” when `peerCount === 0`, and only “Synced” when
`!syncing && peerCount > 0`.

### 12. BTC peer setting is saved without validation

`set_btc_peer` trims and stores any string
(`vtorrent-tauri/src/commands/settings.rs:86-95`). The UI says “saved” and the
sync fails later. Validate `host:port` (or resolve it) before saving and return
a clear error.

### 13. Seed-ratio target is global, but the UI describes it per session

`AppState.seed_ratio_target` is global (`vtorrent-rpc/src/state.rs:53`), and
`engine.rs:718-736` stops uploads when the target is reached. The UI says “the
session reaches that ratio,” and the session state remains `Seeding`. Label the
setting as global and expose a “target reached” state.

### 14. Activity notification links can point outside the loaded window

`ActivityPage` only opens details if the txid is in the loaded activity list
(`vtorrent-ui/src/pages/ActivityPage.tsx:53-60`). A notification for a txid
older than the 200-row window shows no details. Fetch the transaction by id or
show a clear “not in recent activity” message.

### 15. Focus outline and icon labels are inconsistent

`index.css:51` uses a low-specificity `:where` focus outline, while `.btn-*`
uses `focus:outline-none`; some page icon buttons have only `title`. Use a
consistent `focus-visible` ring utility on icon buttons and add `aria-label`
across the app.

## Positive observations

- The prior frontier findings are fixed: BTC reorg metadata preservation,
  wallet-switch/BTC scoping, P2CS mempool policy, wallet-path confinement,
  registry durability, BTC scan persistence, BTC broadcast rollback/network,
  cold-stake authorization across wallets, WebSocket slot reservation, and WIF
  zeroization.
- The backup manifest checks (SHA-256, duplicate names, path traversal, size
  caps) are solid.
- The P2CS-aware staking snapshot and the imported staking-key flow are a
  meaningful improvement.
- The activity center, settings diagnostics, torrent controls, and onboarding
  are useful additions.

## Verification performed

- `cargo test --workspace --all-features` at `30896d6`: 48 test result groups,
  no failures.
- `pnpm test`: 41 frontend tests; lint and production build pass.
- Targeted Clippy and formatting pass for the changed crates.
- Source inspection of the recent features and the security-critical paths.

## Limitations

- This is a source-based external-review simulation, not a substitute for an
  independent human security firm.
- No dynamic exploit testing, no mainnet-scale BTC SPV soak, and no live Tauri
  desktop journey were performed in this pass.
- The prior live Tauri journey, two-node cold-stake test, and real BTC SPV swap
  are supporting evidence, not a replacement for an independent review.

## Top 5 actions

1. Resolve the 2FA-at-rest/backup threat model.
2. Bind the reviewed quote to the broadcast transaction.
3. Persist or explicitly session-scope the cold-stake delegate key, and guard
   the spending key in the backend.
4. Fix activity pending fees, deduplication, and reorged/failed states.
5. Make the torrent rate limiter cancellation-aware and add regression tests.
