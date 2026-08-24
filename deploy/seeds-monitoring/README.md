# Seed Fleet Monitoring

Prometheus + Alertmanager + Grafana stack deployed on `vtr-seed1`
(91.98.80.38), scraping both seed nodes.

## Architecture

| Component | Host | Port | Notes |
|---|---|---|---|
| Prometheus | vtr-seed1 | 9090 (localhost) | scrapes both daemons + node_exporters |
| Alertmanager | vtr-seed1 | 9093 (localhost) | webhook → ntfy push |
| Grafana | vtr-seed1 | **3000 (public)** | `admin` / password in ops vault |
| node_exporter | both seeds | 9100 | ufw: seed1 IP only |
| metrics proxy (nginx) | vtr-seed2 | 9105 | `/metrics` → 127.0.0.1:22525, ufw: seed1 IP only |

The daemons' RPC listeners stay localhost-only; seed2's daemon metrics are
exposed read-only through an nginx location restricted to seed1's IP.

## Alerts (`vtorrent-alerts.yml`)

- `DaemonDown` — /metrics unreachable for 2m
- `PeerCountZero` — no P2P peers for 5m
- `HeightStalled` — height > 0 unchanged for 1h (silent pre-launch at 0)
- `StakingStopped` — staking flipped off within the last 24h
- `HostDiskFull` — root filesystem < 15% free
- `HostOutOfMemory` — available RAM < 10%

Notifications are pushed to ntfy topic `vtorrent-seeds-4254e0588837`
(topic name acts as the secret). Subscribe at https://ntfy.sh/ or in the
mobile app. Alertmanager delivers its native JSON envelope.

## Reinstall from scratch

```bash
# vtr-seed2 (remote scrape target)
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
