#!/usr/bin/env bash
# Provision a vTorrent mainnet seed node from a fresh Ubuntu 22.04 box.
#
# Usage:
#   deploy/provision-seed.sh <seed-peer-ip> [binary-path]
#
#   <seed-peer-ip>  IP of an existing seed to bootstrap from (e.g. 91.98.80.38)
#   [binary-path]   locally built daemon (default: target/release/vtorrent-daemon)
#
# Assumes: root SSH access to the target is already configured in ~/.ssh/config
# or you pipe the script over SSH yourself:
#   ssh root@NEW_NODE 'bash -s' -- <ARGS> < deploy/provision-seed.sh   # see docs

set -euo pipefail

SEED_PEER="${1:?usage: provision-seed.sh <seed-peer-ip>}"
# The value is interpolated into a systemd unit. Accept only an IPv4 literal
# so a newline or metacharacter cannot inject a `[Service]` directive.
if [[ ! "$SEED_PEER" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]; then
  echo "invalid seed peer '$SEED_PEER': expected an IPv4 address" >&2
  exit 1
fi
# Runs ON the target node; the daemon binary must already be staged at
# /tmp/vtorrent-daemon (scp it from a build machine first — see header).

cat <<'EOF'
── vTorrent seed provisioning ───────────────────────────────────────────────
This script expects to run ON the target node as root, with the daemon
binary already at /tmp/vtorrent-daemon:

  export VTR_DAEMON_SHA256=$(sha256sum target/release/vtorrent-daemon | cut -d' ' -f1)
  scp target/release/vtorrent-daemon root@NODE:/tmp/vtorrent-daemon
  scp deploy/provision-seed.sh root@NODE:/tmp/
  ssh root@NODE "VTR_DAEMON_SHA256=$VTR_DAEMON_SHA256 /tmp/provision-seed.sh SEED_PEER_IP"

────────────────────────────────────────────────────────────────────────────
EOF

[[ -f /tmp/vtorrent-daemon ]] || { echo "missing /tmp/vtorrent-daemon" >&2; exit 1; }
expected="${VTR_DAEMON_SHA256:?set VTR_DAEMON_SHA256 to the published daemon digest}"
stage_dir=$(mktemp -d /run/vtorrent-provision.XXXXXXXX)
trap 'rm -f -- "$stage_dir/vtorrent-daemon"; rmdir -- "$stage_dir"' EXIT
install -m 0700 /tmp/vtorrent-daemon "$stage_dir/vtorrent-daemon"
actual=$(sha256sum "$stage_dir/vtorrent-daemon" | cut -d' ' -f1)
[[ "$actual" == "$expected" ]] || {
  echo "daemon checksum mismatch: got $actual, expected $expected" >&2
  exit 1
}

id -u vtorrent &>/dev/null || useradd --system --home-dir /var/lib/vtorrent --create-home --shell /usr/sbin/nologin vtorrent

install -m 0755 "$stage_dir/vtorrent-daemon" /usr/local/bin/vtorrent-daemon

install -d -o vtorrent -g vtorrent -m 0700 /etc/vtorrent
if [[ ! -f /etc/vtorrent/rpc-api-key ]]; then
  (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > /etc/vtorrent/rpc-api-key)
fi
[[ -f /etc/vtorrent/rpc-api-key && ! -L /etc/vtorrent/rpc-api-key ]] || {
  echo "RPC key must be a regular file, not a symlink" >&2
  exit 1
}
chown vtorrent:vtorrent /etc/vtorrent/rpc-api-key
chmod 0600 /etc/vtorrent/rpc-api-key

cat > /etc/systemd/system/vtorrent.service <<EOF
[Unit]
Description=vTorrent seed node
After=network-online.target
Wants=network-online.target

[Service]
User=vtorrent
Group=vtorrent
ExecStart=/usr/local/bin/vtorrent-daemon --listen 0.0.0.0:22526 --rpc-addr 127.0.0.1:22525 --rpc-api-key-file /etc/vtorrent/rpc-api-key --data-dir /var/lib/vtorrent --seed ${SEED_PEER}:22526
Restart=on-failure
RestartSec=10
LimitNOFILE=65536
NoNewPrivileges=true
ProtectSystem=full
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF

ufw allow OpenSSH
ufw allow 22526/tcp comment 'vTorrent P2P'
ufw --force enable

systemctl daemon-reload
systemctl enable --now vtorrent
sleep 3

echo "── verify ────────────────────────────────────────────────────────────────"
systemctl is-active vtorrent
ss -ltnp | grep -E ':22526|:22525'
curl -s http://127.0.0.1:22525/api/v1/info | head -c 200; echo
echo "Done. Next: add this node's IP:22526 to bootstrap/peers.txt,"
echo "BOOTSTRAP_PEERS (vtorrent-p2p/src/peer_manager.rs), DNS, and monitoring."
