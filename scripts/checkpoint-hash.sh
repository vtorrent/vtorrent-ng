#!/usr/bin/env bash
# Read a block height from one or more vTorrent RPC endpoints and print the
# agreed checkpoint as HEIGHT:HASH. Exits non-zero on any RPC error or
# disagreement.
#
#   scripts/checkpoint-hash.sh 10000 http://127.0.0.1:22525 http://seed2:22525
#
# RPC key: VTORRENT_RPC_KEY, or VTORRENT_RPC_KEY_FILE (default
# /etc/vtorrent/rpc-api-key when readable).
set -euo pipefail

height="${1:-}"
shift || true
if [[ -z "$height" || $# -eq 0 ]]; then
    echo "usage: $0 <height> <rpc-url> [rpc-url...]" >&2
    exit 2
fi
if ! [[ "$height" =~ ^[0-9]+$ ]]; then
    echo "height must be a non-negative integer" >&2
    exit 2
fi

key="${VTORRENT_RPC_KEY:-}"
if [[ -z "$key" ]]; then
    key_file="${VTORRENT_RPC_KEY_FILE:-/etc/vtorrent/rpc-api-key}"
    if [[ -r "$key_file" ]]; then
        key="$(cat "$key_file")"
    fi
fi
if [[ -z "$key" ]]; then
    echo "set VTORRENT_RPC_KEY or VTORRENT_RPC_KEY_FILE" >&2
    exit 2
fi

hashes=()
for url in "$@"; do
    url="${url%/}"
    response="$(curl -fsS -H "X-API-Key: $key" \
        "$url/api/v1/blockchain/block/height/$height")"
    hash="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["hash"])' <<<"$response")"
    if [[ ! "$hash" =~ ^[0-9a-fA-F]{64}$ ]]; then
        echo "$url returned an invalid block hash" >&2
        exit 1
    fi
    echo "$url: $hash" >&2
    hashes+=("${hash,,}")
done

first="${hashes[0]}"
for hash in "${hashes[@]:1}"; do
    if [[ "$hash" != "$first" ]]; then
        echo "checkpoint disagreement at height $height" >&2
        exit 1
    fi
done

printf '%s:%s\n' "$height" "$first"
