# Passphrase Change & Key Rotation — Design

Status: DRAFT (no consensus change; wallet + RPC + UI)
Scope: `vtorrent-wallet`, `vtorrent-rpc`, `vtorrent-ui`.
Motivation: the wallet is encrypted with a passphrase (Argon2id +
ChaCha20-Poly1305, `encryption.rs`), but there is **no way to change the
passphrase** — a user who suspects a passphrase leak cannot rotate it without
re-importing keys. This is a basic custody operation. The design also covers
rotating the wallet's keys (a stronger response to a suspected key compromise).

## 1. What exists

- `encrypt_wallet` / `decrypt_wallet` (`encryption.rs:68`), `derive_key` (`:48`),
  `EncryptedWallet { version, salt, nonce, ciphertext }` (`:30`).
- `Wallet::save` (`wallet.rs:144`) atomic; `Wallet::load` (`:117`).
- No passphrase-change path.

## 2. Design

### 2.1 Change passphrase (re-encrypt)

- `change_passphrase(old, new)`:
  1. **Verify** `old` (decrypt the wallet).
  2. Re-encrypt the plaintext with a **new salt + nonce** and a key derived from
     `new` (Argon2id).
  3. **Atomically** replace `wallet.json` (tmp + rename, as `save` does).
  4. Zeroize the old passphrase and the plaintext buffers.
- **Fresh salt + nonce** are mandatory (never reuse); the new file must decrypt
  with `new` and **not** with `old`.
- **2FA**: if TOTP is enabled, require the OTP to change the passphrase (the OTP
  gates the operation, not the decryption — `docs/wallet-backup-recovery-design.md`
  R5).

### 2.2 Verify after change

- Re-open the new file with `new`, re-derive addresses, and confirm balances
  match — a change that bricks the wallet is unacceptable. If verification fails,
  **restore the old file** (keep a backup until verified).

### 2.3 Key rotation (stronger)

- If a **key** (not just the passphrase) is suspected compromised, the user must
  move funds to **fresh addresses** (a new HD account or new keys) and sweep the
  old UTXOs. This is a **transaction**, not just a re-encrypt.
- Provide a **"rotate to new keys"** flow: generate a new account, sweep old
  UTXOs to it, and mark the old keys retired (kept for history, not for new
  receives).
- **Staking**: if the compromised key was staking, re-delegate/restake under the
  new key (ties to `docs/cold-staking-p2cs-design.md` — the spending key can
  rotate without the staking key, and vice versa).

### 2.4 UI

- A **Change passphrase** panel on the Security page (old, new, confirm; OTP if
  enabled), with a "verify" step and a warning to back up first.
- A separate **Rotate keys** flow with the sweep explanation.

## 3. Adversarial review

- **R1 — fresh salt + nonce.** Reusing the salt/nonce across a re-encrypt is a
  nonce-reuse catastrophe for ChaCha20-Poly1305. Always regenerate; test that the
  new ciphertext differs and decrypts only with the new passphrase.
- **R2 — atomic + backup.** A crash mid-change must not brick the wallet; write
  atomically and keep the old file until the new one verifies.
- **R3 — verify, don't assume.** Re-open + re-derive after the change; restore on
  failure.
- **R4 — OTP gates the operation.** With 2FA on, require the OTP to change the
  passphrase, but keep OTP out of the *decryption* layer (else a lost OTP bricks
  the wallet).
- **R5 — rotation is a transaction.** Changing keys means sweeping funds; don't
  conflate it with a re-encrypt. The UI must make the difference clear.
- **R6 — zeroize.** Old passphrase, old plaintext, and intermediate keys are
  zeroized on drop; no copies linger.
- **R7 — no consensus change.** Wallet/RPC/UI only.

## 4. Test plan

- Change passphrase: new file decrypts with `new`, fails with `old`; addresses
  and balances unchanged; salt/nonce differ.
- Crash mid-change leaves the old wallet intact (atomic).
- Verify-after-change restores the old file on a forced failure.
- OTP required when 2FA is on; not required for decryption.
- Key rotation sweeps UTXOs to new keys; old keys retired but history preserved.
- No consensus/chain change.

## 5. Non-goals

- Not hardware-backed key rotation (pairs with hardware signing).
- Not a password manager.
- Not part of the current soak window.

## 6. References

- `vtorrent-wallet/src/encryption.rs:30,48,68` `EncryptedWallet`, `derive_key`,
  encrypt/decrypt.
- `vtorrent-wallet/src/wallet.rs:117,144` load/save; `otp.rs`.
- `docs/wallet-backup-recovery-design.md`, `docs/cold-staking-p2cs-design.md`,
  `docs/hardware-wallet-signing-design.md`.
