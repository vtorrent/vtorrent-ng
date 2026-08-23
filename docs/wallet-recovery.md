# vTorrent-NG Wallet Recovery

Recovering VTR from legacy vTorrent (VTR) wallet.dat files.

## Overview

vTorrent-NG includes a migration tool that parses legacy BerkeleyDB wallet.dat files, extracts private keys, and imports them into the new Rust wallet. The legacy wallet uses:

- **Key format**: WIF (Wallet Import Format) with prefix `0xC6` (198) for mainnet, `0xEF` (239) for testnet
- **Address format**: Base58Check with prefix `0x46` (70) for mainnet (`V...` addresses)
- **Encryption**: Optional AES-256-CBC encryption of private keys
- **Key derivation**: scrypt (legacy) or Argon2id (vTorrent-NG)

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
1. Opens the BerkeleyDB database
2. Parses all `key` records (WIF-encoded private keys)
3. If encrypted, decrypts keys using scrypt-derived key + AES-256-CBC
4. Imports all keys into the vTorrent-NG wallet
5. Labels them with the original key creation timestamp

### Method 3: From Seed Phrase (if available)

If the legacy wallet used a mnemonic seed phrase:

```bash
vtorrent-cli wallet import --seed "word1 word2 ... word24"
```

## Encrypted Wallet Recovery

The legacy wallet uses:

1. **Key derivation**: `scrypt(password, salt, N=8192, r=8, p=1)` → 32-byte key
2. **Encryption**: AES-256-CBC(key, iv, plaintext)
3. **IV**: First 16 bytes of the encrypted data
4. **Salt**: Stored in the wallet.dat `ckey` record

If you don't know the passphrase, recovery is not possible without brute-force.

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

## Troubleshooting

| Issue | Solution |
|---|---|
| "BerkeleyDB error" | Ensure wallet.dat is not locked by another process |
| "Decryption failed" | Wrong passphrase — try again |
| "No keys found" | wallet.dat may be empty or corrupted |
| "Address mismatch" | Check network: mainnet vs testnet prefix |

## Implementation

- `vtorrent-migrate/src/`: Legacy wallet.dat parser (BerkeleyDB, AES-256-CBC, scrypt)
- `vtorrent-wallet/src/wallet.rs`: New wallet key storage and management
- `vtorrent-wallet/src/encryption.rs`: Argon2id + AES-256-GCM encryption
