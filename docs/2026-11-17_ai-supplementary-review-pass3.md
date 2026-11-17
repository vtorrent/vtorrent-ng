# AI supplementary security review — third pass, 2026-11-17

> **AI-generated supplementary review. It does not satisfy the independent
> human security-review gate.** No code was shared externally. This pass covers
> areas the first two did not: untrusted legacy/snapshot parsers, wallet
> internals and signing, core primitives/script, the frontend IPC boundary, and
> operations/release safety.

**Reviewed tree:** `b9b05e3`, plus the fixes below (`8f7fe14`, `49d56d2`).
**Method:** five parallel read-only passes. Every fixed item was reproduced or
re-derived against the source and covered by a focused test; open items below
were verified by code reading except where marked as requiring a fixture.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| High (DoS) | `OP_CHECKMULTISIG` had no 20-key consensus cap. A hand-built script with ~148 keys and ~68 signatures forced thousands of ECDSA verifications inside one execution step; a block could burn minutes of validation CPU. | Fixed in `49d56d2`: `n_keys` and `n_sigs` are capped at 20 before verification, with a regression test showing a 21-key script is rejected. A separate per-block sigop budget remains open. |
| Medium | Wallet `save()` used a deterministic temp name and leaked the temp on write/sync errors, so one ENOSPC/EIO bricked every later save in the process. | Fixed in `49d56d2`: unique temp name, `create_new`, fsync, cleanup on every error arm. |
| Medium | `ColdStakeKeys` derived `Debug`, printing both staking and spending WIFs; `Zeroizing<String>`'s `Debug` prints the inner value. | Fixed in `49d56d2`: manual redacted `Debug`; test asserts neither WIF appears. |
| Medium | The desktop refund-bump form sent no passphrase/OTP, so the newly required authorization always failed. | Fixed in `49d56d2`: the form passes the existing wallet passphrase and 2FA code; the hook forwards both. |
| High | Release workflow pinned the signing-key-bearing `tauri-action` to a mutable tag and passed signing secrets to it, while releases are actually unsigned. | Fixed in `8f7fe14`: every action is SHA-pinned; top-level `permissions: contents: read`; the release job keeps its `contents: write` override. Signing env vars were removed because no signing identity/updater is configured, and the release text now states the builds are unsigned. |
| High | Release build did not enforce the audited lockfile. | Fixed in `8f7fe14`: `tauri build` receives `--locked`. |
| High | Seed installer copied `/tmp/vtorrent-daemon` over a root path with no verification. | Fixed in `8f7fe14`: the installer requires `VTR_DAEMON_SHA256`, verifies it, and refuses a mismatch; it also creates a `0600` RPC key and passes `--rpc-api-key-file`. |
| Medium | The ntfy topic was committed and treated as the secret. | Fixed in `8f7fe14`: the relay reads `VTR_NTFY_TOPIC` and fails closed; the README no longer prints a topic. **The old topic must still be rotated** because it is in git history. |

Verified: 90 script tests, 67 wallet tests, 44 frontend tests, frontend lint and
build, `bash -n`, Python compile, YAML parse, and Clippy on the changed crates.
The full workspace suite on `49d56d2` passed **1,388 tests, 0 failures, 11
ignored**.

## Open findings

### Legacy migration and snapshot parsing

1. **Critical (needs a real-BDB fixture before release) — the native BDB parser
   reads page-header fields 8 bytes early.** `vtorrent-migrate/src/bdb.rs` uses
   `page[17]` for type, `page[12..14]` for entries, and `page[0..4]` for the page
   number; BDB 4.8/5.3 uses 25, 20, and 12. The reviewer reproduced zero records
   from a real `db5.3_load` database. On Linux the external dump masks this; on
   Windows/macOS desktops without `db5.3_dump`, legacy import would always fail.
   Fix the offsets, validate `pgno`, and add a real fixture (the current test
   fabricates the parser's own layout).
2. **High — the `db5.3_dump` fallback is position-based, not format-based.**
   It pairs `lines[2..]` and drops blank lines, so an empty value (emitted as a
   single space) shifts parity and every later record is misparsed. It never
   locates `HEADER=END`/`DATA=END` or checks the record count.
3. **High — dump results replace native results with no completeness check, and
   non-zero dumps are accepted** if stdout happens to contain `main`/`ckey` hex.
   A salvage run that died after one key is treated as success.
4. **High — native parse memory amplification.** Offsets are neither unique nor
   monotonic, so every pair re-copies up to a page; a 256 KiB crafted file drove
   ~379 MB RSS in the reviewer's test. Require strictly increasing offsets, no
   overlaps, and a total decoded-byte cap.
5. **Medium — the desktop import has no input-size cap** before base64 decode,
   clone, and the `/tmp` dump fallback.
6. **Medium — the snapshot reader accepts truncation and tampering.**
   `entry_count` is clamped to available bytes, trailing bytes are ignored, the
   stored hash is not checked on load and does not cover the headline totals,
   and duplicate addresses are not merged; `lookup_balance` returns an arbitrary
   duplicate.
7. **Medium — `btc-state-stage` derives network provenance from the filename
   only**, and `--confirm-isolated/owner/recovery-preconditions` are accepted as
   false. Copying a mainnet state file to a `regtest` name bypasses the
   mainnet-disabled guard. Bind the network in persisted state and require the
   flags to be true.
8. **Medium — the archive destination is not confined** to the data directory or
   a seed-scoped name.
9. **Low/medium — mkey handling** rejects wallets with more than eight keys and
   permits a multi-second KDF burn per crafted key.
10. **Low — the dump fallback dumps and reopens by path**, allowing a same-user
    swap between read and dump.
11. **Low — unchecked balance sums and wide varints** in the snapshot/leveldb
    readers can panic under `overflow-checks` or wrap silently.

### Wallet internals

12. **High — the cold-stake spending key is absent from both the wallet and the
    desktop backup.** `spending_wif` is `skip_serializing`; `create_cold_stake`
    exports it once beside the wallet, and the backup collector never includes
    `*.spending-key`. After a machine loss and wallet-only restore, the registry
    still marks the script as fundable, but the key needed to withdraw is gone
    until the CLTV locktime (up to ~2036). Either encrypt the spending key inside
    the wallet, include the export file in the backup, or gate funding on a
    persisted “spending key backed up” flag.
13. **Medium — no wallet format-version gating.** Unknown versions/fields load,
    are silently dropped, and the next save (including the automatic TOTP
    re-save) rewrites the file as v1. Reject future versions and unknown fields.
14. **Medium — AEAD failures are reported as “incorrect passphrase”.** Bit rot
    or tampering is indistinguishable from a typo, inviting a restore from an old
    backup that loses later keys.
15. **Medium — KDF parameters are compile-time constants not stored in the
    file.** Changing them makes every existing wallet undecryptable with no
    migration branch.
16. **Low — the fee estimate undercounts serialization** (missing varint,
    `tx_type`, option markers), so fees are ~3% below the requested rate.
17. **Low — the single-key fallback in the P2PKH signer defeats the key/UTXO
    match guard**; every production caller passes one key.
18. **Low — the HD mnemonic controls no keys** and has no restore path; delete it
    or implement derivation before exposing it.

### Frontend and IPC boundary

19. **Medium — the migration UI claims 2FA was verified while it is dropped.**
    Import creates a fresh wallet and hardcodes `has2FA: false`, but the wizard
    shows a green “2FA verified” badge when the legacy file had 2FA.
20. **Medium — loopback RPC is unauthenticated by default** and CORS allows
    `http://localhost:3000`; a local process (or anything served on that port)
    can reach wallet/send/claim routes. Mint a key on first run and narrow CORS.
21. **Low/medium — CSP is permissive** (`connect-src` allows any loopback port,
    missing `object-src`/`base-uri`/`frame-ancestors`/`form-action`), the theme
    bootstrap is inline, and browser mode loads Google Fonts with no CSP.
22. **Low/medium — TOTP seed and staking WIF remain in React state/DOM/clipboard**
    without clearing or reveal gating.
23. **Low — the legacy claim page says the key is “never transmitted”**, while
    browser mode posts it to the loopback RPC.
24. **Low — locking the wallet leaves addresses/balances in the webview.**
25. **Low — frontend-supplied torrent/export paths are accepted unvalidated.**

### Operations and release

26. **High — releases are still unsigned**, and there is no published checksum
    file. The false “all releases are signed” claim is fixed, but signing and a
    published digest remain release work.
27. **Medium — the backup cron is fragile**: `set -e` with missing files skips
    retention, `tar` of a live redb is torn, no restore verification, and no
    `umask 077` around a wallet-bearing archive.
28. **Medium — the benchmark gate can no-op**: it regenerates a lockfile instead
    of using `release/Cargo.lock` and exits 0 when the baseline is empty.
29. **Low — the testnet stack runs root with `NET_ADMIN`/`NET_RAW` and
    `apparmor=unconfined`, Grafana is `admin/admin` on a LAN port, and bitcoind
    allows RPC from any address.**
30. **Low — image defaults invite exposure** (`0.0.0.0` RPC without a key,
    mutable default runtime image, root daemon).
31. **Low — the dead-man wrapper path does not match the installer.**

### Core and script

32. **Medium — CLTV/CSV use the candidate block's height/time, not the parent
    median-time-past**, and the height comparison is off by one (`>=` vs `<`).
    A staker can timestamp a refund block up to two hours early; the 6-hour swap
    margin absorbs it, but two hours of taker protection is spendable. CSV also
    does not require tx version 2.
33. **Medium/low — `classify_script` cannot recognize a real HTLC** (it matches
    a two-byte prefix that real scripts do not start with) and can mislabel an
    arbitrary script as one. No consumer today, but future fee/explorer rules
    keyed on it would be wrong.
34. **Low — `Address::parse` accepts any version byte** outside the payment
    builder; the payload hash is the same, so this is confusion, not loss.
35. **Low — `Script::push_int` mis-encodes values ≥ 0x80.**
36. **Info — interpreter leniencies** (codeseparator NOP, ELSE/ENDIF outside a
    conditional accepted, empty CHECKMULTISIG dummy accepted).

## Launch implications

- **Do not rely on legacy `wallet.dat` import until finding 1 is fixed and tested
  against a real BDB file, and findings 2–4 are fixed.** This is the recovery
  path for old holders.
- **Cold-stake funding should require a proven spending-key backup** (finding
  12); otherwise a wallet-only restore can lock new cold-stake funds forever.
- **The multisig cap closes the immediate script DoS, but a block-level sigop
  budget is still needed** before mainnet.
- **Unsigned releases (finding 26) mean the download channel is not
  tamper-evident.** Publishing a signed `SHA256SUMS` is release work, not a code
  fix.
- The consensus timestamp/CLTV items (pass 2 findings 1–2, pass 3 finding 32)
  remain the highest-risk open class and belong explicitly in the human review.

## Verdict

This pass found one critical legacy-parser defect (pending a real fixture), a
high cold-stake recovery gap, a high script-DoS vector (now capped), and a set
of release/ops weaknesses. The fixed items are verified; the rest require design
decisions or real fixtures. None of this is independent review.
