# AI supplementary security review — 2026-11-17

> **This is an AI-generated supplementary review. It does not satisfy the
> independent human security-review gate.** No code was shared with an external
> party. Treat it as an additional defect-finding pass, not as sign-off, and do
> not cite it as independent review.

**Candidate:** `611d56b` (`611d56bba6bc982b5c2b6166897261da4fc8b935`), plus the
fix commits below. Documentation-only commits follow it on `main`.
**Method:** five parallel read-only review passes, one per risk area. Every
finding below was re-derived against the source by the reviewer; claimed issues
that could not be reproduced from code are excluded. Fixes were written
test-first, and each regression test was observed to fail before the fix.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| Critical | Desktop `match_dex_order` ignored its passphrase/OTP arguments, read the unlocked wallet WIF, and funded an HTLC to a caller-supplied taker address. `place_dex_order` accepted any `maker_address`, so an order could be created for the victim wallet's address; matching it then spent the victim's VTR. | Fixed in `a202519`; regression test covers wrong passphrase, missing 2FA, and a positive-path order lookup. |
| High | Desktop `vtr_claim` always injected the wallet WIF, so `vtr_claim_with_state` skipped passphrase/TOTP verification. | Fixed in `a202519`; the HTTP route also clears a caller-supplied `taker_wif`. |
| High | Desktop `send_btc`, `rebroadcast_btc`, `btc_fund`, `btc_claim`, and `swap_refund` authorized through the RPC helper, which parses the desktop seed wallet as a hot-wallet payload and fails. BTC spends and swap refunds could not succeed. | Fixed in `a202519`: desktop commands authorize against their own wallet and call already-authorized shared functions. |
| Medium | A crash between creating the TOTP replay file and writing it left an empty file that made every later verification fail permanently. | Fixed in `713fcc5`; empty state is treated as uninitialized, legacy per-wallet state included. |
| Medium-high | Desktop 2FA enrollment (`enable_2fa`, `verify_2fa`) accepted a TOTP secret without the wallet passphrase, allowing a compromised webview to enroll its own secret and lock the owner out. | Fixed in `aace0e5`; both commands require the passphrase, and the UI collects it before enrollment. |
| High | BTC sends with sub-dust change or a sub-dust recipient produced a non-standard transaction that peers never relay, while the inputs were already marked pending. | Fixed in `aace0e5`; sub-546-sat change is absorbed into the fee and dust recipients are rejected. |

Verification for the fixed items: `cargo test -p vtorrent-tauri --all-features`
(31 passed), `cargo test -p vtorrent-rpc --all-features` (204 passed),
`cargo test -p vtorrent-wallet otp::tests` (11 passed), and Clippy `-D warnings`
on both crates. The full workspace suite on `aace0e5` then passed **1,383 tests,
0 failures, 11 ignored**.

## Open findings (verified, not yet fixed)

### Wallet authentication and TOTP

1. **High — legacy hot wallet silently loses 2FA after restart.**
   `vtorrent-rpc/src/handlers/mod.rs` (`verify_wallet_auth`) accepts a bare-WIF
   payload and takes the TOTP secret only from `wallet_totp_secret`, which is
   process memory. Daemon startup (`vtorrent-daemon/src/main.rs`) restores
   `wallet_encrypted` but never that secret. A pre-`d128ed7` wallet therefore
   accepts passphrase-only unlock after a restart. Fail closed on the legacy
   format with an instruction to re-import the WIF and TOTP secret, or migrate
   atomically after a fresh code. This is a product decision.
2. **Medium-high — 2FA enrollment is unauthenticated on desktop.** Fixed in
   `aace0e5`; see the fixed table.
3. **Medium — TOTP replay is per environment, not per host.**
   The store path depends on `VTORRENT_TOTP_STATE_DIR`/`XDG_STATE_HOME`/`HOME`,
   and the key is only the secret hash. A daemon under systemd and a desktop
   under the user session can accept the same ±1-window code. Derive the path
   from the uid and bind the record to the wallet identity.
4. **Medium — verify/persist race consumes a step then fails.**
   `verify_wallet_auth` decrypts before taking `wallet_totp_last_step`; two
   concurrent requests can each consume a different step, and the loser fails
   the unchanged-blob check after consuming its step. Serialize the whole
   decrypt→consume→swap→persist sequence.
5. **Low/medium — a stored future step locks out 2FA after a clock error.**
   A code accepted while the clock is ahead persists a high step; after the
   clock is corrected every current code is rejected. Ignore implausibly future
   stored steps and document re-enrollment.
6. **Medium — wallet save is not atomic on Windows and uses a deterministic
   temp name on Unix.** `vtorrent-wallet/src/wallet.rs` (`save`): the non-Unix
   branch lacks `create_new`, `0600`, and `sync_all` before rename; the temp
   name is `<path>.tmp.<pid>`, so a stale temp from one failed save blocks later
   saves in that process. Use a random suffix, create-new, sync, and clean up.

### Desktop recovery and lifecycle

7. **Low/medium — restore can be overwritten by a concurrent wallet save.**
   `restore_backup` renames files before taking `state.wallet`; a concurrent
   `generate_address`/`verify_2fa` holding the old wallet can re-save it to
   `wallet.vtr` after the restore reports success. Hold the wallet lock across
   the rename/clear phase.
8. **Low — unbounded IPC inputs.** `import_legacy_wallet` base64-decodes and
   `torrent.rs` accepts a source string with no size cap (contrast
   `backup.rs`). Add explicit caps.
9. **Low — cold-stake export path is caller-chosen** and permissions are
   Unix-only; `set_torrent_settings` creates an arbitrary IPC-supplied
   directory. Confine both.

### Cold staking

10. **Medium — script registration is not an ownership check.**
    `create_cold_stake` is unauthenticated (desktop and RPC), and funding trusts
    `durable || in_memory || in_wallet`. A script generated by any IPC/API
    caller is "wallet-generated". Funding still requires the owner's
    passphrase, so this is not direct theft, but the error text overstates the
    guarantee. Authenticate creation and store the spending-key commitment in
    the encrypted wallet.
11. **Medium — the registry is keyed by wallet path, not wallet identity.**
    A recreated wallet at the same path inherits stale script hashes; funding
    one produces coins the new wallet cannot spend. Bind the registry to a
    wallet fingerprint and reject mismatches.
12. **Medium — `create_cold_stake` returns the staking WIF over IPC.**
    This contradicts the documented "frontend never receives key material"
    invariant. R1 and the mempool P2CS rule prevent reward redirect, so the
    direct impact is bounded, but the key belongs in Rust and should be handed
    to the staking engine internally.
13. **Low — locktime bounds are missing in constructors.** `build_p2cs` and
    `generate_cold_stake_keys` accept `u32`; values above `0x7FFF_FFFF` are
    rejected by CLTV but accepted by the endpoints' `2_100_000_000` clamp,
    producing an unspendable output. Validate in the constructors and lower the
    endpoint limit.
14. **Low — attestation fields are not cross-validated.** `check_policy` does
    not check `tip_height >= block_height`, the confirmation arithmetic, or a
    TTL bound. A compromised or buggy attestor can sign inconsistent fields or
    an unbounded expiry.
15. **Low — cold-spend authorization ignores chain time.** Only self-keyed P2CS
    scripts (`staking_hash == spending_hash`) are affected; require the ELSE
    selector or repeat the CLTV chain/type checks.

### Swaps, DEX, and RPC

16. **High trust gap — DEX order gossip is unauthenticated.** Announcements
    carry no signature, and `order_id` is not recomputed from content. A peer can
    inject or replace an announced order. No on-chain theft path was found
    (BTC funding requires local swap state), but the UI presents forged offers
    as genuine. Sign the canonical announcement and recompute the id.
17. **Medium — BTC funding does not require `target_asset == "BTC"`.**
    `btc_fund`, `btc_claim`, and the bump path can build a BTC leg for a
    non-BTC order; the reconciliation path then refuses to observe it.
18. **Medium — maker BTC addresses are not network-checked at order creation.**
    A testnet address on mainnet is accepted until `btc_fund`, stranding the VTR
    leg until expiry. Call `require_network` when the order is created and
    matched.
19. **Medium — the taker's VTR claim has no approved fee-bump path**, unlike the
    BTC claim and VTR refund. A stuck claim can let the maker refund after
    expiry while the taker has already paid BTC.
20. **Low — recovery buffers are not zeroized** (`btc_claim_raw`,
    replacement/funding/refund raw transactions contain the preimage), and one
    unreadable recovery record aborts unlock instead of being quarantined.
21. **Low — `Claimed` is set when both claim txids are recorded, not when
    settled.** Risk correctly uses six-confirmation observations, but the status
    string is easy for UI code to misread.

### BTC wallet recovery

22. **High — a full reset leaves `spent_reserved` set, permanently hiding an
    outpoint an uncovered reorg made unspent again.**
    `vtorrent-btc/src/utxo.rs` (`clear_chain_utxos`, `clear_journal`,
    `note_spend`, `add`) and `wallet.rs` (`reset_chain_cache_from`). A
    swap-reserved input observed spent is removed from `reserved_utxos` and
    recorded in `spent_reserved`. An uncovered reorg clears chain UTXOs and the
    journal but not `spent_reserved`; the rescan re-derives the output, `add`
    refuses it, and release paths require a `reserved_utxos` record. The input
    is neither spendable nor releasable. The current behavior is deliberate:
    `a_full_reset_discards_reservation_observations` asserts that a reset does
    not prove the observed spend was reorged out. Fixing the stranded case
    therefore needs a contract-aware change, not a blanket clear: let an active
    contract re-reserve the output after a rescan, and make release work without
    a `reserved_utxos` record. A blanket clear trades the permanent hide for a
    possible double-spend window and should not be applied casually.
23. **High — sub-dust change or a sub-dust recipient creates a permanently
    pending transaction and locks the inputs.** Fixed in `aace0e5` for new
    builds; a transaction already stuck this way still needs the cancel path
    described above, which remains open.
24. **Medium/high — a dead swap-funding transaction can never release its
    reservation, while an old live one can lose its contract.**
    `vtorrent-wallet-service` reserves before broadcast and retains on error;
    `btc_reconciliation.rs` releases only after a confirmed spend; verification
    is bounded to the last 1,008 blocks. A funding that never confirms has no
    path to release; a funding older than the window is reported unfunded while
    its spend is seen, deleting a live contract. Add an operator release for a
    funding proven absent from chain and mempool, and refuse release outside the
    verified window.
25. **Medium — every restart forces a full reset, so a wallet older than the
    header retention is blocked after restart.** Persist a trusted tip/lineage
    or size retention to `tip - birthday`; otherwise recovery needs a manual
    retention increase and a genesis header resync.
26. **Medium/low — a lower-height journal entry mutates the UTXO set and is then
    dropped.** `utxo.rs` (`push_undo`) after a cursor rewind without clearing
    the journal. Reject and roll back, or clear the journal when the cursor is
    rewound. No production caller does this today.
27. **Low — `restore_utxos` can resurrect a confirmed-spent outpoint** and cancel
    a journal removal; require proof of non-inclusion. No production caller
    today.

## Areas that looked sound

- Wallet AEAD/KDF: Argon2id with a fresh salt and nonce per save, ChaCha20-
  Poly1305 integrity, no nonce reuse.
- Desktop path confinement and backup entry validation; BTC/swap/cold-stake
  restore is fail-closed and no longer overwrites newer recovery records.
- Swap settlement observation: six confirmations with anchor invalidation; BTC
  blocks checked against the validated header chain; BTC funding refused until
  the VTR leg is confirmed.
- HTLC construction, claim/refund key and CLTV binding, fee/dust checks, and
  replacement lineage.
- RPC auth placement, hashed key comparison, non-loopback bind refusal, and
  single-use WebSocket tokens.
- P2CS script/key binding and the R1 cold-stake rule.
- `bincode` is pinned to `=1.3.3`; remote decode is capped at 4 MiB and the
  codec rejects oversized payloads. Mainnet legacy BTC promotion is
  hard-disabled.

## Verdict

The desktop authorization defects in the fixed section were release blockers
and are now closed in the working tree. The remaining items are real but do not
all need to block a review request. They do need the independent human review
the gate requires, and the exact-image soak afterwards. This document is
supplementary evidence only.
