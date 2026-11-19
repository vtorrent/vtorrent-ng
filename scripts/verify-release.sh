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

echo "release verification passed: $dir"
