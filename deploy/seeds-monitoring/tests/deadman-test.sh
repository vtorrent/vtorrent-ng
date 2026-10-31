#!/bin/sh
set -eu
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir "$root/bin" "$root/state"
touch "$root/key" "$root/known_hosts"
cat > "$root/bin/ssh" <<'EOF'
#!/bin/sh
for last do :; done
[ "$last" = /usr/local/libexec/vtorrent-monitor-ready ] || exit 64
[ "$(cat "$MOCK_STATUS")" = up ]
EOF
cat > "$root/bin/curl" <<'EOF'
#!/bin/sh
cat >/dev/null
[ ! -e "$MOCK_CURL_FAIL" ] || exit 22
printf 'sent\n' >> "$MOCK_SENT"
EOF
chmod +x "$root/bin/ssh" "$root/bin/curl"
export PATH="$root/bin:$PATH"
export MOCK_STATUS="$root/status" MOCK_SENT="$root/sent"
export MOCK_CURL_FAIL="$root/curl-fail"
export SEED1_SSH_TARGET='deadman@seed1' SEED1_SSH_KEY="$root/key"
export SEED1_KNOWN_HOSTS="$root/known_hosts" NTFY_TOPIC='test-topic'
export DEADMAN_STATE_DIR="$root/state"
script="$(dirname "$0")/../deadman.sh"
printf 'down' > "$MOCK_STATUS"
sh "$script"
sh "$script"
[ "$(wc -l < "$MOCK_SENT")" -eq 1 ]
printf 'up' > "$MOCK_STATUS"
sh "$script"
sh "$script"
[ "$(wc -l < "$MOCK_SENT")" -eq 2 ]
printf 'down' > "$MOCK_STATUS"
touch "$MOCK_CURL_FAIL"
if sh "$script"; then echo 'failed notification must not succeed' >&2; exit 1; fi
[ ! -e "$DEADMAN_STATE_DIR/alerted" ]
rm "$MOCK_CURL_FAIL"
sh "$script"
[ "$(wc -l < "$MOCK_SENT")" -eq 3 ]
printf 'dead-man transition and deduplication test passed\n'
