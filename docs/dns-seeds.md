# vTorrent 2.0 — DNS Seed Infrastructure

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

### Requirements

- VPS with at least 2 GB RAM and 20 GB SSD
- Ubuntu 22.04 LTS
- Open port `22524` (TCP, vTorrent P2P)
- Static IP address

### Installation

```bash
# 1. Clone the vtorrent-ng repository
git clone https://github.com/vtorrent/vtorrent-ng.git
cd vtorrent-ng

# 2. Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 3. Build the node binary
cargo build --release -p vtorrent-node

# 4. Create the data directory
mkdir -p ~/.vtorrent

# 5. Create the node configuration
cat > ~/.vtorrent/vtorrent.conf << 'EOF'
# vTorrent 2.0 Node Configuration
listen=1
port=22524
maxconnections=125
# Enable this node as a seed (accepts inbound connections)
seednode=1
# Log level: error, warn, info, debug
loglevel=info
EOF

# 6. Create a systemd service
sudo tee /etc/systemd/system/vtorrent.service << 'EOF'
[Unit]
Description=vTorrent 2.0 Node
After=network.target

[Service]
Type=simple
User=ubuntu
ExecStart=/home/ubuntu/vtorrent-ng/target/release/vtorrent-node
Restart=on-failure
RestartSec=10
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

# 7. Enable and start the service
sudo systemctl daemon-reload
sudo systemctl enable vtorrent
sudo systemctl start vtorrent

# 8. Check status
sudo systemctl status vtorrent
journalctl -u vtorrent -f
```

### Firewall Configuration

```bash
# Allow vTorrent P2P port
sudo ufw allow 22524/tcp comment 'vTorrent P2P'
sudo ufw allow 22525/tcp comment 'vTorrent RPC (local only)'
sudo ufw enable
```

## Updating the Seed Node List in the Codebase

Once your seed nodes are running, update `vtorrent-node/src/genesis.rs`:

```rust
pub const DNS_SEEDS: &[&str] = &[
    "seed1.vtorrent.io",
    "seed2.vtorrent.io",
    "seed3.vtorrent.io",
    "dnsseed.vtorrent.io",
];
```

And update `vtorrent-p2p/src/peer_manager.rs` to use these seeds in the initial peer discovery.

## DNS Seed Crawler

For a production network with many nodes, run the Bitcoin DNS seeder adapted for vTorrent:

```bash
# Install the DNS seeder (based on sipa's bitcoin-seeder)
git clone https://github.com/sipa/bitcoin-seeder.git
cd bitcoin-seeder

# Configure for vTorrent
# Edit main.cpp: change port to 22524 and magic bytes to 0x22053570

make
./dnsseed -h dnsseed.vtorrent.io -n vps1.vtorrent.io -m admin@vtorrent.io
```

## Network Parameters Reference

| Parameter | Value |
|---|---|
| **P2P Port** | 22524 |
| **RPC Port** | 22525 |
| **Network Magic** | `0x22 0x05 0x35 0x70` |
| **Protocol Version** | 70002 |
| **Chain ID** | `vtorrent-mainnet-v2` |
| **Genesis Block Date** | 2024 (new chain) |
| **Legacy Snapshot Date** | 2018-01-10 (block 1,680,456) |

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
