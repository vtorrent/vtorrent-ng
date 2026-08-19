#!/bin/bash
# Shared helpers for vTorrent regtest smoke tests.
#
# Source this file from a smoke test script. It provides:
#   - DAEMON_BIN: path to the built daemon binary
#   - start_daemon / stop_daemon: lifecycle helpers with cleanup traps
#   - wait_for_rpc: poll an RPC endpoint until it responds
#   - rpc: curl wrapper for JSON-RPC calls
#   - jq_field: extract a JSON field without jq (grep/sed fallback)

set -euo pipefail

# Resolve the repo root (this file lives in scripts/smoke/).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DAEMON_BIN="${REPO_ROOT}/target/debug/vtorrent-daemon"

# Test key material (deterministic, regtest-only).
MAKER_WIF="WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS"
MAKER_ADDR="VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k"
TAKER_WIF="WHorCxM7fSQDwTd6W19pdTjSH2nAHrgwEULonwbkDkDw7TdSxTVU"
TAKER_ADDR="VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh"

# PIDs of daemons started by this script, cleaned up on exit.
declare -a DAEMON_PIDS=()

cleanup() {
    for pid in "${DAEMON_PIDS[@]:-}"; do
        kill -9 "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT

# Start a daemon in the background and record its PID.
# Usage: start_daemon <data-dir> <rpc-addr> <listen-addr> [extra args...]
start_daemon() {
    local data_dir="$1" rpc_addr="$2" listen_addr="$3"
    shift 3
    mkdir -p "$data_dir"
    "$DAEMON_BIN" \
        --regtest \
        --data-dir "$data_dir" \
        --rpc-addr "$rpc_addr" \
        --listen "$listen_addr" \
        --no-dht \
        --log-level info \
        "$@" > "$data_dir/daemon.log" 2>&1 &
    DAEMON_PIDS+=("$!")
}

# Poll an RPC endpoint until it returns a non-empty body (or timeout).
# Usage: wait_for_rpc <url> [timeout_secs]
wait_for_rpc() {
    local url="$1" timeout="${2:-30}"
    for _ in $(seq 1 "$timeout"); do
        if curl -s --max-time 2 "$url" 2>/dev/null | grep -q .; then
            return 0
        fi
        sleep 1
    done
    return 1
}

# curl wrapper for a JSON-RPC call.
rpc() {
    local method="$1" url="$2" data="${3:-}"
    if [[ -n "$data" ]]; then
        curl -s --max-time 10 -X "$method" "$url" -H 'Content-Type: application/json' -d "$data"
    else
        curl -s --max-time 10 -X "$method" "$url"
    fi
}

# Extract a JSON string field without jq.
# Usage: jq_field <json> <field>
jq_field() {
    local json="$1" field="$2"
    echo "$json" | grep -o "\"$field\":\"[^\"]*\"" | head -1 | cut -d'"' -f4
}

# Extract a JSON numeric field without jq.
# Usage: jq_num <json> <field>
jq_num() {
    local json="$1" field="$2"
    echo "$json" | grep -o "\"$field\":[0-9]*" | head -1 | cut -d: -f2
}
