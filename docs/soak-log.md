# Testnet Soak Operations Log

## 2026-09-08 — node3 release canary upgrade

Only local regtest follower `vtr-node3` was upgraded to commit
`84125ea5c273d0380778d6c60933c063cacff17e`. Node1 (the staker), node2,
Bitcoin regtest, monitoring, and production seeds were not restarted or upgraded.
CI was green and all 17 release-mode recovery tests passed before this operation.

- Preflight: all three nodes agreed at height 2663; node1 staking enabled,
  node3 staking disabled. Node3 reached height 2664 before shutdown.
- Graceful stop: `2026-09-08T08:08:01.959253307Z`, exit code 0.
- Backup: stopped `/data/node3` and the previous daemon binary copied to private,
  git-ignored `.ops-backups/node3-20260908-dvqrcV/`. Its README records checksums
  and rollback precautions. This local backup is not an off-site backup.
- Restart: `2026-09-08T08:08:46.826859663Z`, with unchanged container arguments,
  volumes, network mode, and seed configuration.
- Replay completed at height 2664 at `08:09:16.845879Z`; peer handshake completed
  at `08:09:16.958609Z`, then node3 caught up to height 2665.
- No derived-UTXO repair warning or replay error appeared during startup.
- Fresh block 2666 arrived at node3 at `08:10:35.275023Z`. All three RPC endpoints
  agreed on `3f2c7fddfb2563f0fd0fbc48e9c9a3b3f15d225fc21bf733bad0dfe63cd847e1`.
  Node3 also accepted block 2667 at `08:13:33.275060Z`.
- Docker health was healthy with restart count 0. Node3 remained a follower;
  node1 was still staking at the `08:16:15Z` follow-up.

Installed binary SHA-256:
`1275b1f06c7b0d6076d3b36a9c9fbceda8e0ecaaa1bfd869d84830c955da79ce`.
The installed checksum matched the locally tested release binary.

This initial operation was a container-writable-layer canary, not an image rollout. The shared
`vtorrent/node:soak` image and Compose configuration were deliberately unchanged.
Recreating node3 from that image would revert its binary; prepare a versioned
image before treating this as a durable fleet rollout.

Node3's availability was interrupted from graceful shutdown until RPC startup
after replay (about 75 seconds). Do not count this interval as uninterrupted
three-node availability. The existing seven-day soak checklist remains pending;
subsequent observations are from a mixed-version fleet until another explicitly
approved rollout.

## 2026-09-08 — node3 versioned-image recreation

The canary binary is now packaged as local image `vtorrent/node:84125ea` and
node3's Compose service references that image without a source-build entry.
Node1/node2 still reference the unchanged `vtorrent/node:soak` image.

- Image ID: `sha256:a56ac5545f87cc989b96500aa819f1715a070d21a2a72e4363316fbad74be41b`.
- Base runtime ID: `sha256:db00ea4eeb1f3c46647dc586c346bfb96d428bf4843ff743ba4781f2920d8ba2`.
- Binary revision/checksum are unchanged from the tested canary above and are
  recorded in the image labels. Build-time verification and the recreated
  container's binary checksum both passed.
- Preflight fleet agreement: height 2674,
  `5082d316d15a8c08956050a9e8e59204dbbc0e586f0a8d272ffae8121ae8cfee`.
- Clean stop: `2026-09-08T08:40:12.534333630Z`, exit 0. Fresh stopped-data and
  binary backups, the image archive, and the minimal build context are retained
  privately in `.ops-backups/node3-image-20260908-oclHcN/`.
- Recreation used `up -d --no-deps --no-build --pull never --force-recreate node3`.
  The existing named data volume and anonymous volume were both preserved.
- New container ID: `8de4883e192ffa6659a8a0076f941bb1aa4d32b723862fe12e81f72f910db922`.
  Started at `2026-09-08T12:42:25.593762420Z`.
- Existing chain replay completed at height 2676 at `12:42:55.394053Z`, with no
  repair warning or replay error. Node3 caught up to height 2771 by `12:42:56.883386Z`.
- Fresh block 2772 propagated at `12:43:28.238319Z`; all three RPC endpoints agreed
  on `b0ed85ac31d48a417ad6c56e79ba957f44829866e7cb4b1ca3f329e37a1b3d8d`.
- Node3 was healthy with restart count 0 and staking disabled. Node1 remained
  staking. Node1/node2 container IDs, image IDs, and September 4 start times
  were unchanged; production seeds were untouched.

The full node3 interruption was approximately **4 hours 2 minutes 43 seconds**,
from clean stop until RPC startup at `12:42:55.410037Z`. This interval does not
count toward uninterrupted three-node soak availability. The seven-day checklist
remains pending.

The image is local, not registry-published. Its exact archive, checksum, build
recipe, and node3-only recreation procedure are documented in
[the testnet image guide](../docker/testnet/README.md). Both rollback backups are
local only, not off-site backups. Recreating node3 with the checked-in Compose
configuration now retains the versioned binary rather than reverting the canary.
