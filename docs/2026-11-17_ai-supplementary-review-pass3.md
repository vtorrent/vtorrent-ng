# AI supplementary security review — third pass, 2026-11-17

> **Correction and follow-up:** The multisig loop advances its key index
> monotonically, so the earlier claim of m×n signature verifications is
> incorrect. The legacy vTorrent client source (checked locally, not part of
> this repository) rejects more than 20 keys in `OP_CHECKMULTISIG`, so the cap
> matches legacy consensus; boundary tests cover 20 keys accepted and 21
> rejected. Native BDB
> parsing now reconstructs overflow chains and is differentially tested against
> `db5.3_dump` on committed fixtures, but representative real wallets are still
> required. TOTP replay state is now replaced atomically. A block-level sigop
> limit of 20,000 now matches the legacy client.

> **AI-generated supplementary review. It does not satisfy the independent
> human security-review gate.** No code was shared externally. This pass covers
> areas the first two did not: untrusted legacy/snapshot parsers, wallet
> internals and signing, core primitives/script, the frontend IPC boundary, and
> operations/release safety.

**Reviewed tree:** `b9b05e3`, plus the fixes below (`8f7fe14`, `49d56d2`,
`aacf371`, `20e3fe6`, `089e7c1`, `9f3d4e9`, and `f0013bb`).
**Method:** five parallel AI-assisted read-only passes. Findings and fixes have
varying levels of verification; do not treat the report as independent review.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| Assessed | `OP_CHECKMULTISIG` lacked a 20-key cap. | `49d56d2` adds the cap. The local legacy client source (not in this repo) rejects more than 20 keys, so the rule matches legacy consensus. Tests cover 20 and 21 keys. |
| Medium | Wallet `save()` used a deterministic temp name and leaked the temp on write/sync errors, so one ENOSPC/EIO bricked every later save in the process. | Fixed in `49d56d2`: unique temp name, `create_new`, fsync, cleanup on every error arm. |
| Medium | `ColdStakeKeys` derived `Debug`, printing both staking and spending WIFs; `Zeroizing<String>`'s `Debug` prints the inner value. | Fixed in `49d56d2`: manual redacted `Debug`; test asserts neither WIF appears. |
| Medium | The desktop refund-bump form sent no passphrase/OTP, so the newly required authorization always failed. | Fixed in `49d56d2`: the form passes the existing wallet passphrase and 2FA code; the hook forwards both. |
| High | Release workflow pinned the signing-key-bearing `tauri-action` to a mutable tag and passed signing secrets to it, while releases are actually unsigned. | Fixed in `8f7fe14`: every action is SHA-pinned; top-level `permissions: contents: read`; the release job keeps its `contents: write` override. Signing env vars were removed because no signing identity/updater is configured, and the release text now states the builds are unsigned. |
| High | Release build did not enforce the audited lockfile. | Fixed in `8f7fe14`: `tauri build` receives `--locked`. |
| High | Seed installer copied `/tmp/vtorrent-daemon` over a root path with no verification. | Fixed in `8f7fe14`: the installer requires `VTR_DAEMON_SHA256`, verifies a private staged copy, and refuses a mismatch. The RPC key and its directory are owned by the service account with `0600`/`0700`. `deploy/test-provision-seed.sh` runs the installer in a container and checks readability, unit content, staging cleanup, and digest rejection; CI runs it. |
| Medium | The ntfy topic was committed and treated as the secret. | Fixed in `8f7fe14`: the relay reads `VTR_NTFY_TOPIC` and fails closed; the README no longer prints a topic. **The old topic must still be rotated** because it is in git history. |
| High | The native BDB parser missed records in a small real B-tree. | Fixed for btree leaf and overflow pages: the header offsets are corrected and `BOVERFLOW` chains are reconstructed. Committed fixtures cover small, overflow (5000-byte value), and multi-leaf databases; a differential test compares native output with `db5.3_dump` when the tool is available. Representative real wallets are still required. |
| High | The `db5.3_dump` fallback paired arbitrary lines, so an empty value shifted every later record. | Fixed in `aacf371`: parse `HEADER=END`→`DATA=END` sections, keep empty values aligned, reject unmatched keys/invalid hex. |
| High | Dump results replaced native results unconditionally, and non-zero dumps were accepted if stdout looked plausible. | `aacf371` requires a zero exit and complete `HEADER=END`/`DATA=END` sections. A failed dump with no native records is now an error. Count comparison remains a heuristic, not proof of completeness. |
| High | Native parsing could amplify memory by repeating offsets. | Duplicate/out-of-range offsets are rejected, decoded bytes are capped by file size, and overflow chains detect cycles, missing pages, and truncation. |
| High | TOTP replay state was truncated in place before the new step was written. | Follow-up: a separate lock file plus temp-file-and-rename replacement. Existing empty state fails closed, and tests assert the stored step and temp cleanup. |
| Medium | A verified torrent piece was published before the disk write completed. | Follow-up: an in-progress claim is separate from `have`; a failed or cancelled write releases the claim. Tests cover the failure path and claim lifecycle. |
| Medium | Seed provisioning had no end-to-end check. | `deploy/test-provision-seed.sh` runs the real installer in `ubuntu:22.04` with stubbed service management, then verifies the service account can read the key, the unit references it, staging is removed, and a bad digest changes nothing. CI runs it. |
| High | Blocks had no aggregate sigop limit, so a block could pack many multisig/P2SH checks. | Added `MAX_BLOCK_SIGOPS = 20_000` (`MAX_BLOCK_SIZE / 50`, matching the legacy client). Block validation counts legacy sigops; the transaction application path adds P2SH redeem-script sigops from the live UTXO set before executing each script, which also covers outputs created earlier in the same block. Mempool admission and template selection enforce the same bound. Tests cover exactly 20,000 accepted, 20,001 rejected, and a same-block P2SH spend that crosses the limit. |
| Medium | The snapshot reader clamped entry counts, ignored trailing bytes, skipped hash verification on load, and did not compare metadata totals with entries. | Parse now rejects entry-count overflow, trailing bytes, duplicate addresses, and hash mismatches; it verifies `total_addresses` and `total_supply` against the parsed entries before sorting. Tests cover each tamper case. |
| Medium | `btc-state-stage` accepted false confirmation flags, derived network provenance only from the filename, and did not confine the archive path. | All three flags must be true; the archive must be inside `--data-dir` and differ from source/output. A 0600 provenance sidecar binds label, wallet id, and SHA-256 at stage time and is required for archive and promotion. |
| High | The cold-stake spending key was absent from the desktop backup. | Encrypted backups now include `<wallet>.<address>.spending-key` exports as regular files (symlinks rejected). Restore deliberately refuses them with the same reviewed-recovery rule as the cold-stake registry, so the key stays in the backup for manual recovery. |
| Medium | Wallet format versions and unknown fields were ignored, allowing a future field to be silently dropped and rewritten as v1. | `WalletFile` and `WalletData` reject unknown fields; loading rejects any format version other than 1 with `UnsupportedWalletVersion`. Tests cover both. |
| Medium | Loopback RPC was unauthenticated by default and release CORS allowed a localhost web origin. | Mainnet now refuses to start without `--rpc-api-key-file` (or the env key); regtest/testnet keep local development. Release CORS allows only Tauri origins; the Vite origin is debug-only. |
| Low/medium | CSP allowed any loopback port, the theme bootstrap was inline, and browser mode loaded Google Fonts. | CSP now names `127.0.0.1:22525` (HTTP/WS) and Tauri IPC, adds `object-src`/`base-uri`/`frame-ancestors`/`form-action` `'none'`, and `font-src 'self'`. The theme bootstrap moved to `src/themeInit.ts`; Google Fonts links were removed. |
| Medium | Parser coverage was example-based with no malformed corpus; no procedure compared a real wallet with `db5.3_dump`. | Added a deterministic mutation test (300 mutated fixtures, no panics) and `scripts/wallet-import-check.sh`, which runs the tool with and without `db5.3_dump` and checks one extracted key per `key`/`ckey` record. Procedure: `docs/wallet-import-verification.md`. Real wallet files are still required. |
| Low/medium | Batch: AEAD errors were reported as a wrong passphrase; fee estimates undercounted bincode overhead; a lone key could sign a foreign UTXO; the migration UI claimed 2FA was verified; the TOTP seed stayed in state/clipboard; `classify_script` missed the swap HTLC shape; `push_int` mis-encoded values above 16; the backup cron and benchmark gate could silently do nothing; the dead-man path was inconsistent; the mkey cap was eight; the dump fallback trusted a re-openable path; varint decoding could wrap. | Addressed in the latest follow-up commit: clearer decryption error; worst-case serialized fee estimate plus a coverage test; fallback removed; 2FA warning; TOTP state cleared and clipboard timed; both HTLC prefixes; minimal `push_int`; backup script quiesces, verifies, and limits permissions; benchmark gate fails closed; sbin path unified; mkey cap raised to 64; post-dump byte comparison; overflow-safe varint. |
| Medium | The desktop legacy import decoded base64 with no size cap. | Fixed in `aacf371`: payloads above 32 MiB are rejected before decoding. |

Verified in the follow-up: 29 migration tests (including a real BDB fixture
that is parsed natively and through `db5.3_dump`, plus mutation coverage),
95 script tests, 245 node tests, 72 wallet tests, 112 torrent tests, 17
snapshot tests, Clippy on the changed crates, the UI test/lint/build, and
`deploy/test-provision-seed.sh` in `ubuntu:22.04`. The full workspace suite on
`49d56d2` passed **1,388 tests, 0 failures, 11 ignored**; the follow-up suite
passed **1,425 tests, 0 failures, 11 ignored**.

## Findings and status

### Legacy migration and snapshot parsing

1. **Evidence gap, not a code defect — representative `wallet.dat` recovery is
   still unproven.** The committed fixtures are synthetic. Run
   `scripts/wallet-import-check.sh` on real wallets and retain the `PASS`
   output before advertising recovery; see `docs/wallet-import-verification.md`.
2. **Medium — dump completeness is still inferred from record counts.** A dump
   that returns the same number of records but different contents would be
   accepted. Compare key material, not only counts.
3. **Fixed — malformed-input coverage.** A deterministic mutation test mutates
   each fixture 100 times and asserts the parser does not panic; the crafted
   overflow, deleted-entry, and page-type tests cover the structural cases.
4. **Fixed — snapshot truncation and tampering.** See the fixed table.
5. **Fixed — `btc-state-stage` confirmation flags and provenance.** See the
   fixed table. Filename-derived provenance is still an operator assertion; the
   sidecar prevents relabelling after staging.
6. **Fixed — archive destination confinement.** See the fixed table.
7. **Partially fixed — mkey handling.** The arbitrary eight-key cap is raised to
   64; a crafted wallet can still spend time on KDF attempts, bounded by that
   cap.
8. **Fixed — dump path swap.** The fallback re-reads the wallet after the dump
   and rejects the result if the bytes changed.
9. **Fixed — unchecked sums and wide varints.** Snapshot balances use checked
   addition; the base-128 decoder rejects overflow instead of wrapping.

### Wallet internals

10. **Fixed — cold-stake spending key custody.** Encrypted backups include the
    export; restore requires manual recovery. Funding still relies on the user
    confirming the file is saved; there is no persisted backed-up flag.
11. **Fixed — wallet format-version gating.** See the fixed table.
12. **Fixed — AEAD failure wording.** Decryption failures now say “incorrect
    passphrase or damaged wallet file” so bit rot is not mistaken for a typo.
13. **Medium — KDF parameters are compile-time constants not stored in the
    file.** Changing them makes every existing wallet undecryptable with no
    migration branch.
14. **Fixed — fee estimate.** Inputs, outputs, and bincode length prefixes are
    charged at their worst-case serialized size; a test asserts the fee covers
    the signed transaction's actual size.
15. **Fixed — single-key fallback removed.** A UTXO that does not match the
    supplied key is now a build error; the fee-floor test was updated to use a
    matching P2PKH script.
16. **Low — the HD mnemonic controls no keys** and has no restore path; delete it
    or implement derivation before exposing it.

### Frontend and IPC boundary

17. **Fixed — migration 2FA messaging.** The wizard now says 2FA is not
    migrated and warns the user to re-enable it after import.
18. **Partially fixed — RPC authorization.** Mainnet refuses to start without a
    key and release CORS is Tauri-only; regtest/testnet still allow
    unauthenticated loopback RPC for local development, and browser mode keeps
    its key in `localStorage`.
19. **Fixed — CSP and external font loading.** See the fixed table.
20. **Partially fixed — TOTP seed handling.** The seed and URI are cleared on
    success/cancel, and a copied seed is removed from the clipboard after 30
    seconds. The staking WIF still renders without a reveal gate.
21. **Low — the legacy claim page says the key is “never transmitted”**, while
    browser mode posts it to the loopback RPC.
22. **Low — locking the wallet leaves addresses/balances in the webview.**
23. **Low — frontend-supplied torrent/export paths are accepted unvalidated.**

### Operations and release

24. **High — releases are still unsigned**, and there is no published checksum
    file. The false “all releases are signed” claim is fixed, but signing and a
    published digest remain release work.
25. **Fixed — backup cron.** The policy now uses `umask 077`, skips missing
    files per-file, stops the daemon around the redb copy, verifies the tar
    listing, and keeps retention. A restore drill is required.
26. **Fixed — benchmark gate.** The comparison uses `release/Cargo.lock` and
    fails when the baseline is absent or nothing was compared.
27. **Low — the testnet stack runs root with `NET_ADMIN`/`NET_RAW` and
    `apparmor=unconfined`, Grafana is `admin/admin` on a LAN port, and bitcoind
    allows RPC from any address.**
28. **Low — image defaults invite exposure** (`0.0.0.0` RPC without a key,
    mutable default runtime image, root daemon).
29. **Fixed — dead-man wrapper path.** The service, installer table, and test
    now all use `/usr/local/sbin/vtorrent-monitor-ready`.

### Core and script

30. **Medium — CLTV/CSV use the candidate block's height/time, not the parent
    median-time-past**, and the height comparison is off by one (`>=` vs `<`).
    A staker can timestamp a refund block up to two hours early; the 6-hour swap
    margin absorbs it, but two hours of taker protection is spendable. CSV also
    does not require tx version 2.
31. **Fixed — HTLC classification.** The classifier matches both the standard
    builder and the atomic-swap `OP_IF OP_SIZE <32> ...` shape, including the
    32-byte hash push.
32. **Low — `Address::parse` accepts any version byte** outside the payment
    builder; the payload hash is the same, so this is confusion, not loss.
33. **Fixed — `Script::push_int`.** Values above 16 now use minimal push
    encodings, including the two-byte form for 0x80 and above.
34. **Info — interpreter leniencies** (codeseparator NOP, ELSE/ENDIF outside a
    conditional accepted, empty CHECKMULTISIG dummy accepted).

## Launch implications

- **Do not advertise legacy `wallet.dat` recovery until real wallets are
  tested.** Overflow parsing is fixed and fixtures pass, but the committed
  databases are synthetic.
- **Cold-stake funding should require a proven spending-key backup** (item 10);
  otherwise a wallet-only restore can lock new cold-stake funds forever.
- **The multisig cap matches the legacy client's 20-key limit, and the
  block-level sigop limit matches its `MAX_BLOCK_SIZE / 50` rule.** Templates
  skip transactions that would exceed the limit.
- **Unsigned releases (item 24) mean the download channel is not
  tamper-evident.** Publishing a signed `SHA256SUMS` is release work, not a code
  fix.
- The consensus timestamp/CLTV items (pass 2 findings 1–2, pass 3 item 30)
  remain the highest-risk open class and belong explicitly in the human review.

## Verdict

This pass identified recovery and release risks, but some original claims were
incorrect or insufficiently verified. None of this is independent review.
