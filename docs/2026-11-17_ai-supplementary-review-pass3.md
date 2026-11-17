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
> budget remains open.

> **AI-generated supplementary review. It does not satisfy the independent
> human security-review gate.** No code was shared externally. This pass covers
> areas the first two did not: untrusted legacy/snapshot parsers, wallet
> internals and signing, core primitives/script, the frontend IPC boundary, and
> operations/release safety.

**Reviewed tree:** `b9b05e3`, plus the fixes below (`8f7fe14`, `49d56d2`,
`aacf371`, and `20e3fe6`).
**Method:** five parallel AI-assisted read-only passes. Findings and fixes have
varying levels of verification; do not treat the report as independent review.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| Assessed | `OP_CHECKMULTISIG` lacked a 20-key cap. | `49d56d2` adds the cap. The local legacy client source (not in this repo) rejects more than 20 keys, so the rule matches legacy consensus. Tests cover 20 and 21 keys. A block-level sigop budget remains open. |
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
| Medium | The desktop legacy import decoded base64 with no size cap. | Fixed in `aacf371`: payloads above 32 MiB are rejected before decoding. |

Verified in the follow-up: 28 migration tests (including a real BDB fixture
that is parsed natively and through `db5.3_dump`), 91 script tests, 68 wallet
tests, 112 torrent tests, Clippy on the four changed crates, and
`deploy/test-provision-seed.sh` in `ubuntu:22.04`. The full workspace suite on
`49d56d2` passed **1,388 tests, 0 failures, 11 ignored**; the follow-up suite
passed **1,402 tests, 0 failures, 11 ignored**.

## Open findings

### Legacy migration and snapshot parsing

1. **High — representative `wallet.dat` recovery remains unproven.** The
   committed fixtures are synthetic, and no real legacy wallet has been imported
   end to end. Run a set of real wallets through both the native parser and
   `db5.3_dump` and compare the recovered key sets before advertising recovery.
2. **Medium — dump completeness is still inferred from record counts.** A dump
   that returns the same number of records but different contents would be
   accepted. Compare key material, not only counts.
3. **Medium — no fuzzing or malformed-fixture corpus.** The parser rejects
   unsupported page types and broken chains, but coverage is by example.
4. **Medium — the snapshot reader accepts truncation and tampering.**
   `entry_count` is clamped to available bytes, trailing bytes are ignored, the
   stored hash is not checked on load and does not cover the headline totals,
   and duplicate addresses are not merged; `lookup_balance` returns an arbitrary
   duplicate.
5. **Medium — `btc-state-stage` derives network provenance from the filename
   only**, and `--confirm-isolated/owner/recovery-preconditions` are accepted as
   false. Copying a mainnet state file to a `regtest` name bypasses the
   mainnet-disabled guard. Bind the network in persisted state and require the
   flags to be true.
6. **Medium — the archive destination is not confined** to the data directory or
   a seed-scoped name.
7. **Low/medium — mkey handling** rejects wallets with more than eight keys and
   permits a multi-second KDF burn per crafted key.
8. **Low — the dump fallback dumps and reopens by path**, allowing a same-user
    swap between read and dump.
9. **Low — unchecked balance sums and wide varints** in the snapshot/leveldb
    readers can panic under `overflow-checks` or wrap silently.

### Wallet internals

10. **High — the cold-stake spending key is absent from both the wallet and the
    desktop backup.** `spending_wif` is `skip_serializing`; `create_cold_stake`
    exports it once beside the wallet, and the backup collector never includes
    `*.spending-key`. After a machine loss and wallet-only restore, the registry
    still marks the script as fundable, but the key needed to withdraw is gone
    until the CLTV locktime (up to ~2036). Either encrypt the spending key inside
    the wallet, include the export file in the backup, or gate funding on a
    persisted “spending key backed up” flag.
11. **Medium — no wallet format-version gating.** Unknown versions/fields load,
    are silently dropped, and the next save (including the automatic TOTP
    re-save) rewrites the file as v1. Reject future versions and unknown fields.
12. **Medium — AEAD failures are reported as “incorrect passphrase”.** Bit rot
    or tampering is indistinguishable from a typo, inviting a restore from an old
    backup that loses later keys.
13. **Medium — KDF parameters are compile-time constants not stored in the
    file.** Changing them makes every existing wallet undecryptable with no
    migration branch.
14. **Low — the fee estimate undercounts serialization** (missing varint,
    `tx_type`, option markers), so fees are ~3% below the requested rate.
15. **Low — the single-key fallback in the P2PKH signer defeats the key/UTXO
    match guard**; every production caller passes one key.
16. **Low — the HD mnemonic controls no keys** and has no restore path; delete it
    or implement derivation before exposing it.

### Frontend and IPC boundary

17. **Medium — the migration UI claims 2FA was verified while it is dropped.**
    Import creates a fresh wallet and hardcodes `has2FA: false`, but the wizard
    shows a green “2FA verified” badge when the legacy file had 2FA.
18. **Medium — loopback RPC is unauthenticated by default** and CORS allows
    `http://localhost:3000`; a local process (or anything served on that port)
    can reach wallet/send/claim routes. Mint a key on first run and narrow CORS.
19. **Low/medium — CSP is permissive** (`connect-src` allows any loopback port,
    missing `object-src`/`base-uri`/`frame-ancestors`/`form-action`), the theme
    bootstrap is inline, and browser mode loads Google Fonts with no CSP.
20. **Low/medium — TOTP seed and staking WIF remain in React state/DOM/clipboard**
    without clearing or reveal gating.
21. **Low — the legacy claim page says the key is “never transmitted”**, while
    browser mode posts it to the loopback RPC.
22. **Low — locking the wallet leaves addresses/balances in the webview.**
23. **Low — frontend-supplied torrent/export paths are accepted unvalidated.**

### Operations and release

24. **High — releases are still unsigned**, and there is no published checksum
    file. The false “all releases are signed” claim is fixed, but signing and a
    published digest remain release work.
25. **Medium — the backup cron is fragile**: `set -e` with missing files skips
    retention, `tar` of a live redb is torn, no restore verification, and no
    `umask 077` around a wallet-bearing archive.
26. **Medium — the benchmark gate can no-op**: it regenerates a lockfile instead
    of using `release/Cargo.lock` and exits 0 when the baseline is empty.
27. **Low — the testnet stack runs root with `NET_ADMIN`/`NET_RAW` and
    `apparmor=unconfined`, Grafana is `admin/admin` on a LAN port, and bitcoind
    allows RPC from any address.**
28. **Low — image defaults invite exposure** (`0.0.0.0` RPC without a key,
    mutable default runtime image, root daemon).
29. **Low — the dead-man wrapper path does not match the installer.**

### Core and script

30. **Medium — CLTV/CSV use the candidate block's height/time, not the parent
    median-time-past**, and the height comparison is off by one (`>=` vs `<`).
    A staker can timestamp a refund block up to two hours early; the 6-hour swap
    margin absorbs it, but two hours of taker protection is spendable. CSV also
    does not require tx version 2.
31. **Medium/low — `classify_script` cannot recognize a real HTLC** (it matches
    a two-byte prefix that real scripts do not start with) and can mislabel an
    arbitrary script as one. No consumer today, but future fee/explorer rules
    keyed on it would be wrong.
32. **Low — `Address::parse` accepts any version byte** outside the payment
    builder; the payload hash is the same, so this is confusion, not loss.
33. **Low — `Script::push_int` mis-encodes values ≥ 0x80.**
34. **Info — interpreter leniencies** (codeseparator NOP, ELSE/ENDIF outside a
    conditional accepted, empty CHECKMULTISIG dummy accepted).

## Launch implications

- **Do not advertise legacy `wallet.dat` recovery until real wallets are
  tested.** Overflow parsing is fixed and fixtures pass, but the committed
  databases are synthetic.
- **Cold-stake funding should require a proven spending-key backup** (finding
  12); otherwise a wallet-only restore can lock new cold-stake funds forever.
- **The multisig cap matches the legacy client's own 20-key limit.** A
  block-level sigop budget remains a separate hardening item.
- **Unsigned releases (finding 26) mean the download channel is not
  tamper-evident.** Publishing a signed `SHA256SUMS` is release work, not a code
  fix.
- The consensus timestamp/CLTV items (pass 2 findings 1–2, pass 3 finding 32)
  remain the highest-risk open class and belong explicitly in the human review.

## Verdict

This pass identified recovery and release risks, but some original claims were
incorrect or insufficiently verified. None of this is independent review.
