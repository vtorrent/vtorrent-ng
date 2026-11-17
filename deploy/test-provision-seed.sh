#!/usr/bin/env bash
# End-to-end test for deploy/provision-seed.sh in a disposable container.
#
# Runs the exact installer with stubbed service management, then checks that the
# service account can read its RPC key, the unit uses it, the staging area is
# cleaned up, and a digest mismatch leaves the installed binary untouched.
#
# This does not start the real daemon or exercise systemd; it tests the
# deployment logic that decides which binary and key the service will use.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
image="${VTR_TEST_IMAGE:-ubuntu:22.04}"
mkdir -p "$repo_root/target"
work=$(mktemp -d "$repo_root/target/provision-test.XXXXXX")
trap 'rm -rf "$work"' EXIT

cat >"$work/vtorrent-daemon" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 0755 "$work/vtorrent-daemon"
digest=$(sha256sum "$work/vtorrent-daemon" | cut -d' ' -f1)

mkdir "$work/stubs"
cat >"$work/stubs/systemctl" <<'EOF'
#!/bin/sh
case "${1:-}" in is-active) echo active ;; esac
exit 0
EOF
cat >"$work/stubs/ufw" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$work/stubs/ss" <<'EOF'
#!/bin/sh
echo 'LISTEN 0 128 0.0.0.0:22526 0.0.0.0:*'
EOF
cat >"$work/stubs/curl" <<'EOF'
#!/bin/sh
echo '{"height":1}'
EOF
chmod 0755 "$work"/stubs/*

docker run --rm \
  -v "$repo_root/deploy/provision-seed.sh:/opt/provision-seed.sh:ro" \
  -v "$work/vtorrent-daemon:/tmp/vtorrent-daemon:ro" \
  -v "$work/stubs:/opt/stubs:ro" \
  -e VTR_DAEMON_SHA256="$digest" \
  "$image" bash -euo pipefail -c '
    export PATH=/opt/stubs:$PATH
    bash /opt/provision-seed.sh 203.0.113.10
    test -f /etc/vtorrent/rpc-api-key
    test "$(stat -c %a /etc/vtorrent/rpc-api-key)" = 600
    test "$(stat -c %U /etc/vtorrent/rpc-api-key)" = vtorrent
    test "$(stat -c %a /etc/vtorrent)" = 700
    runuser -u vtorrent -- test -r /etc/vtorrent/rpc-api-key
    grep -q -- "--rpc-api-key-file /etc/vtorrent/rpc-api-key" \
      /etc/systemd/system/vtorrent.service
    if ls /run | grep -q vtorrent-provision; then
      echo "staging directory was left behind" >&2
      exit 1
    fi
    installed=$(sha256sum /usr/local/bin/vtorrent-daemon | cut -d" " -f1)
    if VTR_DAEMON_SHA256=deadbeef bash /opt/provision-seed.sh 203.0.113.10; then
      echo "digest mismatch was accepted" >&2
      exit 1
    fi
    test "$(sha256sum /usr/local/bin/vtorrent-daemon | cut -d" " -f1)" = "$installed"
    echo "provision-seed checks passed"
  '
