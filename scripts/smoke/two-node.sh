#!/bin/bash
# Two-node regtest smoke test: verifies P2P connectivity and block propagation.
#
# Starts two daemons (A seeds B), faucets coins on A, and asserts that B
# receives the minted block via inv -> getdata -> block.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

A_DATA="/tmp/vtr-smoke-a"
B_DATA="/tmp/vtr-smoke-b"
rm -rf "$A_DATA" "$B_DATA"

start_daemon "$A_DATA" 127.0.0.1:22525 127.0.0.1:22526
start_daemon "$B_DATA" 127.0.0.1:22527 127.0.0.1:22528 --seed 127.0.0.1:22526

wait_for_rpc http://127.0.0.1:22525/api/v1/info
wait_for_rpc http://127.0.0.1:22527/api/v1/info
sleep 3

echo "=== peers A ==="
rpc GET http://127.0.0.1:22525/api/v1/peers
echo
echo "=== peers B ==="
rpc GET http://127.0.0.1:22527/api/v1/peers
echo

echo "=== faucet on A ==="
rpc POST http://127.0.0.1:22525/api/v1/faucet \
    '{"address":"VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT","amount_satoshis":10000000000}'
echo
sleep 6

A_HEIGHT=$(jq_num "$(rpc GET http://127.0.0.1:22525/api/v1/blockchain/height)" height)
B_HEIGHT=$(jq_num "$(rpc GET http://127.0.0.1:22527/api/v1/blockchain/height)" height)
echo "A height: $A_HEIGHT"
echo "B height: $B_HEIGHT"

if [[ -z "$B_HEIGHT" || "$B_HEIGHT" -lt 1 ]]; then
    echo "FAIL: node B did not receive the minted block" >&2
    exit 1
fi

echo "PASS: block propagated from A to B"
