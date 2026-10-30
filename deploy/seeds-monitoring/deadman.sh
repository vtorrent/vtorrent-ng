#!/bin/sh
set -eu
: "${SEED1_PROMETHEUS_URL:?set SEED1_PROMETHEUS_URL}"
: "${NTFY_TOPIC:?set NTFY_TOPIC}"
if ! curl -fsS --max-time 5 "$SEED1_PROMETHEUS_URL/-/ready" >/dev/null; then
    curl -fsS --max-time 10 \
        -H "Title: seed1 monitoring unreachable" \
        -d "Prometheus on seed1 is not ready; alerting may be down." \
        "https://ntfy.sh/$NTFY_TOPIC" >/dev/null
fi
