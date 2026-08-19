#!/bin/bash
# VTR-side refund smoke test: faucet -> place order -> match (fund) -> advance
# mocktime past expiry -> refund. Asserts the refund is rejected before expiry
# and lands in the mempool after.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DATA="/tmp/vtr-smoke-refund"
rm -rf "$DATA"

start_daemon "$DATA" 127.0.0.1:22525 127.0.0.1:22526
wait_for_rpc http://127.0.0.1:22525/api/v1/info

rpc POST http://127.0.0.1:22525/api/v1/faucet \
    "{\"address\":\"$MAKER_ADDR\",\"amount_satoshis\":10000000000}" > /dev/null
rpc POST http://127.0.0.1:22525/api/v1/wallet/import \
    "{\"wif\":\"$MAKER_WIF\",\"passphrase\":\"testpass\"}" > /dev/null
rpc POST http://127.0.0.1:22525/api/v1/wallet/unlock \
    '{"passphrase":"testpass","timeout_secs":0}' > /dev/null

echo "=== place DEX order (expiry 7200s) ==="
ORDER=$(rpc POST http://127.0.0.1:22525/api/v1/dex/order \
    "{\"maker_address\":\"$MAKER_ADDR\",\"offer_amount_satoshis\":100000000,\"offer_asset\":\"VTR\",\"request_amount_satoshis\":100000,\"request_asset\":\"BTC\",\"expiry_secs\":7200,\"passphrase\":\"testpass\"}")
ORDER_ID=$(jq_field "$ORDER" order_id)
echo "order id: $ORDER_ID"

echo "=== match (fund VTR HTLC) ==="
rpc POST http://127.0.0.1:22525/api/v1/dex/match \
    "{\"order_id\":\"$ORDER_ID\",\"taker_address\":\"$TAKER_ADDR\",\"passphrase\":\"testpass\"}" > /dev/null

echo "=== refund BEFORE expiry (should fail) ==="
BEFORE=$(rpc POST http://127.0.0.1:22525/api/v1/swap/refund \
    "{\"order_id\":\"$ORDER_ID\"}")
echo "$BEFORE"
if ! echo "$BEFORE" | grep -q "not expired"; then
    echo "FAIL: refund before expiry was not rejected" >&2
    exit 1
fi

echo "=== advance mocktime past expiry ==="
FUTURE=$(( $(date +%s) + 14400 ))
rpc POST http://127.0.0.1:22525/api/v1/debug/mocktime \
    "{\"timestamp\":$FUTURE}" > /dev/null

echo "=== refund AFTER expiry (should succeed) ==="
AFTER=$(rpc POST http://127.0.0.1:22525/api/v1/swap/refund \
    "{\"order_id\":\"$ORDER_ID\"}")
echo "$AFTER"
REFUND_TXID=$(jq_field "$AFTER" txid)

MEMPOOL=$(rpc GET http://127.0.0.1:22525/api/v1/mempool)
echo "=== mempool ==="
echo "$MEMPOOL"

if ! echo "$MEMPOOL" | grep -q "$REFUND_TXID"; then
    echo "FAIL: refund tx not in mempool" >&2
    exit 1
fi

echo "PASS: VTR HTLC refunded after expiry"
