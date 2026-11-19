#!/usr/bin/env bash
# Observe an isolated release-candidate fleet for the exact-image soak.
#
#   SOAK_NAME=review-9533a2c SOAK_RPC_BASE=23950 \
#   SOAK_API_KEY_FILE=target/review-9533a2c-secrets/api-key \
#   SOAK_EVIDENCE=/mnt/temp/vtorrent-ng-evidence/soak-9533a2c \
#   scripts/soak-observe-candidate.sh [window_start_epoch]
#
# Read-only. Appends a markdown block to $SOAK_EVIDENCE/observations.md and
# exits non-zero if a node is down, tips disagree, or a container restarted.
set -euo pipefail

name="${SOAK_NAME:?set SOAK_NAME}"
rpc_base="${SOAK_RPC_BASE:?set SOAK_RPC_BASE}"
key_file="${SOAK_API_KEY_FILE:?set SOAK_API_KEY_FILE}"
evidence="${SOAK_EVIDENCE:?set SOAK_EVIDENCE}"
window_start="${1:-${SOAK_WINDOW_START:-}}"
if [[ -z "$window_start" ]]; then
    echo "set a window start epoch or pass it as the first argument" >&2
    exit 2
fi
api_key="$(cat "$key_file")"
prom="${PROM_URL:-http://127.0.0.1:23973}"

now_epoch=$(date +%s)
now_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
since_iso=$(date -u -d "@${window_start}" +%Y-%m-%dT%H:%M:%SZ)
elapsed=$(( (now_epoch - window_start) / 3600 ))

fail=0
declare -a heights=()
declare -a hashes=()
out="$evidence/observations.md"
mkdir -p "$evidence"

{
    echo "## $(date -u +%Y-%m-%d) — candidate observation (${name}, +${elapsed}h)"
    echo
    echo "Read-only check at \`${now_iso}\` (window start \`${since_iso}\`)."
    echo
    echo "| Node | Height | Peers | Staking | BTC height | BTC synced | RSS | Restarts |"
    echo "|---|---|---|---|---|---|---|---|"
} >> "$out"

for i in 1 2 3; do
    port=$((rpc_base + i))
    container="${name}-${i}"
    info="$(curl -fsS --max-time 10 -H "X-Api-Key: $api_key" \
        "http://127.0.0.1:${port}/api/v1/info")" || { fail=1; info='{}'; }
    height="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("block_height","?"))' <<<"$info")"
    peers="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("connections","?"))' <<<"$info")"
    hash="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("best_block_hash",""))' <<<"$info")"
    heights+=("$height")
    hashes+=("$hash")

    staking="$(curl -fsS --max-time 10 -H "X-Api-Key: $api_key" \
        "http://127.0.0.1:${port}/api/v1/staking/status")" || { fail=1; staking='{}'; }
    staking_state="$(python3 -c 'import json,sys; d=json.load(sys.stdin); print("on" if d.get("enabled") else "off")' <<<"$staking")"
    btc="$(curl -fsS --max-time 10 -H "X-Api-Key: $api_key" \
        "http://127.0.0.1:${port}/api/v1/btc/status")" || { fail=1; btc='{}'; }
    btc_height="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("best_height","?"))' <<<"$btc")"
    btc_synced="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("synced","?"))' <<<"$btc")"
    rss="$(docker stats --no-stream --format '{{.MemUsage}}' "$container" 2>/dev/null | cut -d/ -f1 | tr -d ' ' || echo '?')"
    restarts="$(docker inspect -f '{{.RestartCount}}' "$container" 2>/dev/null || echo '?')"
    [[ "$restarts" == "0" ]] || fail=1

    printf '| %s | %s | %s | %s | %s | %s | %s | %s |\n' \
        "$container" "$height" "$peers" "$staking_state" "$btc_height" \
        "$btc_synced" "$rss" "$restarts" >> "$out"
done

{
    echo
    if [[ "${heights[0]}" == "${heights[1]}" && "${heights[1]}" == "${heights[2]}" \
        && "${hashes[0]}" == "${hashes[1]}" && "${hashes[1]}" == "${hashes[2]}" \
        && -n "${hashes[0]}" ]]; then
        echo "- Tip agreement: **yes** at height \`${heights[0]}\`."
    else
        echo "- Tip agreement: **NO** (heights ${heights[*]})."
        fail=1
    fi
    up="$(curl -fsS --max-time 10 --get --data-urlencode 'query=up{job="vtorrent-daemon"}' \
        "${prom}/api/v1/query" 2>/dev/null \
        | python3 -c 'import json,sys; r=json.load(sys.stdin)["data"]["result"]; print(sum(1 for x in r if x["value"][1]=="1"))' \
        2>/dev/null || echo 0)"
    echo "- Prometheus targets up: \`${up}/3\`."
    [[ "$up" == "3" ]] || fail=1
    echo "- Container restarts must all be \`0\`; any nonzero exits this check."
} >> "$out"

sed -n "/candidate observation (${name}, +${elapsed}h)/,\$p" "$out" | tail -20
exit "$fail"
