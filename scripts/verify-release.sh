#!/usr/bin/env bash
# Verify a vTorrent release directory.
#
#   scripts/verify-release.sh <release-dir> [minisign.pub] [--require-signature]
#
# Checks SHA256SUMS against every listed file. If a public key is supplied and
# SHA256SUMS.minisig exists, verifies the signature first. With
# --require-signature, a missing/invalid signature is fatal.
set -euo pipefail

usage() {
    echo "usage: $0 <release-dir> [minisign.pub] [--require-signature]" >&2
    exit 2
}

[[ $# -ge 1 ]] || usage
dir="$1"
shift
pubkey=""
require_signature=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --require-signature) require_signature=1 ;;
        -h|--help) usage ;;
        *)
            if [[ -n "$pubkey" ]]; then usage; fi
            pubkey="$1"
            ;;
    esac
    shift
done

# Supplying a public key is a request to verify a signature, not a hint.
if [[ -n "$pubkey" ]]; then
    require_signature=1
fi

[[ -d "$dir" ]] || { echo "release directory not found: $dir" >&2; exit 1; }
sums="$dir/SHA256SUMS"
[[ -f "$sums" ]] || { echo "missing $sums" >&2; exit 1; }

if [[ -f "$dir/SHA256SUMS.minisig" ]]; then
    [[ -n "$pubkey" ]] || {
        echo "SHA256SUMS.minisig exists but no public key was supplied" >&2
        exit 1
    }
    command -v minisign >/dev/null 2>&1 || {
        echo "minisign is required to verify the release signature" >&2
        exit 1
    }
    minisign -Vm "$sums" -p "$pubkey" -x "$dir/SHA256SUMS.minisig"
elif [[ "$require_signature" -eq 1 ]]; then
    echo "release is unsigned: SHA256SUMS.minisig is missing" >&2
    exit 1
else
    echo "warning: release is unsigned; checking integrity only" >&2
fi

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$dir" && sha256sum -c SHA256SUMS)
else
    (cd "$dir" && shasum -a 256 -c SHA256SUMS)
fi

# A checksum file must cover every artifact in the directory. Otherwise an
# attacker can add an unlisted binary that no check ever touches.
python3 - "$dir" <<'PY'
import os
import sys

directory = sys.argv[1]
listed = set()
with open(os.path.join(directory, "SHA256SUMS"), encoding="utf-8") as handle:
    for line in handle:
        parts = line.rstrip("\n").split("  ", 1)
        if len(parts) == 2:
            listed.add(parts[1].removeprefix("./"))
actual = {
    name
    for name in os.listdir(directory)
    if os.path.isfile(os.path.join(directory, name))
    and name not in {"SHA256SUMS", "SHA256SUMS.minisig"}
}
extra = actual - listed
missing = listed - actual
if extra:
    print("unlisted release files: " + ", ".join(sorted(extra)), file=sys.stderr)
if missing:
    print("listed files missing: " + ", ".join(sorted(missing)), file=sys.stderr)
if extra or missing:
    sys.exit(1)
PY

echo "release verification passed: $dir"
