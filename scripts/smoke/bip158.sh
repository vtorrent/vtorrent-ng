#!/bin/bash
# Bitcoin BIP-158 UTXO scan smoke test against a local Bitcoin Core regtest
# node. Requires Docker and a bitcoind container with -blockfilterindex=1 and
# -peerblockfilters=1 (see README.md in this directory).

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# Configurable via env; defaults match the README setup.
BTC_CONTAINER="${BTC_CONTAINER:-vtr-btc-regtest}"
BTC_PORT="${BTC_PORT:-18444}"
BTC_SEED="${BTC_SEED:-33333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333333}"
# Deterministic index-0 address for the seed above (derived via
# vtorrent_btc::keys::derive_address(&[0x33; 64], 0, Regtest)).
DAEMON_BTC_ADDR="${DAEMON_BTC_ADDR:-bcrt1qvumegsp0hfnxndattnaa4h57r3m88r9dtn5eqf}"

DATA="/tmp/vtr-smoke-bip158"
rm -rf "$DATA"

# Fund the daemon's deterministic address before it starts (the scan runs
# once at startup, then every 300s).
docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
    -rpcwallet=test sendtoaddress "$DAEMON_BTC_ADDR" 1.0 > /dev/null 2>&1
docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser=user -rpcpassword=pass \
    -rpcwallet=test generatetoaddress 1 "$DAEMON_BTC_ADDR" > /dev/null 2>&1

start_daemon "$DATA" 127.0.0.1:22525 127.0.0.1:22526 \
    --btc-regtest --btc-seed "$BTC_SEED" --btc-peer "127.0.0.1:$BTC_PORT"

# Wait for header sync + BIP-158 scan to complete.
for _ in $(seq 1 60); do
    STATUS=$(rpc GET http://127.0.0.1:22525/api/v1/btc/status 2>/dev/null || true)
    if echo "$STATUS" | grep -q '"synced":true'; then
        break
    fi
    sleep 1
done

echo "=== BTC status ==="
echo "$STATUS"

BALANCE=$(jq_num "$STATUS" balance_satoshis)
if [[ -z "$BALANCE" || "$BALANCE" == "0" ]]; then
    echo "FAIL: BIP-158 scan did not discover the funded output" >&2
    exit 1
fi

echo "PASS: BIP-158 scan discovered $BALANCE satoshis"
