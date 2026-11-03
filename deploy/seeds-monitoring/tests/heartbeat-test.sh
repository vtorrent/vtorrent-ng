#!/bin/sh
# Hermetic checks for heartbeat.sh: which URL is hit, and when nothing is sent.
# No network access and no running daemon are required.
set -eu

root=$(mktemp -d)
server_pid=
cleanup() {
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null || true
    rm -rf "$root"
}
trap cleanup EXIT

cat > "$root/server.py" <<'PY'
import http.server
import os
import sys

port_file, log_file = sys.argv[1], sys.argv[2]


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(log_file, 'a') as f:
            f.write(self.path + '\n')
        self.send_response(200)
        self.end_headers()

    def log_message(self, *args):
        pass


server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
with open(port_file + '.tmp', 'w') as f:
    f.write(str(server.server_address[1]))
os.replace(port_file + '.tmp', port_file)
server.serve_forever()
PY

: > "$root/requests.log"
python3 "$root/server.py" "$root/port" "$root/requests.log" >/dev/null 2>&1 &
server_pid=$!
for _ in $(seq 1 50); do
    [ -s "$root/port" ] && break
    sleep 0.1
done
[ -s "$root/port" ] || { echo 'stub server did not start' >&2; exit 1; }
port=$(cat "$root/port")

# monitor-ready.sh stands in for the readiness check; STUB_READY selects it.
cat > "$root/monitor-ready.sh" <<'SH'
#!/bin/sh
[ "${STUB_READY:-0}" = 1 ]
SH
chmod +x "$root/monitor-ready.sh"
cp "$(dirname "$0")/../heartbeat.sh" "$root/heartbeat.sh"
chmod +x "$root/heartbeat.sh"

ping_url="http://127.0.0.1:$port/ping/ok"
fail_url="http://127.0.0.1:$port/ping/fail"

# 1. Healthy stack: the success URL is hit exactly once.
: > "$root/requests.log"
STUB_READY=1 DEADMAN_PING_URL="$ping_url" DEADMAN_FAIL_URL="$fail_url" \
    sh "$root/heartbeat.sh"
[ "$(cat "$root/requests.log")" = '/ping/ok' ] || {
    echo "expected /ping/ok, got: $(cat "$root/requests.log")" >&2; exit 1; }

# 2. Unhealthy stack: the failure URL is hit instead, never the success one.
: > "$root/requests.log"
STUB_READY=0 DEADMAN_PING_URL="$ping_url" DEADMAN_FAIL_URL="$fail_url" \
    sh "$root/heartbeat.sh"
[ "$(cat "$root/requests.log")" = '/ping/fail' ] || {
    echo "expected /ping/fail, got: $(cat "$root/requests.log")" >&2; exit 1; }

# 3. Unhealthy stack with no failure URL: silence, non-zero exit, no request.
: > "$root/requests.log"
if STUB_READY=0 DEADMAN_PING_URL="$ping_url" sh "$root/heartbeat.sh" 2>/dev/null; then
    echo 'expected a non-zero exit without DEADMAN_FAIL_URL' >&2; exit 1
fi
[ -s "$root/requests.log" ] && { echo 'sent a request without a URL to send to' >&2; exit 1; }

# 4. A URL that would break out of the curl config is rejected before use.
: > "$root/requests.log"
if STUB_READY=1 DEADMAN_PING_URL='http://x/"injected"' sh "$root/heartbeat.sh" 2>/dev/null; then
    echo 'expected a rejection of an URL containing a quote' >&2; exit 1
fi
[ -s "$root/requests.log" ] && { echo 'sent a request for a rejected URL' >&2; exit 1; }

# 5. A missing ping URL is an error, not a silent no-op.
if STUB_READY=1 sh "$root/heartbeat.sh" 2>/dev/null; then
    echo 'expected a non-zero exit without DEADMAN_PING_URL' >&2; exit 1
fi

printf 'heartbeat sends the right URL and stays silent when it should\n'
