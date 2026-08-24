#!/usr/bin/env bash
# Soak test status: height / peers / memory for the 3-node docker testnet.
# Usage: scripts/soak-status.sh

set -euo pipefail

NODES=("22625:vtr-node1" "22627:vtr-node2" "22629:vtr-node3")

for entry in "${NODES[@]}"; do
    port="${entry%%:*}"
    name="${entry##*:}"
    info=$(curl -s --max-time 3 "http://127.0.0.1:${port}/api/v1/info" || true)
    peers=$(curl -s --max-time 3 "http://127.0.0.1:${port}/api/v1/peers" | grep -o '"count":[0-9]*' | cut -d: -f2)
    if [[ -z "$info" ]]; then
        echo "${name} :${port}  DOWN"
        continue
    fi
    height=$(echo "$info" | grep -o '"block_height":[0-9]*' | cut -d: -f2)
    conns=$(echo "$info" | grep -o '"connections":[0-9]*' | cut -d: -f2)
    mem=$(docker stats --no-stream --format '{{.MemUsage}}' "$name" 2>/dev/null | awk '{print $1}' || true)
    echo "${name} :${port}  height=${height}  connections=${conns}  peers_seen=${peers:-?}  mem=${mem:-?}"
done

echo "────"
docker ps --filter name=vtr-node --filter name=vtr-btc-regtest --filter name=vtr-prometheus --filter name=vtr-grafana \
    --format '{{.Names}}: {{.Status}}'
