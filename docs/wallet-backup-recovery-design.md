# Wallet Backup & Recovery — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: the wallet is encrypted (Argon2id + ChaCha20-Poly1305) and saved
atomically (`wallet.rs:144`), and HD (BIP39) is optional (`:394`) — but there is
**no backup, no restore, and no way to verify a backup**. A user who loses the
file loses funds, and a user who *thinks* they have a backup may be wrong. For a
custody product this is a must-have.

## 1. What exists

- `EncryptedWallet` = `[version][salt][nonce][ciphertext+tag]`
  (`encryption.rs:30`), Argon2id-derived key, ChaCha20-Poly1305.
- `Wallet::save` (`wallet.rs:144`) is atomic (tmp + rename, with symlink
  hardening).
- HD account (BIP39 mnemonic) is **optional** (`wallet.rs:63,394`); non-HD
  wallets hold individual WIF keys (`WalletKeyEntry.wif`).

## 2. Design

### 2.1 Two backup forms (be explicit)

1. **Seed-phrase backup (HD wallets)** — the BIP39 mnemonic restores the whole
   HD wallet. This is the *portable* backup; it must be shown once, with a
   confirm-you-wrote-it-down step.
2. **Encrypted-file backup (all wallets)** — a copy of `wallet.json` plus the
   passphrase. Necessary for **non-HD** wallets (individual keys) and for
   legacy-imported keys, which the mnemonic does **not** cover.

The UI must state clearly **which keys a given backup covers** — a mnemonic does
not restore legacy-imported keys.

### 2.2 Backup

- **Export**: write an encrypted backup file (same format) to a user-chosen path,
  optionally with a **new passphrase**.
- **Verify**: re-open the exported file with the passphrase and re-derive the
  addresses/balances, confirming it decrypts and matches. A backup you haven't
  verified is not a backup.
- **Reminder**: periodic "have you backed up?" nudge (ties into
  `docs/notifications-design.md`), and a badge on the Security page.

### 2.3 Restore

- **From mnemonic**: enter the phrase, re-derive the HD account, rescan for
  balances.
- **From file**: import the encrypted file + passphrase; merge or replace.
- **Merge semantics**: restoring must not silently drop existing keys; define
  merge vs replace explicitly (recommend merge with a preview).

### 2.4 Recovery hygiene

- **Legacy import**: the legacy `wallet.dat` import path
  (`vtorrent-migrate`) is a separate recovery route; document it alongside.
- **2FA (TOTP)**: `otp.rs` — a restore must handle the OTP config (it's in the
  wallet data); losing the TOTP secret must not lock the user out of their own
  backup (decide: OTP gates *unlock*, not *decryption*).

## 3. Adversarial review

- **R1 — a mnemonic does not cover legacy-imported keys.** The most dangerous
  misunderstanding. Non-HD / legacy keys live only in the file; the UI must say
  so and require a file backup for them.
- **R2 — an unverified backup is worthless.** Always offer/require a verify step
  (decrypt + re-derive) after export.
- **R3 — restore must not silently drop keys.** Merge semantics with a preview;
  never overwrite a wallet with keys the backup lacks without explicit consent.
- **R4 — passphrase loss is unrecoverable.** Argon2id + no escrow means a lost
  passphrase = lost funds. State it plainly; don't imply recovery.
- **R5 — OTP must not brick recovery.** If OTP gates decryption, a lost TOTP
  secret locks the backup. Keep OTP at the *unlock* layer, not the *decryption*
  layer.
- **R6 — export path safety.** The exported file is as sensitive as the wallet;
  write with 0600, warn about cloud/plaintext locations, and never log it.
- **R7 — no consensus change.** Wallet/RPC/UI only.

## 4. Test plan

- Export → verify (decrypt + re-derive) succeeds; a wrong passphrase fails.
- Mnemonic restore reproduces the HD addresses/balances.
- File restore merges without dropping existing keys (preview shown).
- A non-HD wallet's mnemonic (none) is not offered as a backup; file backup is.
- OTP: a wallet with OTP restores/decrypts with the passphrase (OTP gates unlock).
- Export file is 0600; never logged.

## 5. Non-goals

- Not social recovery / multisig recovery (larger; pairs with cold staking).
- Not changing the encryption format.
- Not part of the current soak window.

## 6. References

- `vtorrent-wallet/src/encryption.rs:30` `EncryptedWallet`; `:48` `derive_key`.
- `vtorrent-wallet/src/wallet.rs:117` `load`, `:144` `save`, `:63,394` HD/mnemonic.
- `vtorrent-wallet/src/otp.rs`; `vtorrent-migrate` (legacy import).
- `docs/notifications-design.md` (backup reminders).
