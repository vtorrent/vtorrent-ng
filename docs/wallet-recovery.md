# vTorrent-NG Wallet Recovery

Recovering VTR from legacy vTorrent (VTR) wallet.dat files.

## Overview

vTorrent-NG includes a migration tool that parses legacy BerkeleyDB wallet.dat files, extracts private keys, and imports them into the new Rust wallet. The legacy wallet uses:

- **Key format**: WIF (Wallet Import Format) with prefix `0xCB` (203) for mainnet (`7...` addresses)
- **Address format**: Base58Check with prefix `0x4B` (75) for mainnet (`X...` addresses)
- **Encryption**: Optional AES-256-CBC encryption of private keys
- **Key derivation**: scrypt (legacy) or Argon2id (vTorrent-NG)
- **Privacy**: Stealth addresses (like Monero) for receiving transactions
- **Ring signatures**: Optional anonymous transactions (Anon module)

## Important: Stealth Address Limitation

**The legacy wallet uses stealth addresses as the default receiving mechanism.** This means:

- The actual deposit addresses that received VTR are **one-time stealth addresses**, not stored in the wallet.dat
- The wallet stores `scan_pubkey` + `spend_pubkey` pairs, not the derived deposit addresses
- The `ckey` records contain encrypted private keys for pool/stealth keys, not the spending keys for received funds
- The `pool` records (100 entries) are the address pool for generating new addresses

### How Stealth Addresses Work

```
1. Sender generates random ephemeral key `e`
2. Sender computes shared secret: S = e * scan_pubkey
3. Sender computes one-time pubkey: P = hash(S) * G + spend_pubkey
4. Funds are sent to address P (one-time, not stored in recipient's wallet)
5. Recipient detects payment by scanning with scan_secret
6. Recipient spends with: spend_secret + ephemeral_key (via StealthSecretSpend)
```

### Implication for Migration

**The migration tool cannot extract the actual deposit addresses from the wallet.dat.** The one-time stealth addresses are derived on-the-fly when scanning the blockchain and are not persisted in the wallet database.

For legacy claim purposes, users need:
1. The original stealth address scan/spend key pairs
2. Access to the old chain's blockchain to scan for transactions
3. Or: a list of the actual deposit addresses (if they have them from other sources)

## Recovery Methods

### Method 1: Import WIF Key

If you have the WIF private key exported from the legacy wallet:

```bash
vtorrent-cli wallet import --wif 7...
```

Or via RPC:
```json
POST /api/v1/wallet/import
{
  "wif": "7...",
  "label": "recovered-wallet"
}
```

### Method 2: Migrate wallet.dat

If you have the legacy `wallet.dat` file:

```bash
# Unencrypted wallet.dat
vtorrent-cli wallet migrate --dat /path/to/wallet.dat

# Encrypted wallet.dat
vtorrent-cli wallet migrate --dat /path/to/wallet.dat --passphrase "old-password"
```

The migration tool:
1. Opens the BerkeleyDB database (uses `db5.3_dump` fallback for BDB v5.3+)
2. Parses all `ckey` records (encrypted private keys) and `mkey` (master key)
3. If encrypted, decrypts keys using scrypt-derived key + AES-256-CBC
4. Extracts the pool/stealth key pairs
5. **Note**: Does NOT extract actual deposit addresses (stealth addresses are one-time)

### Method 3: From Seed Phrase (if available)

If the legacy wallet used a mnemonic seed phrase:

```bash
vtorrent-cli wallet import --seed "word1 word2 ... word24"
```

## Encrypted Wallet Recovery

The legacy wallet uses:

1. **Key derivation**: `EVP_BytesToKey(SHA-512, salt, passphrase, iterations)` (method 0) or scrypt (methods 1/2, N=2^14, r=8, p=1)
2. **Encryption**: AES-256-CBC(key, iv, plaintext)
3. **IV**: First 16 bytes of the KDF output (master key) or SHA256d(public key) (ckey records)
4. **Salt**: Stored in the wallet.dat `mkey` record
5. **OTP wallets**: effective passphrase = `hex(SHA256(otp_secret || passphrase))` — handled automatically

If you don't know the passphrase, recovery is not possible without brute-force.

## Legacy Chain Address Format

The old vTorrent client (v1.x) used different address prefixes than vTorrent-NG:

| Type | Legacy (v1.x) | vTorrent-NG (v2.0) |
|---|---|---|
| PUBKEY_ADDRESS | 75 (`X...`) | 70 (`V...`) |
| SECRET_KEY (WIF) | 203 (`7...`) | 198 (`W...`) |
| SCRIPT_ADDRESS | 125 (`Y...`) | 50 (`3...`) |

The genesis snapshot uses vTorrent-NG addresses (`V...`), but the old chain used `X...` addresses. The snapshot was built by converting `X→V` (same hash160, different version byte).

## Verifying Recovery

After import, verify your recovered address:

```bash
vtorrent-cli wallet addresses
```

Check the balance:
```bash
vtorrent-cli wallet balance
```

Or via RPC:
```json
GET /api/v1/wallet/balance
```

## Security Notes

- **Private keys are stored locally** in `~/.vtorrent/wallet.key` (encrypted with Argon2id + AES-256-GCM)
- **Passphrase is zeroized** from memory immediately after key derivation
- **Never share** your WIF keys or wallet.dat file
- **Backup** the new wallet after recovery: `vtorrent-cli wallet backup`
- **Legacy wallet.dat** should be securely deleted after successful recovery

### Legacy 2FA/OTP Weakness

The legacy wallet supported optional two-factor authentication (2FA/TOTP) via the `keyOTP` database record. The 2FA **was part of the unlock path**, but not in a way that protects key material from a wallet.dat holder:

```
OTP-enabled unlock flow (crypter_otp.cpp / wallet_otp.cpp):
  1. keyOTP record = otaCrypt(SimpleCrypt-style XOR) blob encrypted with the
     first 4 chars of the raw passphrase (lowercased) + CRC-16/X-25 checksum
  2. otp_secret = base64decode(ota_decrypt(keyOTP, passphrase))
  3. effective passphrase = hex(SHA256(otp_secret || passphrase))  ("mixedHash")
  4. master key = EVP_BytesToKey(SHA-512, salt, effective passphrase, iterations)
  5. master key -> AES-256-CBC -> private keys (ckey)
```

**Consequence**: the OTP secret is itself encrypted with only the passphrase (a 4-character-effective-key XOR cipher), and the TOTP code is only checked in the UI. Anyone with the passphrase can recover the OTP secret from wallet.dat and derive the effective passphrase offline — no 2FA code or authenticator app is required.

**For migration**: only the passphrase is required. The migration tool detects the `keyOTP` record and derives the mixed passphrase automatically (see `derive_otp_mixed_passphrase` in `vtorrent-migrate/src/crypter.rs`).

**For vTorrent-NG**: if re-implementing 2FA, the OTP secret must be factored into the key derivation function with a memory-hard KDF (e.g. `argon2id(passphrase || otp_secret, salt)`) and the OTP secret must never be stored on disk in a form recoverable with the passphrase alone.

### Decryption Validation (important)

Wrong passphrases are detected via two independent checks:
1. **PKCS7 padding** on the AES-256-CBC plaintext (master key + ckey records)
2. **Public key match**: the decrypted private key must derive the ckey record's public key

Earlier versions of the migration tool only checked that the decrypted bytes were a valid secp256k1 scalar — which random garbage passes ~100% of the time, silently producing bogus WIFs. Both checks are now mandatory.

## Troubleshooting

| Issue | Solution |
|---|---|
| "BerkeleyDB error" | Ensure wallet.dat is not locked by another process; tool uses `db5.3_dump` fallback |
| "Decryption failed" | Wrong passphrase — try again |
| "No keys found" | wallet.dat may be empty or corrupted |
| "Address mismatch" | Check network: legacy uses `X...` (prefix 75), vTorrent-NG uses `V...` (prefix 70) |
| "No claimable balance" | Stealth addresses are one-time; need to scan old chain for actual deposits |

## Implementation

- `vtorrent-migrate/src/`: Legacy wallet.dat parser (BerkeleyDB, AES-256-CBC, scrypt, db5.3_dump fallback)
- `vtorrent-wallet/src/wallet.rs`: New wallet key storage and management
- `vtorrent-wallet/src/encryption.rs`: Argon2id + AES-256-GCM encryption
- `vtorrent-node/src/genesis.rs`: Legacy snapshot (59,375 addresses, v2.0 format)
- `vtorrent-snapshot/`: UTXO snapshot extractor for old chain
