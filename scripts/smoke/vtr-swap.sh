#!/bin/bash
# VTR-side atomic swap smoke test: faucet -> place order -> match (fund VTR
# HTLC) -> claim VTR. Asserts both the funding and claim transactions land in
# the mempool.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

DATA="/tmp/vtr-smoke-swap"
rm -rf "$DATA"

start_daemon "$DATA" 127.0.0.1:22525 127.0.0.1:22526
wait_for_rpc http://127.0.0.1:22525/api/v1/info

echo "=== faucet 100 VTR to maker ==="
rpc POST http://127.0.0.1:22525/api/v1/faucet \
    "{\"address\":\"$MAKER_ADDR\",\"amount_satoshis\":10000000000}" > /dev/null

echo "=== import + unlock maker wallet ==="
rpc POST http://127.0.0.1:22525/api/v1/wallet/import \
    "{\"wif\":\"$MAKER_WIF\",\"passphrase\":\"testpass\"}" > /dev/null
rpc POST http://127.0.0.1:22525/api/v1/wallet/unlock \
    '{"passphrase":"testpass","timeout_secs":0}' > /dev/null

echo "=== place DEX order ==="
ORDER=$(rpc POST http://127.0.0.1:22525/api/v1/dex/order \
    "{\"maker_address\":\"$MAKER_ADDR\",\"offer_amount_satoshis\":100000000,\"offer_asset\":\"VTR\",\"request_amount_satoshis\":100000,\"request_asset\":\"BTC\",\"expiry_secs\":86400,\"passphrase\":\"testpass\"}")
ORDER_ID=$(jq_field "$ORDER" order_id)
echo "order id: $ORDER_ID"

echo "=== match (fund VTR HTLC) ==="
MATCH=$(rpc POST http://127.0.0.1:22525/api/v1/dex/match \
    "{\"order_id\":\"$ORDER_ID\",\"taker_address\":\"$TAKER_ADDR\",\"passphrase\":\"testpass\"}")
FUNDING_TXID=$(jq_field "$MATCH" funding_txid)
echo "funding txid: $FUNDING_TXID"

echo "=== get preimage (regtest debug) ==="
PRE=$(rpc GET "http://127.0.0.1:22525/api/v1/debug/order/$ORDER_ID/preimage")
PREIMAGE=$(jq_field "$PRE" preimage)
echo "preimage: $PREIMAGE"

echo "=== vtr_claim ==="
CLAIM=$(rpc POST http://127.0.0.1:22525/api/v1/swap/vtr-claim \
    "{\"order_id\":\"$ORDER_ID\",\"preimage\":\"$PREIMAGE\",\"taker_wif\":\"$TAKER_WIF\"}")
CLAIM_TXID=$(jq_field "$CLAIM" txid)
echo "claim txid: $CLAIM_TXID"

echo "=== mempool ==="
MEMPOOL=$(rpc GET http://127.0.0.1:22525/api/v1/mempool)
echo "$MEMPOOL"

if ! echo "$MEMPOOL" | grep -q "$FUNDING_TXID"; then
    echo "FAIL: funding tx not in mempool" >&2
    exit 1
fi
if ! echo "$MEMPOOL" | grep -q "$CLAIM_TXID"; then
    echo "FAIL: claim tx not in mempool" >&2
    exit 1
fi

echo "PASS: VTR HTLC funded and claimed"
