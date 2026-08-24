# vTorrent On-Call Runbook

Operational procedures for the seed fleet and soak environment. Pair this with
the monitoring setup in `deploy/seeds-monitoring/`.

## Fleet Map

| Node | IP | Role | Access |
|---|---|---|---|
| `vtr-seed1` | 91.98.80.38 | Mainnet seed + monitoring stack | SSH root |
| `vtr-seed2` | 2.29.8.113 | Mainnet seed | SSH root |
| dev workstation | localhost | 7-day soak (docker compose) | local |

- Daemon binary: `/usr/local/bin/vtorrent-daemon`, systemd unit `vtorrent.service`,
  user `vtorrent`, data dir `/var/lib/vtorrent` (`chain.db`, `overlay.key`,
  `peers.dat`).
- P2P `22526/tcp` public; RPC `127.0.0.1:22525` localhost-only.
- Monitoring: Grafana http://91.98.80.38:3000 (admin), Prometheus/Alertmanager
  localhost on seed1; push alerts via ntfy topic `vtorrent-seeds-4254e0588837`.
- Bootstrap surfaces: DNS seeds `seed1/seed2.vtorrent.org` (IONOS),
  `BOOTSTRAP_PEERS` in `vtorrent-p2p/src/peer_manager.rs`,
  `bootstrap/peers.txt` on GitHub (+ jsdelivr/statically mirrors).

## Daily Health Check

```bash
scripts/soak-status.sh                      # soak fleet
ssh root@91.98.80.38 'systemctl is-active vtorrent; curl -s localhost:22525/api/v1/info'
ssh root@2.29.8.113 'systemctl is-active vtorrent; curl -s localhost:22525/api/v1/info'
```

Healthy = service active, height equal across seeds, connections ≥ 1 per node,
disk < 85%, no crash-looping units.

## Alert Triage

| Alert | Likely cause | Action |
|---|---|---|
| `DaemonDown` (>2m) | process crash / OOM / host reboot | `systemctl status vtorrent -l`; check `journalctl -u vtorrent -n 100`. Restart: `systemctl restart vtorrent`. If OOM-killed repeatedly, check memory trend in Grafana before restarting. |
| `PeerCountZero` (>5m) | peer lost connectivity, or remote seed down | From each side: `curl -s localhost:22525/api/v1/peers`. Test cross-connectivity: from seed1 `nc -zv 2.29.8.113 22526`. If ufw dropped rules after a host reboot: re-apply `ufw allow 22526/tcp`. If the other seed is the dead one, see *Seed failover*. |
| `HeightStalled` | staking stopped fleet-wide, or mempool poisoning | Check latest block age vs Grafana. Verify a staker is alive: `curl -s localhost:22525/api/v1/staking/status`. Inspect recent daemon log for "Rejected block" lines — if one node rejects all blocks, capture its log and restart it last. |
| `StakingStopped` | wallet locked / UTXO spent / passphrase cleared | `POST /api/v1/staking/start` with address+passphrase after unlocking. |
| `HostDiskFull` (<15%) | chain.db growth or journald bloat | `du -sh /var/lib/vtorrent/*`; `journalctl --vacuum-size=200M`. Do NOT delete chain.db (see *Chain-state recovery*). |
| `HostOutOfMemory` | leak or co-tenant | `systemctl status vtorrent` for oom score; consider restarting the daemon during a quiet window. |

## Procedure: Rolling Daemon Upgrade (seeds)

1. Build locally: `cargo build --release -p vtorrent-daemon`.
2. Stage: `scp target/release/vtorrent-daemon root@SEED:/tmp/vtorrent-daemon.new`.
3. On the seed:
   ```bash
   systemctl stop vtorrent
   install -m 0755 /tmp/vtorrent-daemon.new /usr/local/bin/vtorrent-daemon
   systemctl start vtorrent && sleep 5 && systemctl is-active vtorrent
   curl -s localhost:22525/api/v1/info   # verify version + resumed height
   ```
4. Repeat on the other seed. Never upgrade both simultaneously — keep one
   healthy seed serving DNS/bootstrap at all times.
5. Chain state must resume from disk ("Resuming from persisted chain" in the
   log). If it starts from genesis instead, STOP and see *Chain-state recovery*.

## Procedure: Ban-List Inspection

Bans live in memory (`BanManager`) — they do not survive a restart.

```bash
journalctl -u vtorrent | grep -iE 'ban|mischaviour|misbehaviour' | tail -50
```

Each `record_misbehaviour` line names the offence (invalid block, malformed
message) and IP. Bans auto-expire after the configured duration; there is no
RPC to clear them early. To clear: `systemctl restart vtorrent` (acceptable on
a seed — resync is fast).

If a legit peer is being banned repeatedly, capture the triggering message
from the log before restarting, then file an issue with the log excerpt.

## Procedure: Chain-State Recovery (store corruption)

Symptom: daemon fails to start, redb errors like "database corrupted", or the
node resumes from genesis unexpectedly.

1. Stop the daemon: `systemctl stop vtorrent`.
2. Preserve evidence: `cp -a /var/lib/vtorrent/chain.db /root/chain.db.bad.$(date +%s)`.
3. Recovery options, least destructive first:
   - **Resync from the other seed** (preferred): move the bad store aside,
     start the daemon with `--seed OTHER_SEED_IP:22526`, let IBD rebuild.
     ```bash
     mv /var/lib/vtorrent/chain.db /var/lib/vtorrent/chain.db.quarantine
     systemctl start vtorrent
     ```
   - **redb recovery**: redb performs its own journal recovery on open; if it
     refuses, the quarantine copy above is your forensic artifact — do not
     delete it until the incident is closed.
4. After resync: verify `block_height` matches the healthy seed and
   `best_block_hash` values are identical across nodes.
5. Post-incident: check disk fullness and RAM before declaring the cause.

## Procedure: RPC Key Rotation

The API key is set at launch via `--rpc-api-key` (see the unit's ExecStart).
RPC binds to localhost only, so rotation is low-risk:

1. Generate: `openssl rand -base64 32`.
2. Edit the unit: `systemctl edit vtorrent` → override `ExecStart=` with the
   new key appended.
3. `systemctl daemon-reload && systemctl restart vtorrent`.
4. Verify: `curl -s -H "X-API-Key: NEW" localhost:22525/api/v1/wallet/balance`.
   Old key must now return 401.
5. Update any scripts/monitoring that used the old key (Prometheus scrapes
   `/metrics`, which is unauthenticated by design).

## Procedure: Seed Failover

If a seed must leave the pool (host loss, abuse report, migration):

1. Remove its IP from:
   - `bootstrap/peers.txt` (commit + push; CDN mirrors refresh ~10 min)
   - `BOOTSTRAP_PEERS` in `vtorrent-p2p/src/peer_manager.rs`
   - the surviving seed's systemd unit `--seed=` argument (if pointed at it)
2. DNS (IONOS API or panel): delete/update the A record
   (`seedX.vtorrent.org`). Nodes resolve DNS seeds per bootstrap attempt, so
   removal takes effect immediately for fresh boots.
3. If ADDING a replacement: provision per `docs/dns-seeds.md`, add records +
   constants, then confirm inter-seed peering before announcing.

## Reorg Response

A reorg warning in the logs is normal at shallow depth (≤ 2–3 blocks) on a
PoS network. Escalate when:

- depth > 10 (possible long-range attack — preserve logs on ALL nodes)
- reorgs recur within an hour
- balances changed for addresses you did not expect

Capture: `journalctl -u vtorrent | grep -E 'Reorg|reorg' | tail`, plus each
node's current `best_block_hash`. Halt stakers (`staking/stop`) if you suspect
an attack, coordinate on the private channel, then resume once consensus on
the canonical tip is reached.

## Soak-Specific Ops

- Stack lives in `docker/testnet/docker-compose.yml` on the workstation.
  Status: `scripts/soak-status.sh`.
- Containers run with `apparmor=unconfined` (snap-docker signal-mediation
  workaround); `docker stop/start` works normally.
- Node1 stakes (500 VTR) and hosts the BTC-regtest SPV wallet; node2/node3
  are followers. Recreating containers preserves chain/wallet data volumes;
  the VTR hot wallet must be re-imported only if `wallet.json` is missing
  (post-fix versions persist it automatically).
- BTC regtest reset (nuclear): `docker compose rm -sf btc && docker volume rm
  vtorrent-testnet_btc-data && docker compose up -d btc`, then re-mine ≥130
  blocks and re-fund via faucet/sendtoaddress.

## Escalation

Anything not covered here, or any action that would take a seed offline during
a live network: stop, capture diagnostics (`systemctl status`, last 200 log
lines, `api/v1/info`, `api/v1/peers`), and escalate on the ops channel before
proceeding.
