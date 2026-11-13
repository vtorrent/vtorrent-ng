#!/usr/bin/env bash
# Install the dead-man heartbeat on seed1.
#
# The ping URL is a credential: it is written through an scp'd 0600 file rather
# than argv, so it never appears in a process list on either host.
#
# Usage:
#   scripts/install-heartbeat.sh --ping-url 'https://hc-ping.com/<uuid>' \
#       [--fail-url 'https://hc-ping.com/<uuid>/fail'] [--host root@91.98.80.38]
#
# Verification is manual and is printed at the end: disable the timer for one
# period and confirm exactly one alert, then re-enable and confirm recovery.

set -euo pipefail

HOST=root@91.98.80.38
PING_URL=
FAIL_URL=
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

while [ $# -gt 0 ]; do
    case "$1" in
        --ping-url) PING_URL=$2; shift 2 ;;
        --fail-url) FAIL_URL=$2; shift 2 ;;
        --host) HOST=$2; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -n "$PING_URL" ] || { echo "error: --ping-url is required" >&2; exit 2; }
case "$PING_URL${FAIL_URL:+$FAIL_URL}" in
    *[!A-Za-z0-9:/?=._~%+-]*) echo "error: URL contains an unsupported character" >&2; exit 2 ;;
esac

echo "== readiness on $HOST"
ssh -o BatchMode=yes "$HOST" '
    set -e
    curl -fsS --max-time 5 http://127.0.0.1:9090/-/ready >/dev/null
    curl -fsS --max-time 5 http://127.0.0.1:9093/-/ready >/dev/null
    systemctl is-active --quiet ntfy-relay
    echo "prometheus, alertmanager and relay are ready"
'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
umask 077
{
    printf 'DEADMAN_PING_URL=%s\n' "$PING_URL"
    [ -n "$FAIL_URL" ] && printf 'DEADMAN_FAIL_URL=%s\n' "$FAIL_URL"
} > "$tmp/heartbeat.env"

echo "== install"
scp -q "$REPO_ROOT/deploy/seeds-monitoring/heartbeat.sh" "$HOST:/usr/local/sbin/vtorrent-heartbeat"
scp -q "$REPO_ROOT/deploy/seeds-monitoring/monitor-ready.sh" "$HOST:/usr/local/sbin/monitor-ready.sh"
scp -q "$REPO_ROOT/deploy/seeds-monitoring/systemd/vtorrent-heartbeat.service" \
    "$HOST:/etc/systemd/system/vtorrent-heartbeat.service"
scp -q "$REPO_ROOT/deploy/seeds-monitoring/systemd/vtorrent-heartbeat.timer" \
    "$HOST:/etc/systemd/system/vtorrent-heartbeat.timer"
scp -q "$tmp/heartbeat.env" "$HOST:/etc/vtorrent/heartbeat.env"

ssh -o BatchMode=yes "$HOST" '
    set -e
    chmod 0755 /usr/local/sbin/vtorrent-heartbeat /usr/local/sbin/monitor-ready.sh
    chmod 0600 /etc/vtorrent/heartbeat.env
    systemctl daemon-reload
    systemctl enable --now vtorrent-heartbeat.timer
    systemctl start vtorrent-heartbeat.service
    systemctl is-active vtorrent-heartbeat.timer
    systemctl show vtorrent-heartbeat.service -p Result --value
'

cat <<EOF

Installed. Verify, then trust it:

1. Confirm a success ping arrived at the dead-man service (its dashboard shows
   a check less than 5 minutes old).
2. Prove the failure path:
     ssh $HOST 'systemctl stop vtorrent-heartbeat.timer'
   wait past the service's grace period, confirm exactly one alert.
3. Restore and confirm the recovery notification:
     ssh $HOST 'systemctl start vtorrent-heartbeat.timer'
4. Record the result in docs/soak-log.md.

Remove with: ssh $HOST 'systemctl disable --now vtorrent-heartbeat.timer'
EOF
