#!/usr/bin/env bash
# Differential check for legacy wallet import.
#
# Compares the native BDB parser with db5.3_dump on a real wallet and verifies
# that the migration tool extracts one key for every key record. Passphrases are
# read only from VTORRENT_WALLET_PASSPHRASE so they don't appear in argv.
#
# Usage:
#   VTORRENT_WALLET_PASSPHRASE='...' scripts/wallet-import-check.sh wallet.dat
#
# Run this against representative real wallets before advertising recovery. It
# is evidence for the import path, not a substitute for independent review.
set -euo pipefail

wallet=${1:?usage: wallet-import-check.sh <wallet.dat>}
[[ -f "$wallet" ]] || { echo "not a file: $wallet" >&2; exit 1; }
command -v db5.3_dump >/dev/null || { echo "db5.3_dump is required" >&2; exit 1; }

target_dir=${CARGO_TARGET_DIR:-target}
cargo build -p vtorrent-migrate --quiet
binary="$target_dir/debug/vtorrent-migrate"
[[ -x "$binary" ]] || { echo "missing migration binary: $binary" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

"$binary" "$wallet" --json >"$work/dump-path.json"
PATH=/nonexistent "$binary" "$wallet" --json >"$work/native.json" 2>"$work/native.err" || {
  echo "native-only import failed:" >&2
  cat "$work/native.err" >&2
  exit 1
}

python3 - "$wallet" "$work/dump-path.json" "$work/native.json" <<'PY'
import json
import subprocess
import sys

wallet, dump_path, native_path = sys.argv[1:4]

def load(path):
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
    return {key["legacy_address"] for key in data.get("keys", [])}

dump_path_keys = load(dump_path)
native_keys = load(native_path)
if dump_path_keys != native_keys:
    print("FAIL: dump-path and native-only extraction differ", file=sys.stderr)
    print("  dump-path only:", sorted(dump_path_keys - native_keys), file=sys.stderr)
    print("  native only:  ", sorted(native_keys - dump_path_keys), file=sys.stderr)
    sys.exit(1)

text = subprocess.run(
    ["db5.3_dump", "-r", wallet], check=True, capture_output=True, text=True
).stdout
records = 0
key_records = 0
in_data = False
pending = None
for line in text.splitlines():
    value = line.strip()
    if not in_data:
        if value == "HEADER=END":
            in_data = True
        continue
    if value == "DATA=END":
        if pending is not None:
            print("FAIL: db5.3_dump ended with an unmatched key", file=sys.stderr)
            sys.exit(1)
        in_data = False
        continue
    data = bytes.fromhex(value) if value else b""
    if pending is None:
        pending = data
    else:
        records += 1
        key = pending
        if key:
            length = key[0]
            if length in (3, 4) and key[1 : 1 + length] in (b"key", b"ckey"):
                key_records += 1
        pending = None
if pending is not None or in_data:
    print("FAIL: db5.3_dump output is incomplete", file=sys.stderr)
    sys.exit(1)

if len(dump_path_keys) != key_records:
    print(
        f"FAIL: extracted {len(dump_path_keys)} keys but the dump contains "
        f"{key_records} key records",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"PASS: {wallet}")
print(f"  records: {records}")
print(f"  key records: {key_records}")
print(f"  extracted addresses: {len(dump_path_keys)}")
print("  dump-path and native-only extraction agree")
PY
