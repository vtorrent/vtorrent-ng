#!/bin/sh
# The forced command must run only the readiness check, whatever command the
# SSH client asked for. Any requested command arrives in SSH_ORIGINAL_COMMAND;
# the wrapper must never evaluate it.
set -eu

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

cat > "$root/monitor-ready.sh" <<'EOF'
#!/bin/sh
printf 'readiness check ran\n'
EOF
chmod +x "$root/monitor-ready.sh"
cp "$(dirname "$0")/../vtorrent-monitor-ready" "$root/vtorrent-monitor-ready"
chmod +x "$root/vtorrent-monitor-ready"

marker="$root/escaped"
out=$(SSH_ORIGINAL_COMMAND="sh -c 'printf escaped > $marker'" \
    sh "$root/vtorrent-monitor-ready")

[ "$out" = 'readiness check ran' ] || { echo "unexpected output: $out" >&2; exit 1; }
[ ! -e "$marker" ] || { echo 'requested command was executed' >&2; exit 1; }

out=$(SSH_ORIGINAL_COMMAND='rm -rf /' sh "$root/vtorrent-monitor-ready")
[ "$out" = 'readiness check ran' ] || { echo "unexpected output: $out" >&2; exit 1; }

printf 'forced-command wrapper ignores SSH_ORIGINAL_COMMAND\n'
