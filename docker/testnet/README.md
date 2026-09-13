# Local testnet images

All three nodes use the locally built PEX top-K randomization release
`vtorrent/node:7db5da3`. No node has a Compose
`build` entry, so recreation cannot
silently replace that version with the current working tree. No image was pushed
to a registry. Provision the image locally before bringing up this stack on
another host.

## Current release image

Source revision: `7db5da31ed87ff18e3b86db7e1ef7d9d0c272374`.
Binary SHA-256:
`fda13af3a58c4d19a4eec86246273ddc9abca7f8ac2ff20eb30ab8cb33e821aa`.
Image ID:
`sha256:1308257446d5a6a3c381dcdeb693a1dfb36ddc0c7337ccb77978ef4753c69935`.

The private exact-image archive is
`.ops-backups/pex-20260913-97BzX7/vtorrent-node-7db5da3.tar`, SHA-256
`30f9d5d0b799e1d66f86702418910e4b6293dae5bfda842938aca247aeb39b0a`.
Verify its checksum before loading it:

```bash
sha256sum .ops-backups/pex-20260913-97BzX7/vtorrent-node-7db5da3.tar
docker image load --input .ops-backups/pex-20260913-97BzX7/vtorrent-node-7db5da3.tar
docker image inspect vtorrent/node:7db5da3 --format '{{.Id}}'
```

It uses the same release-artifact recipe and verified runtime base described
below, with the canary revision/checksum above and binary-only context
`.ops-backups/pex-20260913-97BzX7/image-context/`. The release daemon passed
all 18 process-recovery tests before packaging. Node1 was upgraded only after
green CI, recorded follower observation, and a verified wallet-unlock procedure.

## Restore the preceding image (rollback)

The immediate rollback image is `vtorrent/node:c9d00a6`, the prior fleet
release:

- Archive: `.ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar`,
  SHA-256 `6baba25381737d940e338b388d3e48a16a789c822acb1fae89086d316371bf5f`,
  image ID `sha256:01c51d65ec2e147e5debf2255106f4a7097e993014b03ba355debab468d79821`,
  binary SHA-256 `6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.

From the repository root, verify and load it:

```bash
sha256sum .ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar
docker image load --input .ops-backups/node3-sync-20260909-xciPth/vtorrent-node-c9d00a6.tar
docker image inspect vtorrent/node:c9d00a6 --format '{{.Id}}'
```

The earlier operational archive `vtorrent/node:84125ea` remains available at
`.ops-backups/node3-image-20260908-oclHcN/vtorrent-node-84125ea.tar`
(SHA-256 `e2d9bc4099a97b76f2d345609dd82712ceb431e63e3f17a011a9b6c709fdaeb6`,
ID `sha256:a56ac5545f87cc989b96500aa819f1715a070d21a2a72e4363316fbad74be41b`) for
deeper rollback. Do not overwrite either version tag with a different artifact.

## Package a validated release artifact

`docker/Dockerfile.release-artifact` packages an already-built, tested Linux
binary. It does not rebuild Rust source or guarantee a reproducible source build
without the original toolchain and dependency lockfile. Use a minimal build
context containing only `vtorrent-daemon`, never a wallet/data backup directory.

For this image, the context is
`.ops-backups/pex-20260913-97BzX7/image-context/`. The base was the existing
compatible runtime `vtorrent/node:soak`, image ID
`sha256:db00ea4eeb1f3c46647dc586c346bfb96d428bf4843ff743ba4781f2920d8ba2`.
Check the base ID before reusing these instructions; a changed tag is not the
same runtime. The saved image archive is the preferred exact-image recovery path.

```bash
docker build --pull=false \
  --build-arg RUNTIME_IMAGE=vtorrent/node:soak \
  --build-arg VCS_REF=7db5da31ed87ff18e3b86db7e1ef7d9d0c272374 \
  --build-arg DAEMON_SHA256=fda13af3a58c4d19a4eec86246273ddc9abca7f8ac2ff20eb30ab8cb33e821aa \
  -f docker/Dockerfile.release-artifact \
  -t vtorrent/node:7db5da3 \
  .ops-backups/pex-20260913-97BzX7/image-context
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
