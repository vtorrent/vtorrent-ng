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
exit "$status"
