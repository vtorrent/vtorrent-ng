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

# ── Host readiness: exactly one Docker daemon ────────────────────────────────
# A second daemon (e.g. an apt-installed docker.service alongside the snap)
# can take over /run/docker.sock, leaving the CLI blind to the real containers
# while they keep running. This happened on 2026-09-21.
DOCKERD_COUNT=$(pgrep -c -x dockerd 2>/dev/null || echo 0)
if [ "$DOCKERD_COUNT" -ne 1 ]; then
    echo "WARNING: $DOCKERD_COUNT dockerd processes running (expected 1)."
    echo "         A second daemon may own /run/docker.sock; container state below may be wrong."
    pgrep -a -x dockerd 2>/dev/null | sed 's/^/         /'
fi
if systemctl is-enabled docker.service >/dev/null 2>&1; then
    echo "WARNING: system docker.service is enabled; it can conflict with snap.docker.dockerd."
fi

# ── Host readiness: no suspend since boot ────────────────────────────────────
# A lid-close suspend freezes every container without killing it, so the fleet
# stalls and Prometheus scrapes simply stop (no zeros). CLOCK_BOOTTIME includes
# suspended time while CLOCK_MONOTONIC does not, so their difference is the
# total time the host has slept since boot. This happened on 2026-09-20 (16.7h
# freeze). Falls back to the /proc/uptime-vs-btime gap if python3 is missing.
SUSPEND_GAP=$(python3 -c 'import time; print(int(time.clock_gettime(time.CLOCK_BOOTTIME) - time.clock_gettime(time.CLOCK_MONOTONIC)))' 2>/dev/null || echo "")
if [ -z "$SUSPEND_GAP" ]; then
    BTIME=$(awk '/^btime/{print $2}' /proc/stat 2>/dev/null || echo 0)
    if [ "$BTIME" -gt 0 ]; then
        SUSPEND_GAP=$(( $(date +%s) - BTIME - $(cut -d. -f1 /proc/uptime) ))
    fi
fi
if [ -n "$SUSPEND_GAP" ] && [ "$SUSPEND_GAP" -gt 120 ]; then
    echo "WARNING: host suspended ~$(( SUSPEND_GAP / 60 )) min since boot (boottime/monotonic gap)."
    echo "         Containers were frozen during that time; soak evidence is interrupted."
fi
