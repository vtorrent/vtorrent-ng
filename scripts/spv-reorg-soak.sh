#!/usr/bin/env bash
set -euo pipefail
# Spv reorg + staking soak harness — 4 phases per spec §6.4
# Reuses docker/testnet/docker-compose.yml 3-node mesh + Grafana localhost:3300

QUICK=0
SINGLE=0
for arg in "$@"; do case "$arg" in --quick) QUICK=1;; --single-node-smoke) SINGLE=1;; --dry-run) echo "dry-run ok"; exit 0;; esac; done

docker_container_running() {
  command -v docker >/dev/null 2>&1 &&
    docker ps --format '{{.Names}}' 2>/dev/null | grep -qx "$1"
}

echo "[spv-soak] phase A: producer/full-node commitment parity"
cargo test -p vtorrent-node staking::root_parity_tests -- --nocapture

if [ "$SINGLE" -eq 1 ]; then echo "[spv-soak] single-node smoke done"; exit 0; fi

echo "[spv-soak] phase B: partition node2 5 min (simulated via docker exec iptables if compose up)"
if docker_container_running vtr-node2; then
  echo "[spv-soak] partitioning vtr-node2"
  docker exec vtr-node2 iptables -A INPUT -p tcp --dport 22526 -j DROP || true
  if [ "$QUICK" -eq 1 ]; then sleep 10; else sleep 300; fi
  docker exec vtr-node2 iptables -F || true
  echo "[spv-soak] partition healed"
else
  if [ "$QUICK" -eq 1 ]; then
    echo "[spv-soak] compose not up — quick local checks only"
  else
    echo "[spv-soak] compose is not running; cannot execute partition soak" >&2
    exit 2
  fi
fi

if docker_container_running vtr-node1; then
  echo "[spv-soak] phase C: require all nodes to converge"
  python3 scripts/check_spv_tip.py --expect-converged
fi

echo "[spv-soak] phase D: forged SPV state commitment must fail closed"
cargo test -p vtorrent-spv test_utxo_root_forgery_fails_closed_immediately -- --nocapture

echo "[spv-soak] done — check Grafana http://localhost:3300 and scripts/soak-status.sh"
