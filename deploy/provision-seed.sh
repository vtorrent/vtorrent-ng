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
# Runs ON the target node; the daemon binary must already be staged at
# /tmp/vtorrent-daemon (scp it from a build machine first — see header).

cat <<EOF
── vTorrent seed provisioning ───────────────────────────────────────────────
This script expects to run ON the target node as root, with the daemon
binary already at /tmp/vtorrent-daemon:

  scp target/release/vtorrent-daemon root@NODE:/tmp/vtorrent-daemon
  scp deploy/provision-seed.sh root@NODE:/tmp/
  ssh root@NODE '/tmp/provision-seed.sh SEED_PEER_IP'

────────────────────────────────────────────────────────────────────────────
EOF

[[ -f /tmp/vtorrent-daemon ]] || { echo "missing /tmp/vtorrent-daemon" >&2; exit 1; }

id -u vtorrent &>/dev/null || useradd --system --home-dir /var/lib/vtorrent --create-home --shell /usr/sbin/nologin vtorrent

install -m 0755 /tmp/vtorrent-daemon /usr/local/bin/vtorrent-daemon

cat > /etc/systemd/system/vtorrent.service <<EOF
[Unit]
Description=vTorrent seed node
After=network-online.target
Wants=network-online.target

[Service]
User=vtorrent
Group=vtorrent
ExecStart=/usr/local/bin/vtorrent-daemon --listen 0.0.0.0:22526 --rpc-addr 127.0.0.1:22525 --data-dir /var/lib/vtorrent --seed ${SEED_PEER}:22526
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
