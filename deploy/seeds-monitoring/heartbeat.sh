#!/bin/sh
# Heartbeat for the external dead-man service.
#
# seed1 pings the external monitor only while its own monitoring stack is
# ready. Silence is the alarm: if seed1 dies, loses network, or Prometheus,
# Alertmanager, or the ntfy relay stops being healthy, the pings stop and the
# service raises the alert. That covers the one failure nothing else on seed1
# can report — seed1 itself being gone.
#
# DEADMAN_PING_URL is the service's success URL (for example
# https://hc-ping.com/<uuid>). DEADMAN_FAIL_URL is optional; when set it is hit
# once when local readiness fails, so the service can report the reason instead
# of a bare timeout.
#
# Secrets stay out of argv: the URL is passed to curl on stdin via --config.

set -eu

: "${DEADMAN_PING_URL:?set DEADMAN_PING_URL to the monitor's success URL}"

check_url() {
    case "$1" in
        *[!A-Za-z0-9:/?=._~%+-]*) echo 'Invalid URL character' >&2; exit 2 ;;
    esac
    case "$1" in
        http://*|https://*) ;;
        *) echo 'Invalid URL: must start with http:// or https://' >&2; exit 2 ;;
    esac
}

check_url "$DEADMAN_PING_URL"
if [ -n "${DEADMAN_FAIL_URL:-}" ]; then
    check_url "$DEADMAN_FAIL_URL"
fi

if "$(dirname "$0")/monitor-ready.sh"; then
    url=$DEADMAN_PING_URL
else
    if [ -z "${DEADMAN_FAIL_URL:-}" ]; then
        echo 'Monitoring is not ready and DEADMAN_FAIL_URL is unset' >&2
        exit 1
    fi
    url=$DEADMAN_FAIL_URL
fi

printf 'url = "%s"\n' "$url" | curl --config - -fsS --max-time 10 >/dev/null
