# vTorrent 2.0 — DNS Seed Infrastructure

> **Status:** The legacy `seed1/2/3.vtorrent.io` domains are retired and no
> longer resolve. Two new mainnet seed nodes are deployed (see inventory below);
> bootstrap peers are published via `bootstrap/peers.txt` (GitHub-hosted) and
> the `BOOTSTRAP_PEERS` constant in `vtorrent-p2p/src/peer_manager.rs`.
> DNS A records and a crawler are still pending.

## Deployed Seed Nodes

| Hostname | IP | Location | P2P | RPC |
|---|---|---|---|---|
| `vtr-seed1` (`seed1.vtorrent.io`) | `91.98.80.38` | Falkenstein, DE | 22526/tcp | localhost only |
| `vtr-seed2` (`seed2.vtorrent.io`) | `2.29.8.113` | Helsinki, FI | 22526/tcp | localhost only |

Both run `vtorrent-daemon` under systemd (unit `vtorrent.service`, user
`vtorrent`, data dir `/var/lib/vtorrent`), peered with each other via `--seed`.

## Overview

DNS seeds are the bootstrap mechanism that allows new nodes to discover peers on the vTorrent 2.0 network without any hardcoded IP addresses. When a new node starts up, it queries the DNS seeds to get an initial list of active peers.

## Required DNS Records

You need to configure the following DNS records on a domain you control (e.g., `vtorrent.io`):

### Seed Nodes (A Records)

| Hostname | Type | Value | Purpose |
|---|---|---|---|
| `seed1.vtorrent.io` | A | `<seed1-server-ip>` | Primary seed node (US/EU) |
| `seed2.vtorrent.io` | A | `<seed2-server-ip>` | Secondary seed node (Asia) |
| `seed3.vtorrent.io` | A | `<seed3-server-ip>` | Tertiary seed node (backup) |

### DNS Seed Crawler (NS Records)

For a production network, you should run a **DNS seed crawler** — a service that continuously crawls the network and returns a rotating list of active node IPs via DNS. This is the same approach used by Bitcoin (`seed.bitcoin.sipa.be`).

| Hostname | Type | Value | Purpose |
|---|---|---|---|
| `dnsseed.vtorrent.io` | NS | `vps1.vtorrent.io` | DNS seed crawler nameserver |
| `vps1.vtorrent.io` | A | `<crawler-server-ip>` | The crawler server itself |

## Setting Up a Seed Node

### Installation

```bash
# 1. Clone the vtorrent-ng repository
git clone https://github.com/vtorrent/vtorrent-ng.git
cd vtorrent-ng

# 2. Build the daemon (or build locally and scp target/release/vtorrent-daemon)
cargo build --release -p vtorrent-daemon
install -m 0755 target/release/vtorrent-daemon /usr/local/bin/

# 3. Create a service user and data directory
useradd --system --home-dir /var/lib/vtorrent --create-home --shell /usr/sbin/nologin vtorrent

# 4. Create the systemd service
sudo tee /etc/systemd/system/vtorrent.service << 'EOF'
[Unit]
Description=vTorrent seed node
After=network-online.target
Wants=network-online.target

[Service]
User=vtorrent
Group=vtorrent
ExecStart=/usr/local/bin/vtorrent-daemon --listen 0.0.0.0:22526 --rpc-addr 127.0.0.1:22525 --data-dir /var/lib/vtorrent
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

# 5. Enable and start the service
sudo systemctl daemon-reload
sudo systemctl enable --now vtorrent

# 6. Check status
sudo systemctl status vtorrent
journalctl -u vtorrent -f
```

### Firewall Configuration

```bash
# Allow vTorrent P2P port
sudo ufw allow OpenSSH
sudo ufw allow 22526/tcp comment 'vTorrent P2P'
sudo ufw --force enable
# RPC stays bound to 127.0.0.1; do NOT open it publicly
```

## Updating the Seed Node List in the Codebase

Once your seed nodes are running, update `vtorrent-p2p/src/peer_manager.rs`:

```rust
pub const BOOTSTRAP_PEERS: &[&str] = &[
    "91.98.80.38:22526",  // vtr-seed1 (Falkenstein, DE)
    "2.29.8.113:22526",   // vtr-seed2 (Helsinki, FI)
];

// Once DNS A records are live, add hostnames here:
pub const DNS_SEEDS: &[&str] = &[];
```

and append the IPs to `bootstrap/peers.txt` (GitHub + CDN mirrors refresh
within ~10 minutes of a push).

## DNS Seed Crawler

For a production network with many nodes, run the Bitcoin DNS seeder adapted for vTorrent:

```bash
# Install the DNS seeder (based on sipa's bitcoin-seeder)
git clone https://github.com/sipa/bitcoin-seeder.git
cd bitcoin-seeder

# Configure for vTorrent
# Edit main.cpp: change port to 22526 and magic bytes to 0x56545232 ("VTR2")

make
./dnsseed -h dnsseed.vtorrent.io -n vps1.vtorrent.io -m admin@vtorrent.io
```

## Network Parameters Reference

| Parameter | Value |
|---|---|
| **P2P Port** | 22526 |
| **RPC Port** | 22525 (localhost only) |
| **Network Magic** | `0x56 0x54 0x52 0x32` (`"VTR2"`) |
| **Chain ID** | `vtorrent-mainnet` |

## Testnet

For development and testing, a separate testnet is available:

| Parameter | Value |
|---|---|
| **Testnet P2P Port** | 32524 |
| **Testnet RPC Port** | 32525 |
| **Testnet DNS Seed** | `seed1-testnet.vtorrent.io` |

To run a testnet node:
```bash
./vtorrent-node --testnet
```
