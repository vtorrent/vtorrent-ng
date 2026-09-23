#!/usr/bin/env bash
# Daily soak observation: read-only evidence collection for the 7-day window.
#
# Usage: scripts/soak-observe.sh [window_start_epoch]
#
# Prints a markdown block ready to append to docs/soak-log.md. Read-only: it
# never restarts, redeploys, or writes to the fleet. The window start defaults
# to 2026-09-21T01:58:13Z (the first post-recovery stake; see docs/soak-log.md).
#
# Evidence collected:
#   - per-node height, tip hash agreement, syncing, connections, mempool
#   - staking status and blocks staked in the window
#   - block cadence (min/median/max) within the window
#   - Prometheus scrape coverage and gaps >30s since the window start
#   - ERROR/panic/reorg/rollback counts and container restart counts
#   - memory RSS per node
#   - BTC SPV status
#
# Exit code is non-zero if any node is down or the fleet disagrees on the tip,
# so it can gate a cron job.

set -euo pipefail

WINDOW_START="${1:-1790140560}" # 2026-09-23T05:16:00Z (first post-redeploy stake)
NODES=("22625:vtr-node1" "22627:vtr-node2" "22629:vtr-node3")
API_KEY="${VTORRENT_RPC_KEY:-testnet-soak-key}"
PROM="${PROM_URL:-http://127.0.0.1:9090}"

now_epoch=$(date +%s)
now_iso=$(date -u +%Y-%m-%dT%H:%MZ)
window_hours=$(( (now_epoch - WINDOW_START) / 3600 ))

fail=0
declare -A HEIGHTS
declare -A HASHES

echo "## $(date -u +%Y-%m-%d) — daily observation (window +${window_hours}h)"
echo
echo "Read-only check at \`${now_iso}\` (window start \`$(date -u -d "@${WINDOW_START}" +%Y-%m-%dT%H:%M:%SZ)\`)."
echo

# ── Per-node state ───────────────────────────────────────────────────────────
for entry in "${NODES[@]}"; do
    port="${entry%%:*}"
    name="${entry##*:}"
    info=$(curl -s --max-time 3 "http://127.0.0.1:${port}/api/v1/info" || true)
    if [[ -z "$info" ]]; then
        echo "- **${name} DOWN** (no response on :${port})"
        fail=1
        continue
    fi
    height=$(echo "$info" | grep -o '"block_height":[0-9]*' | cut -d: -f2)
    hash=$(echo "$info" | grep -o '"best_block_hash":"[0-9a-f]*"' | cut -d: -f2 | tr -d '"')
    conns=$(echo "$info" | grep -o '"connections":[0-9]*' | cut -d: -f2)
    syncing=$(echo "$info" | grep -o '"syncing":[a-z]*' | cut -d: -f2)
    mempool=$(echo "$info" | grep -o '"mempool_size":[0-9]*' | cut -d: -f2)
    HEIGHTS[$name]=$height
    HASHES[$name]=$hash
    mem=$(docker stats --no-stream --format '{{.MemUsage}}' "$name" 2>/dev/null | awk '{print $1}' || echo "?")
    echo "- ${name}: height **${height}**, hash \`${hash:0:16}…\`, syncing=${syncing}, connections=${conns}, mempool=${mempool}, mem=${mem}"
done
echo

# ── Tip agreement ────────────────────────────────────────────────────────────
# The nodes are queried sequentially, so a block propagating between two
# requests can show a transient one-height disagreement. Re-read once before
# declaring a real disagreement.
if [[ "$(printf '%s\n' "${HEIGHTS[@]}" | sort -u | wc -l)" -ne 1 ]]; then
    sleep 5
    for entry in "${NODES[@]}"; do
        port="${entry%%:*}"
        name="${entry##*:}"
        info=$(curl -s --max-time 3 "http://127.0.0.1:${port}/api/v1/info" || true)
        [[ -z "$info" ]] && continue
        HEIGHTS[$name]=$(echo "$info" | grep -o '"block_height":[0-9]*' | cut -d: -f2)
        HASHES[$name]=$(echo "$info" | grep -o '"best_block_hash":"[0-9a-f]*"' | cut -d: -f2 | tr -d '"')
    done
fi
unique_heights=$(printf '%s\n' "${HEIGHTS[@]}" | sort -u | wc -l)
unique_hashes=$(printf '%s\n' "${HASHES[@]}" | sort -u | wc -l)
if [[ "$unique_heights" -eq 1 && "$unique_hashes" -eq 1 ]]; then
    echo "All three nodes agree on height and tip hash."
else
    echo "**DISAGREEMENT**: ${unique_heights} distinct heights, ${unique_hashes} distinct hashes."
    fail=1
fi
echo

# ── Staking ──────────────────────────────────────────────────────────────────
staking=$(curl -s --max-time 3 -H "X-Api-Key: ${API_KEY}" \
    "http://127.0.0.1:22625/api/v1/staking/status" || true)
if [[ -n "$staking" ]]; then
    enabled=$(echo "$staking" | grep -o '"enabled":[a-z]*' | cut -d: -f2)
    utxos=$(echo "$staking" | grep -o '"eligible_utxos":[0-9]*' | cut -d: -f2)
    staked=$(echo "$staking" | grep -o '"blocks_staked":[0-9]*' | cut -d: -f2)
    echo "- Staking enabled=${enabled}, eligible UTXOs=${utxos}, blocks staked this run=${staked}."
fi
echo

# ── Block cadence within the window ──────────────────────────────────────────
cadence=$(docker logs --since "${window_hours}h" vtr-node1 2>&1 \
    | sed 's/\x1b\[[0-9;]*m//g' \
    | grep 'Block accepted' \
    | sed -E 's/.*height=([0-9]+).*timestamp=([0-9]+).*/\1 \2/' \
    | sort -n -k1 -u \
    | python3 -c "
import sys, statistics
rows=[l.split() for l in sys.stdin if l.strip()]
seen={}
for h,t in rows: seen[int(h)]=int(t)
ws=${WINDOW_START}
hs=[h for h in sorted(seen) if seen[h]>=ws]
ts=[seen[h] for h in hs]
diffs=[ts[i+1]-ts[i] for i in range(len(ts)-1)]
if diffs:
    slow=sum(1 for d in diffs if d>120)
    print(f'n={len(diffs)} min={min(diffs)} median={statistics.median(diffs)} max={max(diffs)} mean={round(statistics.mean(diffs),1)} slow(>120s)={slow}')
else:
    print('no blocks in window')
" 2>/dev/null || echo "unavailable")
echo "- Block cadence within the window: ${cadence}."
echo

# ── Prometheus coverage ──────────────────────────────────────────────────────
# Note: the sample at exactly WINDOW_START can read down=1 on nodes that were
# still finishing recovery at that instant; it is a boundary artifact, not an
# in-window outage. Check the reported timestamps before treating it as one.
coverage=$(curl -s --max-time 5 "${PROM}/api/v1/query_range?query=up%7Bjob%3D%22vtorrent-nodes%22%7D&start=${WINDOW_START}&end=${now_epoch}&step=15s" \
    | python3 -c "
import json,sys
d=json.load(sys.stdin)
expected=(${now_epoch}-${WINDOW_START})//15+1
for r in d['data']['result']:
    vals=r['values']
    downs=sum(1 for _,v in vals if float(v)==0)
    gaps=0; prev=None
    for ts,_ in vals:
        if prev is not None and float(ts)-prev>30: gaps+=1
        prev=float(ts)
    print(f\"  - {r['metric']['instance']}: {len(vals)}/{expected} samples, down={downs}, gaps>30s={gaps}\")
" 2>/dev/null || echo "  - Prometheus unavailable")
echo "- Prometheus since window start:"
echo "$coverage"
echo

# ── Errors and restarts ──────────────────────────────────────────────────────
echo "- Errors since window start:"
for entry in "${NODES[@]}"; do
    name="${entry##*:}"
    errs=$(docker logs --since "${window_hours}h" "$name" 2>&1 \
        | sed 's/\x1b\[[0-9;]*m//g' \
        | grep -cE ' ERROR |panic|Reorg|reorg|rollback' || true)
    restarts=$(docker inspect "$name" --format '{{.RestartCount}}' 2>/dev/null || echo "?")
    echo "  - ${name}: errors=${errs}, restarts=${restarts}"
done
echo

# ── BTC SPV ──────────────────────────────────────────────────────────────────
btc=$(curl -s --max-time 3 -H "X-Api-Key: ${API_KEY}" \
    "http://127.0.0.1:22625/api/v1/btc/status" || true)
if [[ -n "$btc" ]]; then
    btc_height=$(echo "$btc" | grep -o '"best_height":[0-9]*' | cut -d: -f2)
    btc_synced=$(echo "$btc" | grep -o '"synced":[a-z]*' | cut -d: -f2)
    echo "- BTC SPV: height=${btc_height}, synced=${btc_synced}."
fi
echo
echo "Earliest sign-off: **2026-09-30 after 05:16Z**. Next daily observation due $(date -u -d 'tomorrow' +%Y-%m-%d)."

exit "$fail"
