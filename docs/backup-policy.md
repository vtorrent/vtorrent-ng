# Backup Policy

**Status:** 2026-08-26 — applies to all mainnet seed nodes and operator wallets.

## Seed Node Data Directories

Each seed's `/var/lib/vtorrent/` (see `docs/dns-seeds.md`) is snapshotted daily:

```bash
# /etc/cron.daily/vtorrent-backup (on each seed)
#!/bin/sh
set -eu
umask 077
STAMP=$(date -u +%Y%m%d)
DEST="/backups/vtorrent-$STAMP.tar.gz"
STAGE=$(mktemp -d /backups/.vtorrent-stage.XXXXXX)
restart() { systemctl start vtorrent >/dev/null 2>&1 || true; }
trap 'restart; rm -rf "$STAGE"' EXIT

# Stop writers so redb is not copied mid-transaction. The seed rejoins after
# the copy; keep this window short.
systemctl stop vtorrent || true
for file in chain.db wallet.json staking.json; do
  if [ -f "/var/lib/vtorrent/$file" ]; then
    cp -a "/var/lib/vtorrent/$file" "$STAGE/"
  fi
done
restart
trap 'rm -rf "$STAGE"' EXIT

tar -czf "$DEST" -C "$STAGE" .
tar -tzf "$DEST" >/dev/null
find /backups -name 'vtorrent-*.tar.gz' -mtime +14 -delete
```

The archive is created with `umask 077` because it may contain `wallet.json`.
Restore drills must extract a recent archive and open the wallet on a staging
host; an untested archive is not a backup.

- `chain.db` is the only irreplaceable file pre-launch; post-launch it can be
  rebuilt by resyncing from peers, but a daily snapshot makes recovery instant.
- Backups are local-only until an off-site target is chosen (launch-week item).

## Genesis & Snapshot Binaries

The following artifacts must exist in **≥2 independent locations** with
checksums published in this file:

| Artifact | Location 1 | Location 2 |
|---|---|---|
| `genesis_snapshot.bin` (2.4 MB) | repo `vtorrent-node/src/` | GitHub Release asset (`v2.0.0`) |
| Legacy snapshot extraction tooling | repo `vtorrent-snapshot/` | GitHub Release source tarball |

Checksums (SHA-256) are printed at release time and appended here:

```
# To be filled at v2.0.0 tag:
# genesis_snapshot.bin  sha256: <pending>
```

## Wallets

Wallet backups are the user's responsibility by design — the encrypted
`wallet.json` (Argon2id + ChaCha20-Poly1305) plus the passphrase is the only
recovery path. The desktop app surfaces this during import; seed operators
follow the cron policy above.
