#!/usr/bin/env bash
# Observe an isolated release-candidate fleet for the exact-image soak.
#
#   SOAK_NAME=review-9533a2c SOAK_RPC_BASE=23950 \
#   SOAK_API_KEY_FILE=target/review-9533a2c-secrets/api-key \
#   SOAK_EVIDENCE=/mnt/temp/vtorrent-ng-evidence/soak-9533a2c \
#   SOAK_NTFY_URL=https://ntfy.sh/... SOAK_DEADMAN_URL=https://hc-ping.com/... \
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

    # A daily point-in-time sample cannot see a node that was down between
    # runs. Query the whole window at 5-minute resolution and reject any down
    # sample or a coverage gap (Prometheus itself unavailable / data lost).
    range_json="$(curl -fsS --max-time 20 --get \
        --data-urlencode 'query=up{job="vtorrent-daemon"}' \
        --data-urlencode "start=${window_start}" \
        --data-urlencode "end=${now_epoch}" \
        --data-urlencode 'step=300' \
        "${prom}/api/v1/query_range" 2>/dev/null)" || range_json='{"data":{"result":[]}}'
    range="$(python3 -c '
import json, sys
start, end, step = int(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3])
result = json.load(sys.stdin)["data"]["result"]
series = len(result)
expected = ((end - start) // step + 1) * series
values = [v for s in result for _, v in s["values"]]
down = sum(1 for v in values if v == "0")
print(series, expected, len(values), down, expected - len(values))
' "$window_start" "$now_epoch" 300 <<<"$range_json" 2>/dev/null || echo "0 0 0 0 0")"
    read -r series expected got down gap <<<"$range"
    echo "- Prometheus range (\`${since_iso}\` to \`${now_iso}\`, 5 min step): series \`${series}\`, samples \`${got}/${expected}\`, down \`${down}\`, missing \`${gap}\`."
    if [[ "$series" != "3" ]]; then
        echo "- Prometheus range is incomplete: expected 3 candidate series."
        fail=1
    fi
    if (( down > 0 )); then
        echo "- **Down samples detected in the window**; this observation fails."
        fail=1
    fi
    if (( expected > 0 && got * 100 < expected * 95 )); then
        echo "- **Coverage gap exceeds 5%** (Prometheus unavailable or data lost)."
        fail=1
    fi
    echo "- Container restarts must all be \`0\`; any nonzero exits this check."
} >> "$out"

if [[ "$fail" -ne 0 ]]; then
    if [[ -n "${SOAK_NTFY_URL:-}" ]]; then
        curl -fsS -m 10 \
            -H "Title: vTorrent candidate soak observation failed" \
            -H "Priority: high" \
            -H "Tags: warning" \
            -d "candidate=${name}; see ${out}" \
            "$SOAK_NTFY_URL" >/dev/null || true
    fi
else
    date +%s > "$evidence/last-observation-epoch"
    if [[ -n "${SOAK_DEADMAN_URL:-}" ]]; then
        # A successful run pings the external dead-man check. If the cron stops
        # running, the missing ping is what raises the alert.
        curl -fsS -m 10 "$SOAK_DEADMAN_URL" >/dev/null || true
    fi
fi

sed -n "/candidate observation (${name}, +${elapsed}h)/,\$p" "$out" | tail -20
exit "$fail"
