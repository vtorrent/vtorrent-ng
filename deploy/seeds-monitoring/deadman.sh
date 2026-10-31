#!/bin/sh
set -eu

: "${NTFY_TOPIC:?set NTFY_TOPIC}"
case "$NTFY_TOPIC" in *[!a-zA-Z0-9_-]*|'') echo 'Invalid ntfy topic' >&2; exit 2;; esac
state_dir=${DEADMAN_STATE_DIR:-"${XDG_STATE_HOME:-$HOME/.local/state}/vtorrent-deadman"}
umask 077
mkdir -p "$state_dir"
exec 9>"$state_dir/lock"
flock -n 9 || exit 0

if [ -n "${SEED1_SSH_TARGET:-}" ]; then
    : "${SEED1_SSH_KEY:?set SEED1_SSH_KEY}"
    : "${SEED1_KNOWN_HOSTS:?set SEED1_KNOWN_HOSTS}"
    case "$SEED1_SSH_TARGET" in -*) echo 'Invalid SSH target' >&2; exit 2;; esac
    if ssh -T -i "$SEED1_SSH_KEY" -o BatchMode=yes -o IdentitiesOnly=yes \
        -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$SEED1_KNOWN_HOSTS" \
        -o ConnectTimeout=5 "$SEED1_SSH_TARGET" /usr/local/libexec/vtorrent-monitor-ready; then
        healthy=1
    else
        healthy=0
    fi
else
    : "${SEED1_PROMETHEUS_URL:?set SEED1_SSH_TARGET or SEED1_PROMETHEUS_URL}"
    if curl -fsS --max-time 5 "$SEED1_PROMETHEUS_URL/-/ready" >/dev/null; then
        healthy=1
    else
        healthy=0
    fi
fi

notify() {
    printf 'url = "https://ntfy.sh/%s"\n' "$NTFY_TOPIC" | \
        curl --config - -fsS --max-time 10 -H "Title: $1" -d "$2" >/dev/null
}

if [ "$healthy" = 0 ] && [ ! -e "$state_dir/alerted" ]; then
    notify 'seed1 monitoring unreachable' 'Prometheus, Alertmanager, or the relay is unavailable.'
    : > "$state_dir/alerted"
elif [ "$healthy" = 1 ] && [ -e "$state_dir/alerted" ]; then
    notify 'seed1 monitoring recovered' 'The independent monitoring check is healthy again.'
    rm -f "$state_dir/alerted"
fi
