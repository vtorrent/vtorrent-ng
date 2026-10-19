#!/usr/bin/env bash
# Daily soak observation wrapper. Appends a timestamped markdown block to
# ~/.local/state/vtr-soak/observations.md and exits with the observation's
# status so cron can record failures.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE="${VTR_SOAK_STATE:-$HOME/.local/state/vtr-soak}"
WINDOW_START="${VTR_SOAK_WINDOW_START:-1792400791}"
mkdir -p "$STATE"
OUT="$STATE/observations.md"

printf '\n<!-- soak observation %s window_start=%s -->\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$WINDOW_START" >> "$OUT"
set +e
"$REPO/scripts/soak-observe.sh" "$WINDOW_START" >> "$OUT" 2>&1
status=$?
set -e
printf '<!-- exit=%s -->\n' "$status" >> "$OUT"

if [ "$status" -ne 0 ]; then
    printf 'failed %s exit=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status" \
        >> "$STATE/failures.log"
    if [ -n "${VTR_SOAK_NTFY_URL:-}" ]; then
        curl -fsS -m 10 \
            -H "Title: vTorrent soak observation failed" \
            -H "Priority: high" \
            -H "Tags: warning" \
            -d "exit=$status; see $OUT" \
            "$VTR_SOAK_NTFY_URL" >/dev/null || true
    fi
fi
exit "$status"
