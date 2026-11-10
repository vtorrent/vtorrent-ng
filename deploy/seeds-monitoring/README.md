# Seed Fleet Monitoring

Prometheus + Alertmanager + Grafana stack deployed on `vtr-seed1`
(91.98.80.38), scraping all three seed nodes.

## Architecture

| Component | Host | Port | Notes |
|---|---|---|---|
| Prometheus | vtr-seed1 | 9090 (localhost) | scrapes all daemons + node_exporters |
| Alertmanager | vtr-seed1 | 9093 (localhost) | webhook → ntfy push |
| Grafana | vtr-seed1 | **3000 (public)** | `admin` / password in ops vault |
| node_exporter | all three seeds | 9100 | ufw: seed1 IP only |
| metrics proxy (nginx) | vtr-seed2, vtr-seed3 | 9105 | `/metrics` → 127.0.0.1:22525, ufw: seed1 IP only |

The daemons' RPC listeners stay localhost-only; seed2/seed3 daemon metrics
are exposed read-only through an nginx location restricted to seed1's IP.
Applying a `prometheus.yml` target change requires copying the file to
`/etc/prometheus/prometheus.yml` on vtr-seed1 and reloading prometheus;
installing the proxy/exporter on a seed host requires SSH access to that
host. Both are production-seed actions — get approval before applying.

## Alerts (`vtorrent-alerts.yml`)

- `DaemonDown` — /metrics unreachable for 2m
- `PeerCountZero` — no P2P peers for 5m
- `HeightStalled` — height > 0 unchanged for 1h (silent pre-launch at 0)
- `StakingStopped` — staking flipped off within the last 24h
- `HostDiskFull` — root filesystem < 15% free
- `HostOutOfMemory` — available RAM < 10%
- `BtcWalletHistoryUnavailable` — synced BTC headers have pruned history the
  wallet still needs; increase header retention and rescan before relying on
  the balance. Header sync alone does not establish wallet readiness.
- `BtcSpvNeverSynced` — BTC is configured but no first header sync has completed
  within two hours of process start. Without it a node whose SPV bootstrap never
  succeeds looks healthy while its BTC wallet and swaps cannot see funding.

### Validate the BTC stale-scan rule

The unit tests cover delayed firing, recovery, healthy scans, disabled BTC,
and an absent timestamp. They need no network access or running daemon:

```bash
docker run --rm --network none --read-only --tmpfs /tmp:rw,size=64m \
  --entrypoint promtool \
  -v "$PWD/deploy/seeds-monitoring:/work:ro" \
  prom/prometheus:v2.53.0 test rules /work/vtorrent-alerts.test.yml
```

Passing these tests does not prove notification delivery. Loading production
rules, configuring an Alertmanager receiver, and testing delivery are separate
operations. On 2026-10-29 a controlled `DeliveryTest` alert was submitted to the
production Alertmanager and received on the ntfy topic; see `docs/soak-log.md`.
Resolved-notification timing was not part of that check.

Notifications are pushed to ntfy topic `vtorrent-seeds-4254e0588837`
(topic name acts as the secret). Subscribe at https://ntfy.sh/ or in the
mobile app. `ntfy-relay.py` (systemd unit `ntfy-relay`, listens on
127.0.0.1:9094) formats Alertmanager's JSON envelope into readable push
titles like `🔥 FIRING: DaemonDown` / `✅ RESOLVED: …`.

## Reinstall from scratch

```bash
# vtr-seed2 (remote scrape target)
apt-get install -y nginx prometheus-node-exporter
cp nginx-vtorrent-metrics.conf /etc/nginx/sites-available/vtorrent-metrics
ln -sf /etc/nginx/sites-available/vtorrent-metrics /etc/nginx/sites-enabled/
rm -f /etc/nginx/sites-enabled/default && systemctl reload nginx
ufw allow from 91.98.80.38 to any port 9105 proto tcp
ufw allow from 91.98.80.38 to any port 9100 proto tcp

# vtr-seed3 (remote scrape target — same recipe)
apt-get install -y nginx prometheus-node-exporter
cp nginx-vtorrent-metrics.conf /etc/nginx/sites-available/vtorrent-metrics
ln -sf /etc/nginx/sites-available/vtorrent-metrics /etc/nginx/sites-enabled/
rm -f /etc/nginx/sites-enabled/default && systemctl reload nginx
ufw allow from 91.98.80.38 to any port 9105 proto tcp
ufw allow from 91.98.80.38 to any port 9100 proto tcp

# vtr-seed1 (central)
apt-get install -y prometheus prometheus-alertmanager prometheus-node-exporter
install prometheus.yml /etc/prometheus/prometheus.yml
install vtorrent-alerts.yml /etc/prometheus/vtorrent-alerts.yml
install alertmanager.yml /etc/prometheus/alertmanager.yml
# single-node AM needs cluster disabled (no private IP on VPS):
systemctl edit --full prometheus-alertmanager   # add --cluster.listen-address=
# Grafana per https://grafana.com/docs/grafana/latest/setup-upgrade/install/debian/
# then provision grafana/datasources + grafana/dashboards and set admin password.
```

## External dead-man check

Prometheus and Alertmanager both run on seed1, so nothing on seed1 can report
that seed1 itself is gone. The chosen fix is a **heartbeat**: seed1 pings an
external dead-man service every five minutes, and silence is the alarm.

`heartbeat.sh` runs `monitor-ready.sh` first and pings the service's success URL
only when Prometheus, Alertmanager, and the ntfy relay are all ready. If local
readiness fails it pings the optional failure URL instead, so the alert carries
a reason rather than a bare timeout. URLs are passed to curl on stdin, never as
argv.

```bash
DEADMAN_PING_URL='https://hc-ping.com/<uuid>' \
DEADMAN_FAIL_URL='https://hc-ping.com/<uuid>/fail' \
  deploy/seeds-monitoring/heartbeat.sh
```

Install on seed1:

| File | Installed as | Mode |
|---|---|---|
| `heartbeat.sh` | `/usr/local/sbin/vtorrent-heartbeat` | 0755 |
| `monitor-ready.sh` | `/usr/local/sbin/monitor-ready.sh` | 0755 |
| `systemd/vtorrent-heartbeat.service` | `/etc/systemd/system/` | 0644 |
| `systemd/vtorrent-heartbeat.timer` | `/etc/systemd/system/` | 0644 |
| `heartbeat.env.example` | `/etc/vtorrent/heartbeat.env` | 0600 root |

Then `systemctl daemon-reload && systemctl enable --now vtorrent-heartbeat.timer`.
Set the service's period to match the timer (five minutes) plus a grace of at
least one interval, so a single missed ping does not page.

The hermetic checks need no network and no running daemon:

```bash
sh deploy/seeds-monitoring/tests/heartbeat-test.sh
```

They cover the healthy ping, the failure ping, silence with no failure URL, and
rejection of a URL that would break out of the curl config.

**What this does not prove.** A running but compromised seed1 can still ping,
so heartbeat health is not evidence that alerts would actually be delivered.
The production Alertmanager-to-ntfy route was verified separately on 2026-10-29;
resolved-notification timing was not part of that check.

### Alternative: SSH-based readiness check (not planned for launch)

`deadman.sh` implements the same idea from seed2 or seed3 over SSH, using a
dedicated `deadman` account on seed1, a forced command, and a pinned host key.
It is kept because it is tested and works, but it is **not** part of the launch
plan: it needs an account and key on three hosts, and both checking hosts
currently cannot reach seed1 and have no pinned host key. Its artifacts are
`deadman.sh`, `ssh/`, `vtorrent-monitor-ready`, and
`systemd/vtorrent-deadman.*`; `sh deploy/seeds-monitoring/tests/deadman-test.sh`
and `sh deploy/seeds-monitoring/tests/monitor-ready-wrapper-test.sh` cover the
transition logic and the forced command. Do not enable its timer without
completing the host-key pin and verifying the path.
