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

Run `deadman.sh` from a host other than seed1, for example a systemd timer on
seed2 or seed3. Use a dedicated SSH identity restricted on seed1 to the
read-only `monitor-ready.sh` forced command. Pin seed1's verified host key out
of band, and place the private key, known-hosts file, and ntfy topic in
root-only files. The remote command checks Prometheus, Alertmanager, and relay
service readiness without exposing their ports:

```bash
SEED1_SSH_TARGET='deadman@seed1' \
SEED1_SSH_KEY='/etc/vtorrent/deadman-key' \
SEED1_KNOWN_HOSTS='/etc/vtorrent/deadman-known-hosts' \
NTFY_TOPIC='<topic>' \
  deploy/seeds-monitoring/deadman.sh
```

It sends one ntfy alert when the check fails and one recovery notification
afterward, with retries when delivery fails. Store the topic in a 0600
environment file or secret store, not in the unit or argv. On the
latest connectivity check, both seed2 and seed3 timed out connecting to
seed1's port 9090. Do not enable the dead-man timer until a restricted,
authenticated monitoring path has been configured and tested from its host;
otherwise it will produce false alerts. The access check did not change the
production configuration. A strict SSH prerequisite check also found that
neither host had seed1's host key in its known-hosts file. A dedicated identity,
verified host-key pin, and restricted command still need to be provisioned.

The forced command is a service-readiness check, not a synthetic end-to-end
notification heartbeat. Verify the production alert route independently; a
compromised but running relay could still report healthy. The hermetic
transition and retry checks run with `sh deploy/seeds-monitoring/tests/deadman-test.sh`.

### Pre-staged deployment artifacts

Nothing in this subsection has been applied to production. These files exist so
provisioning is a short, reviewable step once the identity and host-key pin are
approved.

| File | Installed as | Host |
|---|---|---|
| `deadman.sh` | `/usr/local/sbin/vtorrent-deadman` (0755) | seed2/seed3 |
| `systemd/vtorrent-deadman.service` | `/etc/systemd/system/` | seed2/seed3 |
| `systemd/vtorrent-deadman.timer` | `/etc/systemd/system/` | seed2/seed3 |
| `ssh/deadman.env.example` | `/etc/vtorrent/deadman.env` (0600 root) | seed2/seed3 |
| `vtorrent-monitor-ready` | `/usr/local/libexec/vtorrent-monitor-ready` (0755) | seed1 |
| `monitor-ready.sh` | `/usr/local/libexec/monitor-ready.sh` (0755) | seed1 |
| `ssh/authorized_keys.deadman` | `~deadman/.ssh/authorized_keys` (0600) | seed1 |
| `ssh/sshd-deadman.conf` | `/etc/ssh/sshd_config.d/deadman.conf` | seed1 |

The wrapper and the `sshd` `Match User deadman` block both set the forced
command. The wrapper ignores `SSH_ORIGINAL_COMMAND` entirely, so a key holder
cannot run anything else even if one of the two layers is missing. Its check is
hermetic and needs no SSH or running daemon:

```bash
sh deploy/seeds-monitoring/tests/monitor-ready-wrapper-test.sh
```

Sequence, after approval:

1. Create the `deadman` account on seed1 with no shell login and no sudo.
2. Install the wrapper and `monitor-ready.sh` side by side in
   `/usr/local/libexec`, then install the `sshd` drop-in and
   `sshd -t` before reloading.
3. Verify seed1's host key fingerprint out of band, pin it in
   `/etc/vtorrent/deadman-known-hosts` on the checking host, and confirm
   `ssh -T ... /usr/local/libexec/vtorrent-monitor-ready` succeeds while
   `ssh -T ... id` also only reports readiness.
4. Install the key, environment file, unit, and timer; run the service once by
   hand and confirm the exit status and state file.
5. Prove the failure path: make the readiness check fail on purpose and confirm
   exactly one notification, then confirm the recovery notification on restore.

Steps 3–5 are the ones that remain unverified in production. Host-key pinning
and the seed2/seed3 → seed1 path are still outstanding; see the paragraph
above.
