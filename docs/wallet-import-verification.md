# Legacy wallet import verification

`scripts/wallet-import-check.sh` compares the native BerkeleyDB parser with
`db5.3_dump` on a real `wallet.dat` and checks that the migration tool extracts
one key for every `key`/`ckey` record.

This is evidence for the recovery path. It is not independent review, and it
does not prove that every historical wallet layout is covered.

## Requirements

- `db5.3_dump` on `PATH`
- a Rust toolchain (`cargo`)
- representative wallets supplied by their owners

## Run

```bash
VTORRENT_WALLET_PASSPHRASE='...' scripts/wallet-import-check.sh wallet.dat
```

The passphrase is read only from the environment; it never appears in argv.
Addresses are printed, WIFs are not. For an unencrypted wallet, omit the
variable.

The script runs the tool twice: once with the normal `db5.3_dump` fallback and
once with `PATH` cleared so only the native parser can run. It fails if the two
extractions differ, if the dump is incomplete, or if the extracted key count
does not match the dump's `key`/`ckey` records.

## Record the result

Retain the `PASS` output with the wallet fingerprint (not the wallet) for the
review package. If a wallet fails, keep the wallet and the exact command out of
shared logs; report only the record counts and the failing addresses.

## Scope limits

- The committed parser fixtures are synthetic. A passing run on real wallets is
  what supports advertising legacy recovery.
- `db5.3_dump` is the reference only if it reads the same BerkeleyDB version.
  Record the `db5.3_dump` version with the result.
- Mainnet legacy BTC promotion remains disabled pending independent review.
