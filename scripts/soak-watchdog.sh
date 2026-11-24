#!/usr/bin/env bash
# Fail and alert when the candidate soak observation is stale.
#
# Run this from a separate cron entry (for example hourly) from the
# observation job. It catches a missed or failed observation even when the
# observation job itself exits before writing. A whole-host outage still needs
# the external SOAK_DEADMAN_URL; this watchdog is the local complement.
#
#   SOAK_EVIDENCE=/mnt/temp/vtorrent-ng-evidence/soak-<commit> \
#   SOAK_NTFY_URL=https://ntfy.sh/... \
#   scripts/soak-watchdog.sh
set -euo pipefail

evidence="${SOAK_EVIDENCE:?set SOAK_EVIDENCE}"
max_hours="${SOAK_MAX_AGE_HOURS:-12}"
if ! [[ "$max_hours" =~ ^[0-9]+$ ]] || [ "$max_hours" -lt 1 ]; then
    echo "SOAK_MAX_AGE_HOURS must be a positive integer" >&2
    exit 2
fi

stamp_file="$evidence/last-observation-epoch"
now=$(date +%s)
if [ -f "$stamp_file" ]; then
    stamp=$(cat "$stamp_file")
    if ! [[ "$stamp" =~ ^[0-9]+$ ]]; then
        echo "invalid timestamp in $stamp_file" >&2
        stamp=0
    fi
    age=$((now - stamp))
else
    stamp=0
    age=999999999
fi

if [ "$age" -le $((max_hours * 3600)) ]; then
    echo "candidate soak observation is current (${age}s old)"
    exit 0
fi

message="candidate soak observation in ${evidence} is stale: ${age}s old (max ${max_hours}h)"
echo "$message" >&2
if [ -n "${SOAK_NTFY_URL:-}" ]; then
    curl -fsS -m 10 \
        -H "Title: vTorrent candidate soak observation stale" \
        -H "Priority: high" \
        -H "Tags: warning" \
        -d "$message" \
        "$SOAK_NTFY_URL" >/dev/null || true
fi
exit 1
