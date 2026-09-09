# Local testnet images

All three nodes use the locally built sync-status release
`vtorrent/node:c9d00a6`. No node has a Compose
`build` entry, so recreation cannot
silently replace that version with the current working tree. No image was pushed
to a registry. Provision the image locally before bringing up this stack on
another host.

## Current release image

Source revision: `c9d00a62c74ff639354706d2990124c3d8dc0190`.
Binary SHA-256:
`6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
Image ID:
`sha256:01c51d65ec2e147e5debf2255106f4a7097e993014b03ba355debab468d79821`.

The private exact-image archive is
`.ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar`, SHA-256
`6baba25381737d940e338b388d3e48a16a789c822acb1fae89086d316371bf5f`.
Verify its checksum before loading it:

```bash
sha256sum .ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar
docker image load --input .ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar
docker image inspect vtorrent/node:c9d00a6 --format '{{.Id}}'
```

It uses the same release-artifact recipe and verified runtime base described
below, with the canary revision/checksum above and binary-only context
`.ops-backups/node3-sync-20260909-xciPth/image-context/`. The release daemon passed
all 18 process-recovery tests before packaging. Node1 was upgraded only after
green CI, recorded follower observation, and a verified wallet-unlock procedure.

## Restore the preceding image (rollback)

The operational archive is private and git-ignored:
`.ops-backups/node3-image-20260908-oclHcN/vtorrent-node-84125ea.tar`.
Its SHA-256 is
`e2d9bc4099a97b76f2d345609dd82712ceb431e63e3f17a011a9b6c709fdaeb6`.
From the repository root, verify and load it:

```bash
sha256sum .ops-backups/node3-image-20260908-oclHcN/vtorrent-node-84125ea.tar
docker image load --input .ops-backups/node3-image-20260908-oclHcN/vtorrent-node-84125ea.tar
docker image inspect vtorrent/node:84125ea --format '{{.Id}}'
```

Expected image ID:
`sha256:a56ac5545f87cc989b96500aa819f1715a070d21a2a72e4363316fbad74be41b`.
Do not overwrite this version tag with a different artifact.

## Package a validated release artifact

`docker/Dockerfile.release-artifact` packages an already-built, tested Linux
binary. It does not rebuild Rust source or guarantee a reproducible source build
without the original toolchain and dependency lockfile. Use a minimal build
context containing only `vtorrent-daemon`, never a wallet/data backup directory.

For this image, the context is
`.ops-backups/node3-image-20260908-oclHcN/image-context/`. The base was the existing
compatible runtime `vtorrent/node:soak`, image ID
`sha256:db00ea4eeb1f3c46647dc586c346bfb96d428bf4843ff743ba4781f2920d8ba2`.
Check the base ID before reusing these instructions; a changed tag is not the
same runtime. The saved image archive is the preferred exact-image recovery path.

```bash
docker build --pull=false \
  --build-arg RUNTIME_IMAGE=vtorrent/node:soak \
  --build-arg VCS_REF=84125ea5c273d0380778d6c60933c063cacff17e \
  --build-arg DAEMON_SHA256=1275b1f06c7b0d6076d3b36a9c9fbceda8e0ecaaa1bfd869d84830c955da79ce \
  -f docker/Dockerfile.release-artifact \
  -t vtorrent/node:84125ea \
  .ops-backups/node3-image-20260908-oclHcN/image-context
```

The build verifies the binary checksum and runtime compatibility and records the
revision/checksum in image labels. Existing image runtime configuration is inherited.

## One-follower recreation

First verify peer health, stop only the selected follower, and preserve a
stopped-data backup. After verifying the image identity, recreate only that
follower. For node3:

```bash
docker compose -f docker/testnet/docker-compose.yml up -d \
  --no-deps --no-build --pull never --force-recreate node3
```

For node2, use the same command with the final service argument `node2`, not
both services. Keep `vtorrent-testnet_vtr-data2` mounted at `/data/node2` and
`vtorrent-testnet_vtr-data3` mounted at `/data/node3`. Do not use `down -v`
or renew anonymous volumes. Check replay logs, binary checksum, health, tip
agreement, and a fresh propagated block before approving any other node upgrade.
Record every interruption in `docs/soak-log.md`.

## Staking-node recreation

Node1 additionally requires a verified wallet unlock path before stopping it.
Its persisted wallet starts locked; unlocking restores the saved staking intent.
Back up the encrypted wallet, staking intent, complete stopped data directory,
and any data in its other mounted volume. Preserve the BTC-regtest seed and
runtime arguments without resetting the BTC service.

After a separately approved maintenance window, use the recreation command above
with only `node1`. Preserve `vtorrent-testnet_vtr-data1` at `/data/node1` and the
existing anonymous volume. Unlock locally without placing the passphrase in
shell arguments or logs, then require a newly staked block to reach both followers.
Record RPC downtime and the longer stop-to-staking-resume interval separately.

The current local wallet uses the repository's publicly known deterministic
regtest key. Its new passphrase is stored privately under the node1 operational
backup directory, not in Compose. Encryption does not make a public test key safe
for real funds. Never use this key or stack for mainnet funds.
