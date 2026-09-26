# Testnet Soak Operations Log

Daily observations are collected with `scripts/soak-observe.sh`, which prints a
markdown block ready to append here. It is read-only and exits non-zero if any
node is down or the fleet disagrees on the tip, so it can gate a cron job.

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

## 2026-09-08 — node2 follower upgrade after canary observation

CI for commit `4438403` passed all required checks. Before proceeding, node3 had
over two hours of uptime and zero restarts. Prometheus queries around 14:49–14:50
UTC covered the preceding 90 minutes: 360 successful 15-second scrapes,
`min_over_time(up)=1`, minimum peer count 1, approximately 38 blocks of height
growth, and maximum sampled lag behind the fleet of one block. This historical
observation covered the additional hour requested after the earlier 30-minute
canary check. Current RPC tips also agreed; it is not a seven-day soak sign-off.

Only non-staking follower `vtr-node2` was upgraded to the same verified local
image `vtorrent/node:84125ea`, ID
`sha256:a56ac5545f87cc989b96500aa819f1715a070d21a2a72e4363316fbad74be41b`.

- Preflight agreement: height 2824,
  `62eff64c369ff06727beacee987aef1c48dcddb5ad917337fa95dc91e3fabe6f`.
- Old binary and stopped `/data/node2` were backed up privately in
  `.ops-backups/node2-image-20260908-7y7Hyc/`; its README records checksums and
  rollback precautions. The backup remains local only.
- Graceful stop: `2026-09-08T14:51:12.629357618Z`, exit 0.
- Replacement start: `2026-09-08T14:51:13.015833654Z`, using
  `up -d --no-deps --no-build --pull never --force-recreate node2`.
- New container: `617de8710bff0ad578c5d5f15dbe52c28b4b30cb8b4b7a858f99b169525521e0`.
  Named volume `vtorrent-testnet_vtr-data2` and the prior anonymous volume were
  preserved, along with all runtime arguments and regtest/fast-stake settings.
- Installed binary checksum matched
  `1275b1f06c7b0d6076d3b36a9c9fbceda8e0ecaaa1bfd869d84830c955da79ce`.
- Replay completed at stored height 2826 at `14:51:46.016452Z`; RPC started at
  `14:51:46.016647Z`. No replay error or derived-state repair warning appeared.
- Peer handshake completed at `14:51:46.122083Z`; node2 caught up to height 2827.
- Fresh block 2828 propagated at `14:56:04.238719Z`; all three RPC endpoints
  agreed on `28678d5a34bb18144573abfb57dc2384e87928cfec1684580d91b1cc5f893615`.
- Node2 was healthy with restart count 0 and staking disabled. Node1 remained
  staking. Node1/node3 container identities and start times were unchanged;
  production seeds were untouched.

Node2's availability interruption was approximately 33 seconds. Exclude that
interval from uninterrupted three-node availability. Both followers now use the
versioned release image; node1 remains on its older image and requires a separate
approved staking-aware upgrade. The seven-day soak remains pending.

## 2026-09-08 — node1 wallet recovery and staking-aware upgrade

After CI passed for `7333b54` and both followers passed an additional hour of
recorded observation, the operator approved upgrading node1. Preflight found
the persisted wallet and staking intent, but the original passphrase was unknown.
The smoke-test passphrase failed wallet decryption; node1 was left running until
the operator separately approved same-key wallet recovery.

The repository's deterministic regtest WIF was independently decoded and its
public key/address derived, matching node1's active staking address. The original
encrypted wallet, staking intent, and binary were privately backed up before
re-importing that same key with a new randomly generated passphrase. Import and
unlock succeeded, with the same address and four UTXOs. No swap recovery directory
existed. The passphrase is retained only in a 0600 file under the 0700, git-ignored
`.ops-backups/node1-image-20260908-nHCswr/` directory; its private README records
the unlock procedure. The old encrypted wallet remains preserved. This public
test key must never hold real funds, regardless of its encryption passphrase.

Only node1 was recreated on `vtorrent/node:84125ea`, image ID
`sha256:a56ac5545f87cc989b96500aa819f1715a070d21a2a72e4363316fbad74be41b`.

- Graceful stop: `2026-09-08T16:11:19.318199265Z`, exit 0.
- Complete stopped `/data/node1` and `/home/vtorrent/.vtorrent` copies were
  preserved in the private backup directory, including the recovered encrypted
  wallet and staking intent. These are local backups, not off-site backups.
- Chain database SHA-256:
  `7e84b2df5c58630b5c73fd2c1a7c601a350b30e2dbdc5d4d72be024980b7414a`.
- New container: `9ddef1e31eea9f8bd1ccca4db11aeec3698d3960630b531efbcaeb754ec6fd9e`,
  started at `2026-09-08T16:11:19.694368912Z` using
  `up -d --no-deps --no-build --pull never --force-recreate node1`.
- Named volume `vtorrent-testnet_vtr-data1` and the existing anonymous volume
  were retained; BTC seed, networking, and all runtime arguments were unchanged.
- Installed binary SHA-256:
  `1275b1f06c7b0d6076d3b36a9c9fbceda8e0ecaaa1bfd869d84830c955da79ce`.
- Validated replay completed at height 2860 at `16:11:52.507874Z`; RPC listened
  at `16:11:52.509014Z`. Wallet restored locked and BTC-regtest SPV initialized.
- Protected-file unlock auto-resumed staking at `16:12:09.848176Z`.
- First new stake: height 2861 at `16:13:39.549933Z`, hash
  `66075c341e72d4bfaf86ee23514751d8fb7fa4dfd165302b2adc1cd56ccc0821`.
  Node2 accepted it at `16:13:39.740312Z`; node3 reconnected and accepted it at
  `16:13:55.734470Z`.
- Follow-up around 23:41–23:42 UTC: 186 blocks staked since restart; all nodes
  matched at height 3046, hash
  `a7c4761bb0e60a22c9647ebfe2a581d1d0a70ed3bf913f400817fa36972f4c75`.
  All containers were healthy with zero restarts. BTC-regtest SPV reported
  initialized and synced at height 140. No node1 WARN/ERROR lines were found
  in the post-upgrade log check.

RPC availability was interrupted for approximately 33 seconds; the stopped-node
to staking-resume interval was approximately 51 seconds. Wallet re-import also
briefly locked the wallet before its pre-upgrade unlock; no continuous staking
claim is made for that recovery interval. Followers were not restarted, but
temporarily lost their seed connection during node1 recreation. Exclude these
maintenance/reconnection intervals from uninterrupted soak measurements.

An unresolved follow-up remains: both followers report `syncing: true` and 99.9%
despite matching node1's tip and propagating new blocks. Record this as a
sync-status discrepancy, not evidence of chain divergence or a completed sync
diagnosis. All three nodes now use the versioned image; production seeds, BTC,
and monitoring services were not redeployed. Seven-day soak sign-off is pending.

## 2026-09-09 — follower sync-status diagnosis (not deployed)

Code inspection confirmed that the daemon event bridge set `syncing` on losing
the last peer but never cleared it after reconnection or block catch-up. It also
retained the highest historical peer height after that peer disconnected.
The source fix recalculates status from the connected peer list and local height
on peer/block/reorg events, with duplicate peer events handled idempotently.

A real-daemon reconnect regression exposed a related transport issue: the peer
task matched only `Some` from its incoming stream, so clean TCP EOF disabled
that select branch without ending the connection task. EOF and command-channel
closure now terminate the task and emit the disconnect event promptly.

Focused status tests, clean-close tests, and the actual-daemon disconnect/reconnect
and fresh-block propagation test pass, as do strict Clippy checks for both affected
crates. These source changes have not been deployed: the running containers still
use `vtorrent/node:84125ea`. No node restart or soak interruption was performed
for this diagnosis and fix.

The complete daemon and P2P all-features test run passed 109 tests: five daemon
unit tests, 17 RPC integration tests, 18 process-recovery tests (including the
interrupted-reorg matrix and swap recovery), and 69 P2P tests. Formatting and
`git diff --check` also passed.

## 2026-09-09 — sync-status release canary on node3

Commits `6a7be17` (node1 rollout record) and `c9d00a6` (sync-status/peer-close
fixes) were pushed to `origin/main`. CI run
[34295160138](https://github.com/vtorrent/vtorrent-ng/actions/runs/34295160138)
passed all required jobs before deployment; desktop builds were skipped because
this was not a release tag. The exact release daemon also passed all 18
process-recovery tests locally before packaging.

Only node3 was upgraded to the new local image `vtorrent/node:c9d00a6`, ID
`sha256:01c51d65ec2e147e5debf2255106f4a7097e993014b03ba355debab468d79821`.
The release binary SHA-256 is
`6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
Its exact image archive, stopped-data copies, and previous executable are private
under `.ops-backups/node3-sync-20260909-xciPth/`. Image recovery details are in
[the testnet image guide](../docker/testnet/README.md). No image was pushed to
a registry; backups remain local only.

- Preflight tips agreed at height 3237; node3 was non-staking. Both old followers
  still reported the known 99.9% sync-status discrepancy.
- Graceful stop: `2026-09-09T07:25:59.990749891Z`, exit 0.
- Both stopped data locations, `/data/node3` and `/home/vtorrent/.vtorrent`,
  were copied before container replacement. Stored height was 3238;
  chain database SHA-256:
  `bb1e2efb8615f3d5de598b663dac9dc2954e4d9738ea70239be3d286e4200c53`.
- New container: `fb07560f136d34355bc3f1780a769feebe7e9c7f31f9ecaae989238f9017f784`,
  started at `2026-09-09T07:26:00.601606503Z`. Recreation used
  `up -d --no-deps --no-build --pull never --force-recreate node3`.
- Named volume `vtorrent-testnet_vtr-data3` and the existing anonymous volume
  were preserved. Runtime arguments, isolation, and network mode were unchanged.
- Installed binary checksum matched the tested artifact. Replay completed at
  height 3238 at `07:26:38.962461Z`; RPC listened at `07:26:38.963229Z`.
  No startup WARN/ERROR or derived-state repair warning appeared.
- Peer handshake completed at `07:26:39.068654Z`; fresh block 3239 was accepted
  at `07:27:17.686535Z`, hash
  `15b438ad40bd0fcd6a5eee39e0f13ddbd53fb48dbfeaaa1007cb604b0c828ba6`.
- Follow-up tips agreed across all three nodes at height 3240, hash
  `1fc373581d7b4c3ba243385f9f57c61fa177b93404c9d35b9b4664f356dbb87f`.
  Node3 reported one peer, `syncing: false`, 100%, healthy, and zero restarts.
- Node1/node2 start times were unchanged; node1 remained staking. Node2 still
  reports 99.9% on its older build; it was not upgraded. Production seeds, BTC,
  and monitoring services were untouched.

Node3's RPC availability interruption was approximately 39 seconds. Exclude that
interval from uninterrupted three-node availability. This verifies startup,
catch-up, and fresh-block propagation on the new canary; disconnect/reconnect
behavior was exercised in the isolated release test, not by disrupting live
peers again. Longer canary observation and seven-day soak sign-off remain pending.

## 2026-09-09 — node2 sync-status release rollout

CI for the canary rollout record (`9993cab`) passed:
[run 34324472050](https://github.com/vtorrent/vtorrent-ng/actions/runs/34324472050).
At approximately 08:54 UTC, node3's preceding hour of Prometheus history showed
240 successful 15-second scrapes, minimum availability 1, minimum peer count 1,
and maximum sampled fleet lag one block. Node3 was healthy with zero restarts;
all three current RPC tips agreed at height 3282 before proceeding.

Only node2 was upgraded to `vtorrent/node:c9d00a6`, image ID
`sha256:01c51d65ec2e147e5debf2255106f4a7097e993014b03ba355debab468d79821`.

- Its previous executable and both stopped data locations were privately backed
  up under `.ops-backups/node2-sync-20260909-8X0Lwy/`. The private README records
  checksums, prior image identity, and rollback precautions. Backups are local only.
- Graceful stop: `2026-09-09T08:55:28.857644078Z`, exit 0.
- Stopped chain database SHA-256:
  `82fb1e14992602ebe5b4b495d0b42b0d34363e897c870bb5040bb2fb49859add`.
- New container: `5aa79aa6df9eef83952caa699036e8ef1ec952d8176e6728d93c7aa52bc421a1`,
  started at `2026-09-09T08:55:29.237602093Z`, using
  `up -d --no-deps --no-build --pull never --force-recreate node2`.
- Named volume `vtorrent-testnet_vtr-data2` and the previous anonymous volume
  were retained, with runtime arguments and network isolation unchanged.
- Installed binary SHA-256 matched the tested release artifact:
  `6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
- Replay completed at stored height 3282 at `08:56:08.105057Z`; RPC listened
  at `08:56:08.105587Z`. No startup error or derived-state repair warning appeared.
- Handshake completed at `08:56:08.232940Z`; node2 caught up to height 3283
  at `08:56:08.448922Z`.
- Both followers reported one peer, `syncing: false`, and 100%; node2 was healthy
  with zero restarts. Node1 remained staking. Node1/node3 start times were unchanged.
- A fresh post-reconnection block, height 3284, was accepted by node2 at
  `08:57:53.755182Z`, hash
  `3c3e8eea147ff74ccab9073f56569edd2666cbc24ec5caa81837d9748bbff15f`.

Node2's RPC availability interruption was approximately 39 seconds. Exclude that
interval from uninterrupted three-node availability. Node1 remains on
`vtorrent/node:84125ea`; production seeds, BTC, and monitoring were untouched.
The seven-day soak remains pending.

## 2026-09-09 — node1 sync-status release rollout

CI for `ecb7829` passed:
[run 34332159332](https://github.com/vtorrent/vtorrent-ng/actions/runs/34332159332).
Both followers were healthy with zero restarts. Their preceding hour of recorded
Prometheus history showed 240 successful scrapes each, minimum one peer each,
and maximum sampled fleet lag one block. Current tips agreed at height 3350.
The existing protected-file wallet unlock succeeded before stopping node1;
no wallet re-import or passphrase change was needed.

Only node1 was upgraded to `vtorrent/node:c9d00a6`, image ID
`sha256:01c51d65ec2e147e5debf2255106f4a7097e993014b03ba355debab468d79821`.

- Private backup: `.ops-backups/node1-sync-20260909-kyKnJw/` (0700), containing
  both stopped data locations, previous binary, and a 0600 copy of the existing
  passphrase paired with the encrypted wallet. Nothing private is committed.
- Graceful stop: `2026-09-09T11:39:15.126390494Z`, exit 0.
- Chain database SHA-256:
  `f77e9d2fe52e942dfe4fa2bac7fac1e05cf4e46c992a9ee693bb96dda6857d4a`.
- Encrypted wallet SHA-256 was unchanged:
  `d6e069dd2ae5ec594e7e19e06f862e87c345a6257ef32563d332e4e64e4b84a7`.
- New container: `8e01696cef7f27c96fdd1393296c02221e3763c151f82a5c2c5097a04a2966de`,
  started at `2026-09-09T11:39:15.504161388Z` using
  `up -d --no-deps --no-build --pull never --force-recreate node1`.
- Named and anonymous volumes were preserved, along with BTC seed, networking,
  and all other runtime arguments. Installed binary SHA-256 matched
  `6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
- Replay completed at height 3350 at `11:39:55.414994Z`; RPC listened at
  `11:39:55.415940Z`, with no startup WARN/ERROR or derived-state repair warning.
- The first readiness GET hit a transient connection reset before any unlock
  POST. The private helper was adjusted to tolerate readiness resets/remote
  closes; unlock POSTs remain single-attempt. Protected-file unlock auto-resumed
  staking at `11:39:55.979020Z`.
- First new stake: height 3351 at `11:41:17.459201Z`, hash
  `83d554ba54b28a094f01fa1bbf1990bf0d6b14a256bc4910b27ff61323b25340`.
  All three RPC endpoints subsequently matched that height/hash and reported
  `syncing: false`, 100%, and connected peers.
- BTC-regtest SPV remained initialized and synced at height 140, with the same
  address and balance of 499948000 satoshis as before the upgrade.

RPC interruption was approximately 40 seconds; stop-to-staking-resume was
approximately 41 seconds. Followers were not restarted, but lost their seed
connection temporarily. Exclude the maintenance/reconnection interval from
uninterrupted three-node availability. All three nodes now use the same release.
Production seeds, BTC, and monitoring were not redeployed. Backups remain local
only. Leave the fleet stable for observation; the seven-day soak is not signed off.

## 2026-09-13 — daily soak observation (read-only, day 4 of 7)

No fleet action taken. All checks were read-only (RPC, Prometheus, `docker logs`,
`docker stats`/`inspect`); no restarts, upgrades, or volume changes.

- All three nodes agree at height 5348, hash
  `d9859382cc4cb5f377f73320b5630110b4cd029b4dd2d25425b912317e5cd710`,
  `syncing: false`, 100%, mempool 0. Node1 (staker) holds 2 connections;
  each follower holds 1.
- Containers `Up 3 days`, restart count 0, start times unchanged
  (node3 09-09T07:26, node2 08:55, node1 11:39 UTC).
- Prometheus 24h: `min_over_time(up) = 1` on all three (zero scrape outages);
  chain growth +540 blocks/24h (~1 per 160s, steady, no stalls); all new blocks
  staked by node1 as designed. `min_over_time(peer_count) = 0` on all nodes:
  transient 0-peer dips still occur and self-heal; `max(syncing) = 1`
  in-window, attributed to those dips. Follow up at sign-off review.
- Logs 24h: node2/node3 clean (no ERROR/panic/reorg/rollback/ban lines).
  Node1 shows only the known self-inflicted 172.20.0.1 ban-manager lines from
  the 09-10/09-12 bulk-join attempts; last rejection 2026-09-12 13:02 UTC with
  zero ban activity in the ~12.5h since. No panics, reorgs, or rollbacks.
- Memory (Docker stats, point-in-time): node1 101.6, node2 77.0, node3
  74.2 MiB. Node2 flat at ~77 after yesterday's 64→77 tick; watch continues.
- Repo `main` at `a7cddbf`, CI green. Seven-day sign-off remains pending;
  earliest review 2026-09-16 after 11:42 UTC.

## 2026-09-13 — PEX top-K randomization release rollout

Approved after green CI run
[34733095117](https://github.com/vtorrent/vtorrent-ng/actions/runs/34733095117)
for `7db5da3` (all required jobs green) and the exact release daemon passing all
18 process-recovery tests (`cargo test -p vtorrent-daemon --release --test
process_recovery`). The change is preventive hardening (`fix(p2p): randomize PEX
candidate selection within top-K pool`) with no wire change: `AddrBook::get_candidates`
now draws uniformly from the top `count × 4` pool by quality score instead of a
deterministic top-N, removing the predictable dial order for eclipse attempts.
Soak observations earlier today showed no eclipse indicators; the previous
seven-day soak is **reset** by this fleet upgrade — earliest sign-off is now
**2026-09-20 after 04:31 UTC** (completion of the final follower reconnection).

Image `vtorrent/node:7db5da3`, ID
`sha256:1308257446d5a6a3c381dcdeb693a1dfb36ddc0c7337ccb77978ef4753c69935`.
Source revision: `7db5da31ed87ff18e3b86db7e1ef7d9d0c272374`.
Binary SHA-256:
`fda13af3a58c4d19a4eec86246273ddc9abca7f8ac2ff20eb30ab8cb33e821aa`.
The exact archive is private under
`.ops-backups/pex-20260913-97BzX7/vtorrent-node-7db5da3.tar`, SHA-256
`30f9d5d0b799e1d66f86702418910e4b6293dae5bfda842938aca247aeb39b0a`.
Build used the checked-in release-artifact recipe and minimal binary-only
`image-context/`; verified base is `vtorrent/node:soak`, ID
`sha256:db00ea4eeb1f3c46647dc586c346bfb96d428bf4843ff743ba4781f2920d8ba2`.
No image was pushed to a registry; backups remain local only. Compose now
references `vtorrent/node:7db5da3` for all three services.

### node3 canary

- Preflight at `04:15:54Z`: all three tips agreed at height 5414,
  `31648ee55abe`, healthy.
- Old binary and stopped `/data/node3` and anonymous volume contents were
  privately backed up in `.ops-backups/node3-pex-20260913-y8cevv/` (0700).
  Its `vtorrent-daemon.previous` SHA-256 matches the prior release
  `6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
- Graceful stop: `2026-09-13T04:24:37.14263805Z`, exit 0. Stored chain
  database SHA-256: `a6c91dc1033131f7cb55a7df8c2e58c68e21a676ab4a65aedeaefdd06d1b315c`.
  Chain height at stop was 5414; resuming height after stop was 5417 due to
  chain growth during the brief window.
- New container: `d396e3d60a1c470f3272353fdb656970c838cfb2525a0782ec630f2457d3e7f8`,
  started at `2026-09-13T04:24:56.344180924Z` using
  `up -d --no-deps --no-build --pull never --force-recreate node3`.
  Named volume `vtorrent-testnet_vtr-data3` and the existing anonymous volume
  were preserved.
- Installed binary SHA-256 matched the tested artifact
  `fda13af3a58c4d19a4eec86246273ddc9abca7f8ac2ff20eb30ab8cb33e821aa`.
  Replay completed at height 5417 at approximately `04:26:00Z`; RPC listened
  after replay. No startup WARN/ERROR or derived-state repair warning appeared.
- Peer handshake recovered: node3 held 1 peer after reconnection. All three
  tips agreed at height 5417, `9f2f24f5540a`, with `syncing: false`, 100%.
  Node3 was healthy with restart count 0.
- Fresh block 5418 at `04:26:44Z` and 5420 at `04:30:57Z` both propagated; the
  canary reported healthy throughout. No ban or reorg lines.

RPC interruption for node3 was approximately 39–67 seconds (replay through
health). Exclude that interval from uninterrupted three-node availability.

### node2 follower

- Preflight at `04:26:52Z`: all three tips agreed at height 5418,
  `292145f3653b`, healthy.
- Private backup: `.ops-backups/node2-pex-20260913-NncyFT/` (0700), containing
  both stopped data locations and previous binary
  `6c12aa9e8849448257dcc16837ce33b6f9d72811a51a467ab90ed9f1cf80f5d3`.
  Chain database SHA-256: `e37e0ec949d44ee0b9b03ac9b287be751a5cc10e12a63da3ba458b6df391bc46`.
- Graceful stop: `2026-09-13T04:26:52.347626951Z`, exit 0.
- New container: `0ad71383c1e6be56354db959c999e91ed70476ff965b65ee7ea9a42edbed6341`,
  started at `2026-09-13T04:26:56.800491402Z`.
  Named volume `vtorrent-testnet_vtr-data2` and anonymous volume were retained.
- Installed binary matched the tested artifact. Replay completed at height 5419
  at approximately `04:28:01Z`; RPC and health followed. Node2 was healthy with
  restart count 0 and reported 1 peer, `syncing: false`, 100%.
- Follow-up: all three tips agreed at height 5419, `82027b476e4f`, after node2
  recreation.

RPC interruption for node2 was approximately 65–85 seconds including replay.

### node1 staker

- Preflight at `04:28:22Z`: all three tips agreed at height 5419,
  `82027b476e4f`, healthy. Node1 staking enabled (4 UTXOs,
  `blocks_staked` 2069 before stop).
- Private backup: `.ops-backups/node1-pex-20260913-FzJDF2/` (0700), containing
  both stopped data locations, previous binary, staking intent, encrypted wallet,
  and a 0600 copy of the existing passphrase. Chain database SHA-256:
  `21aeee46ebe49c4f00d51b82c30a6519636b939173b1142b8507a6caaef256bf`.
  Encrypted wallet SHA-256 was unchanged from the prior recovery.
- Graceful stop: `2026-09-13T04:28:22.826676087Z`, exit 0.
- New container: `54531aef934a23e49421fdc647d33ed853146aaf421be8544621ae35afa0f421`,
  started at `2026-09-13T04:28:27.072169902Z` using
  `up -d --no-deps --no-build --pull never --force-recreate node1`.
  Named and anonymous volumes were preserved, along with BTC seed, networking,
  and all runtime arguments. Installed binary matched the tested artifact.
- Replay completed at height 5419; RPC listened and BTC-regtest SPV
  re-scanned 141 blocks through height 140. Wallet restored locked.
- Protected-file unlock auto-resumed staking at `2026-09-13T04:29:48.792709Z`
  via `python3 .ops-backups/node1-image-20260908-nHCswr/wallet-ops.py unlock`
  (single-attempt POST, staking address `VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k`
  verified). Node1 initially reported 0 peers; all three nodes reconnected
  at `04:30:01–04:30:02Z` (node1 2 peers, each follower 1 peer).
- First new stake: height 5420 at `2026-09-13T04:30:57.260862Z`, hash
  `cc659f6843d8a8b35f72bc8dcb42c7b4d169308bd5484669c552b815ac3c2875`.
  All three RPC endpoints subsequently matched that height/hash and reported
  `syncing: false`, 100%, and connected peers.
- BTC-regtest SPV remained initialized at height 140.

RPC interruption was approximately 60–65 seconds; stop-to-staking-resume was
approximately 86 seconds; stop-to-peer-reconnect was approximately 99 seconds.
Followers were not restarted during node1's window but lost their seed
connection temporarily. Exclude the maintenance/reconnection interval from
uninterrupted three-node availability. All three nodes now use the same release
`7db5da3`. Production seeds, BTC, and monitoring were not redeployed. Backups
remain local only. Leave the fleet stable for observation; the seven-day soak is
not signed off.

## 2026-09-13 — operator host reboot for user/hostname change (soak interrupted)

Operator-initiated host reboot: laptop restart after changing to a new user and
hostname (working directory is now `/home/pnoch`). No fleet deploy, upgrade,
or volume change was performed; all three nodes still run local image
`vtorrent/node:7db5da3`. This was an availability interruption, not a
version change. Exclude the window below from uninterrupted soak measurements;
uninterrupted evidence restarts at the first post-recovery stake.

- Prometheus `up{job="vtorrent-nodes"}` (15s scrapes): last success
  `09:25:30Z`, failures `09:25:45Z` through the reboot; Prometheus itself was
  down ~`09:58Z`–`10:03:45Z` (no data points, not zeros). Nodes scraped `1`
  again from `10:05:15Z`.
- Host reboot: `2026-09-13T09:53:58Z` (`16:53:58 +07`, `last reboot` /
  `journalctl --list-boots`; prior boot since 2026-08-03). The ~28 minutes of
  scrape failures preceding the reboot timestamp indicate the host was already
  degrading or shutting down before the recorded boot line.
- Containers restarted `2026-09-13T10:04:01–02Z` (node1 `10:04:01Z`, node3
  `10:04:02Z`, node2 `10:04:02Z`), restart count 0 (fresh boot, not docker
  restarts). Named and anonymous volumes preserved; no data loss.
- Replay completed at stored height 5550 on all nodes
  (`7987796112049f0235c6182cd893e446fa32aff46bd67004a7cdb578f49619b6`);
  no startup WARN/ERROR, derived-state repair warning, reorg, or rollback.
- Peers reconnected `10:05:09–10:05:14Z` (node1 2 peers, each follower 1 peer,
  seed `node1:22526`). All three RPC endpoints agreed at height 5550 with
  `syncing: false`, 100%, mempool 0. Post-restart log scan: zero ERROR/panic/
  reorg/rollback/ban lines on all three nodes.
- Node1 wallet was restored locked after the reboot, so staking was disabled
  and the chain stalled at 5550. `staking.json` intent remained
  `enabled: true` for `VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k`; no re-import was
  needed. Protected-file unlock via
  `.ops-backups/node1-image-20260908-nHCswr/wallet-ops.py unlock`
  (single-attempt POST) auto-resumed staking at approximately `10:11Z`
  (4 UTXOs, same address verified).
- First new stake: height 5551 at `2026-09-13T10:12:19Z`, hash
  `b070b21659e10b86f28198e2e78bfd8a4889d6c76b01af2eb57519e8e11d3cac`,
  accepted by both followers the same second. All three endpoints agree with
  `syncing: false`, 100%. Memory (Docker stats, point-in-time): node1 69.0,
  node2 62.1, node3 61.9 MiB.
- BTC-regtest SPV on node1 re-scanned 141 blocks through height 140 with no
  error. Production seeds, BTC, and monitoring configuration were untouched.
- CI is green for all recent pushes (`34743228890`, `34743039777`,
  `34738118792` all success).

Soak impact: the PEX-rollout window (earliest review 2026-09-20 after
04:31 UTC) is broken by this host-level interruption. Uninterrupted
three-node evidence restarts at the first post-recovery stake
(`2026-09-13T10:12:19Z`); earliest sign-off is now no earlier than seven
full days after that, contingent on continuous evidence — not an automatic
pass. Next daily observation entry remains due 2026-09-14.

## 2026-09-14 — daily soak observation (read-only, day 1 of 7)

No fleet action taken. All checks were read-only (RPC, Prometheus, `docker
logs`, `docker stats`/`inspect`); no restarts, upgrades, or volume changes.
The observation window opened at the post-reboot recovery stake
(`2026-09-13T10:12:19Z`, height 5551); this entry covers the first full 24h
plus the following day, read at `2026-09-15T01:04Z`.

- All three nodes agree at height 6496, hash
  `28d805f41b9d36ff1009844bdd3189235169804e02cace7e60555412f7d90dca`,
  `syncing: false`, 100%, mempool 0. Node1 (staker) holds 2 connections;
  each follower holds 1.
- Containers `Up 39 hours`, restart count 0, start times unchanged
  (node1 09-13T10:04:01Z, node2 10:04:02Z, node3 10:04:02Z).
- Prometheus 24h: `min_over_time(up) = 1` on all three (zero scrape
  outages); `min_over_time(peer_count) = 2/1/1` — no zero-peer dips this
  window, unlike the 09-13 entry; `max_over_time(syncing) = 0` on all three;
  chain growth +579 blocks/24h (~1 per 149s, steady, no stalls). All new
  blocks staked by node1 as designed.
- Logs since the window start: zero ERROR/panic/reorg/rollback/ban lines on
  all three nodes. Node1's earlier self-inflicted 172.20.0.1 ban-manager
  lines did not recur.
- Memory (Docker stats, point-in-time): node1 129.1, node2 82.6, node3
  83.6 MiB. Node1 showed a warmup climb (69.0 at recovery → 102.7 at
  09-13T15:19Z → 108 at 09-14T01:55Z → 129.1 now). Follow-up on 09-15
  found this **plateaus**: VmRSS held 140832→140912 kB (+80 kB) over 17.5
  min while 9 blocks were staked, with cgroup `memory.current` flat at
  139.5–141.0 MiB. The climb was glibc malloc arena expansion (5 arenas /
  64 MiB on the staker vs 3 / 25 MiB on followers), not a leak; the
  2026-09-04 fix (`1a3d010`) is intact. See
  `docs/memory-observability-design.md` §7.1. Node metrics expose no
  process-memory series, so this remains point-in-time only.
- Consensus depth check (added 2026-09-15, read-only): the last 100 blocks
  have identical hashes on all three nodes, and the last 50 also match on
  `(hash, merkle_root)` — zero mismatches. No store corruption/self-heal/
  truncation lines since the window start.
- BTC-regtest SPV on node1: handshakes every 5 min against the local regtest
  peer at height 140, no errors. Production seeds, BTC, and monitoring
  configuration were untouched.
- Repo `main` at `8102ef4`; frontend toolchain refreshed this window (pnpm
  10, ESLint 10, Vite 6, Vitest 4, React Router 7, zero audit advisories).
  CI green on push runs; the weekly scheduled Cargo Audit job had been
  failing since 2026-08-17 on a missing `issues: write` permission (the
  action files an issue on `schedule` events, a check on push), fixed in
  `8102ef4`. A stale local lockfile also carried rustls 0.23.43
  (RUSTSEC-2026-0285); a fresh `cargo generate-lockfile` resolves 0.23.45
  and `cargo audit` exits 0.

Soak impact: none — this window is uninterrupted. Earliest seven-day
sign-off remains no earlier than `2026-09-20T10:12Z`, contingent on
continuous evidence. Next daily observation entry due 2026-09-15.

## 2026-09-17 — C1 stake-kernel fix deployed (fleet upgrade, window reset)

The C1 consensus fix (`a3dd177`, "normalize stake kernel by total staked
supply") was deployed to all three nodes. This is a consensus-rule change, so
the fleet was stopped and recreated together rather than rolled — a mixed
fleet would fork, because v2 is *easier* than v1 at the current
`total_staked` (~504 VTR), so a v2 node would produce blocks the v1 nodes
reject.

**Pre-deployment verification (non-destructive).** The new image was run
against a copy of node3's live data before any fleet action. It replayed the
entire chain to height 8010 with the **exact same tip hash** as the running
old binary (`a5bc409f2deb1ea3eb7543a5be33c42f4ed295b5de896a3d2e773503a06f0b04`)
and zero errors, confirming replay compatibility.

**Backup.** All three `/data/nodeN` volumes were copied to
`vtr-preupgrade-backup` before the upgrade (chain.db 51.9/71.0/51.9 MB). The
pre-upgrade tip was height 8215, hash
`2ca142ec88361e36b3e07c19825cb29f2453d97b202bf27791af9c40e2c8e57f`, agreed by
all three nodes.

**Rollout.** `docker compose stop node1 node2 node3` (clean stop, all at the
same tip), compose image pin `7db5da3` → `eb4dd0c`, then
`docker compose up -d`. All three replayed to the pre-upgrade tip with zero
errors. Staking did not auto-resume because the wallet restores locked; the
documented unlock helper
(`.ops-backups/node1-image-20260908-nHCswr/wallet-ops.py unlock`) restored
staking for the same address with the same four UTXOs.

**Result.** All three nodes agree and are producing blocks under the v2 rule
at **61.0s average over six consecutive intervals** (the 60s target), versus
~146s under v1. Zero ERROR/panic/reorg lines since the upgrade. Staking
enabled, 4 eligible UTXOs.

**Soak impact.** The seven-day window is **reset** by this fleet upgrade.
Earliest sign-off is now no earlier than seven days after the upgrade
(`2026-09-24`), contingent on uninterrupted evidence. The previous window
(`2026-09-13T10:12:19Z`) is superseded.

**Rollback.** Requires restoring the `vtr-preupgrade-backup` volumes and
re-pinning `7db5da3`; the old binary cannot replay a v2-produced chain (v1 is
harder), so rollback is not possible without the volume restore.

## 2026-09-18 — host restart stalls staking; recovered

All containers (nodes, Prometheus, Grafana, BTC regtest) restarted together at
`2026-09-18T13:46:03Z` and came back at `13:46:07Z`. The cause was external to
the daemon — every container on the host restarted in the same second, the
Docker daemon itself did not restart, and there is no cron/watchtower job that
would do this. The restart was graceful: all three nodes exited 0 and no
daemon error preceded it. Cause not definitively identified; treat as an
unexplained host-level event.

**Impact.** The chain was intact — all three nodes replayed to height 9191
(`9c5a22d443e4a302`) with zero ERROR/panic/reorg lines. But staking did **not**
resume: the wallet restores locked, so the chain stalled at 9191 for ~46
minutes (last block `13:45:10Z`, staking restored ~`14:33Z`).

**Recovery.** The documented unlock helper
(`.ops-backups/node1-image-20260908-nHCswr/wallet-ops.py unlock`) restored
staking for the same address and four UTXOs. Blocks resumed immediately
(9191 → 9194 within two minutes, three stakes).

**Soak impact.** This is a staking interruption, so the seven-day window is
reset again. Earliest sign-off is now no earlier than seven days after
staking resumed (`2026-09-25`), contingent on uninterrupted evidence. The
previous window (`2026-09-17T21:09Z`) is superseded.

**Follow-up.** The wallet-restores-locked behaviour means any host restart
stalls the chain until an operator unlocks. Consider persisting the staking
intent so it can auto-resume without the passphrase, or documenting the
unlock step in the on-call runbook as a mandatory post-restart action.

## 2026-09-19 — S4/S5/M8/M10/M15 deployed (fleet upgrade, window reset)

The pending non-consensus fix batch was deployed to all three nodes:
`eb4dd0c` → `2160eb9`.

**Pre-deployment verification (non-destructive).** The new image was run
against a copy of node3's live data before any fleet action. It replayed the
entire chain to height 9759 with the **exact same tip hash** as the running
old binary (`5f9a2b0fb663702ac92708bfe14f83bb33ea160c28b291aeb02591395bde5fe2`)
and zero errors.

**Backup.** All three `/data/nodeN` volumes copied to
`vtr-preupgrade-backup` (49 MB each). Pre-upgrade tip was height 9761
(`751285e9f55bb6ea2e4f37a97cfea72117ebb68f67edb7d9454e5c7fc7e93e6d`),
agreed by all three nodes.

**Rollout.** All three stopped together, compose image pin bumped, recreated.
All three replayed to the pre-upgrade tip with zero errors. Staking restored
with the documented unlock helper (wallet restores locked).

**Live verification of the deployed fixes:**
- S4 — `POST /api/v1/swap/btc-claim-bump` is present and validating (400 on a
  malformed body, not 405).
- M10 — `POST /api/v1/wallet/import` on the running node returns
  `A wallet is already imported; set "overwrite": true to replace it`.
- M8/M15 — covered by unit tests; the tracker announce error is swallowed by
  `if let Ok(...)` in the engine, so a live rejection is not observable in the
  logs (noted as a logging gap, not a functional one).

**Result.** All three agree and produce blocks at **61.0s average** over six
consecutive intervals. Zero ERROR/panic/reorg lines, zero restarts.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is now
no earlier than seven days after this upgrade (`2026-09-26`), contingent on
uninterrupted evidence. The previous window (`2026-09-18T14:33Z`) is
superseded.

**Rollback.** Restore the `vtr-preupgrade-backup` volumes and re-pin
`eb4dd0c`. This batch is not consensus-breaking, so unlike the C1 upgrade the
old binary can replay a chain produced by it.

## 2026-09-19 — wallet auto-unlock deployed (staking resumes without operator)

Deployed `625de28` to all three nodes, adding `--wallet-passphrase-file` so
the daemon unlocks the wallet at boot and the existing staking-intent
auto-resume re-enables staking. This removes the post-restart stall hazard
that caused the 2026-09-18 ~46-minute outage.

**Pre-deployment verification.** The new image replayed a copy of node3's
live data to height 9820 with the exact same tip hash as the running old
binary (`884925f7562eff80518c415cab089e7d1e0a9601472cb8fa68735d8e5b45c6de`),
zero errors. Volumes backed up to `vtr-preupgrade-backup` (49 MB each);
pre-upgrade tip 9823 (`7a8113cb`).

**Rollout.** All three stopped together, image pin bumped, recreated. All
three replayed to the pre-upgrade tip with zero errors.

**Verification of the fix.** No manual unlock was performed at any point:
- On first start, node1 logged `Restored encrypted wallet … (locked)` then
  `Wallet auto-unlocked from /run/secrets/wallet-passphrase` and
  `Auto-resuming staking for VDR9EJdw…`.
- `docker compose restart node1` (the exact 09-18 scenario) again
  auto-unlocked and auto-resumed; staking status `enabled: true`, 4 UTXOs,
  and the chain advanced 9827 → 9830.

**Security.** The passphrase is read from a 0600 file mounted read-only at
`/run/secrets/wallet-passphrase`, sourced from the gitignored
`.ops-backups/` path. It never enters argv, the environment, `docker
inspect`, or logs. An unreadable passphrase file aborts startup (fail
closed) rather than silently running locked.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is
now no earlier than seven days after this upgrade (`2026-09-26`), contingent
on uninterrupted evidence.

## 2026-09-19 (later) — auto-unlock hardening deployed (96aef91)

Follow-up to the wallet auto-unlock deployment, fixing two gaps that would
have silently recreated the stall the feature prevents:

- Docker creates a *directory* when a bind-mount source is missing, so the
  original existence-only pre-flight would pass, the daemon would start, and
  the read would fail — leaving the wallet locked and staking stalled.
  `validate_passphrase_file` now rejects anything that is not a regular file.
- The compose mount used the short syntax, which also silently creates the
  missing source. Switched to the long syntax with
  `bind.create_host_path: false`, so a missing passphrase file fails the
  deploy with `bind source path does not exist` and creates nothing.

**Verified before rollout.** With `VTORRENT_WALLET_PASSPHRASE_FILE` unset,
`docker compose up -d node1` now hard-fails and creates no directory at the
fallback path (previously it would have started a broken container).

**Rollout.** Volumes backed up (49 MB each), all three stopped together,
image pin `625de28` → `96aef91`, recreated. No manual unlock performed:
node1 logged `Restored encrypted wallet … (locked)` → `Wallet auto-unlocked
from /run/secrets/wallet-passphrase` → `Auto-resuming staking for
VDR9EJdw…`. Chain advanced 9957 → 9961 with zero errors.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is
now no earlier than seven days after this upgrade (`2026-09-26`).

## 2026-09-19 (later) — M12 + DoS + low-severity batch deployed (1226e4f)

Deployed the accumulated non-consensus batch: M12 (BTC spend authorization),
M2/M5/M7/M9/M11/M17 (resource-exhaustion bounds), M16 (`fork()` safety), and
L7/L8/L10/L18 plus the deferred `DNS_SEEDS` seed3 entry.

**Pre-deployment verification.** The new image replayed a copy of node3's live
data to height 10769 with the exact same tip hash as the running old binary
(`12ecb6706fd033ac9149c469838cd3c925e661ac2e6219ad05800397bb42df14`), zero
errors. Volumes backed up to `vtr-preupgrade-backup` (52 MB each); pre-upgrade
tip 10773 (`1548a22b`).

**Rollout.** All three stopped together, image pin `96aef91` → `1226e4f`,
recreated. Auto-unlock and staking auto-resume worked with no manual action
(`Wallet auto-unlocked from /run/secrets/wallet-passphrase` →
`Auto-resuming staking for VDR9EJdw…`). All three replayed to the pre-upgrade
tip with zero errors.

**Live verification of the deployed fixes:**
- L18 — `POST /api/v1/spv/headers` with 2001 headers returns
  `Too many headers in one request (2001 > 2000)`.
- M12 — `POST /api/v1/btc/send` passes the unlock gate (wallet unlocked) and
  fails on address validation, confirming the gate is on the path.

**Result.** All three agree and produce blocks at 61s intervals after the
restart gap. Zero ERROR/panic/reorg lines.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is now
no earlier than seven days after this upgrade (`2026-09-26`).

## 2026-09-20 — L5/L6 + M16 + low-severity batch deployed (38ef9de)

Deployed the final fix batch: L5/L6 (legacy-claim fund safety), M16 (`fork()`
safety), and L7/L8/L10/L18 plus the `DNS_SEEDS` seed3 entry.

**L5 is a consensus-rule change**, so it was treated like C1: verified
replay-safe before rollout. The rule now requires a claim to match the
snapshot balance exactly (previously any amount up to the balance was
accepted, stranding the remainder). A full-chain scan confirmed the running
chain contains **no legacy claims** (sampled 116 blocks across the whole
chain; none had more than one transaction), so replay re-validates nothing
against the old rule.

**Pre-deployment verification.** The new image replayed a copy of node3's live
data to height 11158 with the exact same tip hash as the running old binary
(`cf071c9bd463bd63e5fe080894edca45166b8eb3da55513b085d97cc3a84426d`), zero
errors. Volumes backed up (53 MB each); pre-upgrade tip 11161 (`ad007a61`).

**Rollout.** All three stopped together, image pin `1226e4f` → `38ef9de`,
recreated. Auto-unlock and staking auto-resume worked with no manual action.
All three replayed to the pre-upgrade tip with zero errors.

**Result.** All three agree and produce blocks at 61s intervals. Zero
ERROR/panic/reorg lines.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is now
no earlier than seven days after this upgrade (`2026-09-27`).

## 2026-09-20 (later) — final low-severity batch deployed (13489d4)

Deployed the last actionable review batch: L3 (startup lock order), L4 (block
size estimate + bits), L9 (Tor control reply timeout), L11 (onion/i2p
dialing), L12 (overlay ingest ordering), L17 (WebSocket cap + idle timeout),
L19 (rate limiter cost), L20 (constant-time compare length leak).

**Pre-deployment verification.** The new image replayed a copy of node3's live
data to height 11213 with the exact same tip hash as the running old binary
(`f484d0c0f57842363dabe96ba05fd3a47e048d656bea5677fe078a44e7a03b3c`), zero
errors. Volumes backed up (53 MB each); pre-upgrade tip 11216 (`e88d2681`).

**Rollout.** All three stopped together, image pin `38ef9de` → `13489d4`,
recreated. Auto-unlock and staking auto-resume worked with no manual action.
All three replayed to the pre-upgrade tip with zero errors.

**Result.** All three agree and produce blocks at 61s intervals. Zero
ERROR/panic/reorg lines.

**Soak impact.** The seven-day window is reset again. Earliest sign-off is now
no earlier than seven days after this upgrade (`2026-09-27`).

**Review status.** With this batch, every finding from both 2026-09-15 review
passes is fixed or explicitly documented as accepted. The only item not fixed
is M6 (DHT source validation), which the torrent DHT already handles.

## 2026-09-20 — production seed fleet upgraded (stale binary + false-positive alert)

While verifying the post-soak checklist, found that the three production seeds
were running a **stale binary** (built 2026-08-29, `2.0.0-beta.2`) — 217
commits behind `main`, missing every fix from the 2026-09-15 review and the
protocol-v3 change (`c5c863b`, 2026-09-02).

**Symptom.** `PeerCountZero` was firing on seed3. It was a false positive: the
node's own log said `Peers: 4` and `/peers` returned `count: 4`, but the
`vtorrent_peer_count` metric reported `0`. The stale binary only refreshed
sync status on the event-bridge *lag* path; the fix (`c9d00a6`, 2026-09-09)
was not deployed. `/info` also misreported `connections: 0`.

**Also corrected:** the checklist item "seed3 monitoring not applied" was
stale — the deployed `prometheus.yml` on seed1 and the nginx config on seed3
are byte-identical to the repo, and all six targets were already `up`.

**Upgrade.** Built `target/release/vtorrent-daemon` from `main`
(sha256 `920a733b53c76f4d85507f3d6cb665a3a1cc082d2bc8512d57a5609f7c4809b3`),
verified it against a copy of seed1's real data, then upgraded all three
together (the protocol version is a hard boundary — `70001` → `3` — and the
seeds peer only with each other, so a partial upgrade would partition them).

- Backed up each seed's data dir and old binary to
  `/root/vtorrent-preupgrade-<ts>/` (36 MB each).
- Stopped the service, installed the new binary, removed the legacy `chain.db`
  (the new store rejects pre-protocol-3 stores; the seeds are at height 0, so
  nothing was lost), restarted.
- `overlay.key` and `peers.dat` were preserved, so node identity is unchanged.

**Result.** All three seeds report `version=2.0.0-beta.2`, `height=0`,
`connections=4`, `syncing=false`. The `vtorrent_peer_count` metric now reads
4 on all three, all seven Prometheus targets are `up`, and the `PeerCountZero`
alert has cleared.

**Note.** The seeds are mainnet and the soak fleet is regtest, so this upgrade
does not affect the soak window. The seeds now carry the C1 consensus rule and
all review fixes, which is what mainnet launch requires.

## 2026-09-20 (later) — MALLOC_ARENA_MAX=2 pinned on the soak fleet

Node1's staker had reached **154.4 MiB RSS — over the <150 MiB budget** — with
4 glibc malloc arenas (53 MiB). RSS was flat over a 10-minute sample (+308 kB
while 8 blocks staked), confirming allocator high-water rather than a leak.

**Measured before applying.** Ran the current image against a copy of node1's
live data (isolated, wallet unlocked, staking) with `MALLOC_ARENA_MAX=2`:
arenas dropped from 4 to **1** and RSS from **154 MiB to ~115 MiB**, stable
across four samples while staking (111.5 → 115.1 MiB, 2 → 6 blocks).

**Applied.** Added `MALLOC_ARENA_MAX: "2"` to all three nodes in
`docker/testnet/docker-compose.yml` and recreated the containers (same image,
config-only change). Auto-unlock and staking auto-resume worked with no manual
action.

**Result.** All three agree at height 11349, `syncing: false`. Arenas are 1 on
all three; RSS is **123.5 / 109.1 / 109.1 MiB** — node1 is now ~31 MiB below
where it was and comfortably under the 150 MiB budget.

**Soak impact.** This was a container recreate, not an image change, so the
chain and staking continued without a window reset. The seven-day window is
unchanged (earliest sign-off 2026-09-27).

## 2026-09-20 — seven-day window baseline (read-only)

Baseline for the current seven-day window, which started when the
`MALLOC_ARENA_MAX` containers were recreated at `2026-09-20T03:22:35Z`. Read at
`2026-09-20T07:07Z` (~3h20m in). All checks read-only; no fleet action.

- All three nodes agree at height **11568**, hash
  `38b97354e0dbdbec1ac98ba1dfe063a53ca8f8e0258bc8ebe017e297693496c8`,
  `syncing: false`, mempool 0. Node1 (staker) holds 2 connections; each
  follower holds 1.
- Staking enabled for `VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k`, 4 eligible UTXOs,
  222 blocks staked.
- Prometheus over the window: `min_over_time(up) = 1` on all three (zero
  scrape outages); `min_over_time(peer_count) = 2/1/1` (no zero-peer dips);
  `max_over_time(syncing) = 0` on all three; `delta(block_height) = +177.2`
  on all three (~61s/block).
- Scrape coverage: 720/720 expected 15s samples on each node (100%).
- Zero ERROR/panic/reorg/rollback/rejected lines on all three since the
  window start; restart count 0 on all three.
- Memory: node1 114.7, node2 99.0, node3 96.8 MiB, **1 malloc arena each**
  (the `MALLOC_ARENA_MAX=2` cap is holding). Node1 is ~40 MiB below its
  pre-cap 154 MiB.

Earliest sign-off remains **2026-09-27T03:22Z**, contingent on uninterrupted
evidence. Next daily observation entry due 2026-09-21.

## 2026-09-21 — host Docker daemon conflict partitioned the fleet; recovered

**Symptom.** `docker ps` returned nothing while the RPC ports still answered
with 22h uptime. Node1 kept staking (height 11689) but node2/node3 were
stalled at 11627 with `connections=0`.

**Root cause.** Two Docker daemons were running on the host:

| Daemon | Started | Data root | Containers |
|---|---|---|---|
| snap `dockerd` (pid 1564872) | Sep 18 20:45 | `/var/snap/docker/...` | 12 (the soak fleet) |
| system `dockerd` (pid 2353143) | **Sep 20 15:07** | `/var/lib/docker` | 0 |

The system `docker.service` (enabled, and it should not have been) started at
15:07 on Sep 20 and took over `/run/docker.sock`. The CLI therefore talked to
the *empty* system daemon, while the snap daemon still owned the real
containers. The snap daemon was still alive (pidfile intact, 31 sockets open)
and the three `vtorrent-daemon` processes kept running — which is why the RPC
ports responded.

**Impact.** Node2 and node3 lost the `vtrnet` bridge interface (their network
namespaces held only `lo`), so they were partitioned from node1 and stalled.
Monitoring was blind: `docker ps`/`logs`/`stats`/`inspect` all queried the
wrong daemon, so `scripts/soak-status.sh` reported misleading values.

**Recovery.**
1. Backed up all three node volumes to `vtr-preincident-backup` (55 MB each).
2. `systemctl stop docker.service docker.socket` and **disabled** both, so the
   stray daemon cannot recur at boot.
3. `snap restart docker` to re-establish the snap daemon's socket.
4. The CLI immediately saw the real containers; the fleet restarted cleanly.

**Result.** All three nodes replayed to the pre-incident tip and now agree at
height 11695, `syncing: false`, peers 2/1/1, all containers healthy. Staking
auto-resumed with no manual action (the `--wallet-passphrase-file` path
worked). Exactly one `dockerd` is running, and the system units are disabled.

**Soak impact.** The fleet restarted, so the seven-day window is reset again.
Earliest sign-off is now no earlier than seven days after this recovery
(`2026-09-28`).

**Follow-up.** The host had two competing Docker installs (snap + apt). The
apt `docker.service`/`docker.socket` are now disabled; a host-readiness check
should assert a single daemon before a soak window starts.

## 2026-09-21 — host lid-close suspend froze the fleet for 16.7h; recovered

**Symptom.** Node1's log stopped at `2026-09-20T08:12:33Z` (last line: PEX
`getaddr` after staking block 11632) and stayed silent for 16.7 hours. All
three nodes froze at the same time; Prometheus marked all three `up = 0` from
`08:12:23Z` to `08:18:08Z`, after which the scrapes themselves stopped
(Prometheus was on the same suspended host, so no data points were recorded —
not zeros). Block height metrics show no samples in the window.

**Root cause.** Operator closed the laptop lid at `2026-09-20T15:13:05 +07`
(`08:13:05Z`). `systemd-logind` suspended the host (`PM: suspend entry
(s2idle)` at `08:13:14Z`). The last node log line is 41s before the kernel
suspend entry, consistent with the fleet being frozen, not crashed. The
container processes were never killed (`RestartCount` 0, `OOMKilled` false,
exit code 0 on the later stop).

**Recovery.** Lid opened `2026-09-21T07:56:19 +07` (`00:56:19Z`); kernel
`PM: suspend exit` the same second. Node1 resumed staking within the same
second (block 11633 at `00:56:19Z`) with no manual action — the staking loop
survived the freeze. Node2/node3 were still partitioned from the earlier
Docker-daemon incident (their bridge was gone), so they stayed at 11627.

At `01:55:31Z` the operator's recovery of the Docker-daemon conflict
(`snap restart docker`, see the previous entry) restarted all containers via
the `unless-stopped` restart policy. All three nodes replayed the full chain
from genesis (node1 1→11691 in ~2m35s, node2 1→11627 then caught up
11628→11695 by `02:02:17Z`, node3 1→11691 then 11692→11702) with **zero
height gaps** in the replay logs, zero ERROR/panic/reorg/rollback lines, and
no store corruption. Wallet auto-unlock re-fired (`01:58:12Z`) and staking
auto-resumed. Peers reconnected (node2 at `01:59:09Z`); all three agreed at
height 11695 by `02:03Z`.

**Result.** Fleet healthy: height 11938, identical best hash
`ad0ba7186aa0…` on all three, `syncing: false`, peers 2/1/1, mempool 0,
staking enabled (4 UTXOs, 248 blocks staked this run). Post-resume block
intervals: n=235, min 61 / median 61 / max 62 s — exactly the 60s target.
Memory: node1 117.9, node2 101.6, node3 102.6 MiB (1 malloc arena each, the
`MALLOC_ARENA_MAX=2` cap holding). Prometheus scrape coverage since the
containers came back: 960/960 expected 15s samples on each node (100%),
zero gaps >30s. BTC-regtest SPV reconnected and stays synced (height 140,
`synced: true`); the BTC container logs "stale tip" warnings because no new
regtest blocks are being mined, which is expected.

**Soak impact.** Two interruptions in one window: the host suspend
(`08:13:14Z` → `00:56:19Z`, 16.7h) and the subsequent container restart
(`01:55:31Z`). The seven-day window resets to the first post-recovery stake
at `2026-09-21T01:58:13Z` (height 11691). Earliest sign-off is now
**2026-09-28 after 01:58Z**.

**Follow-up.** The soak host is a laptop; lid-close suspends it. Before the
next window, disable automatic suspend on lid close (or run the fleet on the
seed servers instead). `soak-status.sh` cannot detect a suspended host — the
scrapes simply stop — so a gap detector over `up` (e.g. `present_over_time`)
would make this visible in Grafana.

## 2026-09-21 (later) — daily observation (post-suspend, window restarted)

Read-only check at `2026-09-21T06:10Z`, ~4h11m after the window restart
(first post-recovery stake `01:58:13Z`, height 11691).

- All three nodes agree at height **11938**, hash `ad0ba7186aa0…`,
  `syncing: false`, mempool 0. Node1 (staker) holds 2 connections; each
  follower holds 1.
- Staking enabled for `VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k`, 4 eligible
  UTXOs, 248 blocks staked this run; wallet auto-unlock re-fired on
  container start with no manual action.
- Block cadence since resume: 235 intervals, min 61 / median 61 / max 62 s.
- Prometheus over the window: `up = 1` on all three with 960/960 expected
  15s samples (100% coverage, zero gaps >30s); `max_over_time(syncing) = 0`;
  `min_over_time(peer_count) = 2/1/1` (no zero-peer dips).
- Zero ERROR/panic/reorg/rollback lines on all three since the window start;
  container `RestartCount` 0 on all three.
- Memory: node1 117.9, node2 101.6, node3 102.6 MiB; 1 malloc arena each
  (`MALLOC_ARENA_MAX=2` holding).
- BTC-regtest SPV: connected, height 140, `synced: true`.

Earliest sign-off: **2026-09-28 after 01:58Z**, contingent on uninterrupted
evidence. Next daily observation entry due 2026-09-22.

## 2026-09-22 — daily observation (window day 1)

Read-only check at `2026-09-22T01:10Z`, ~23h12m into the restarted window
(first post-recovery stake `2026-09-21T01:58:13Z`, height 11691).

- All three nodes agree at height **13057**, hash `c7078b418b0afe9b…`,
  `syncing: false`, mempool 0. Node1 (staker) holds 2 connections; each
  follower holds 1.
- Staking enabled, 4 eligible UTXOs, **1367 blocks staked** this window.
- Block cadence within the window: n=1367, min 61 / median 61 / max 62 s,
  mean 61.0 — exactly the 60s target, **zero intervals >120s**.
- Prometheus since the window start: **5559/5559 expected 15s samples** on
  each node (100% coverage), zero gaps >30s. The 204 `up = 0` samples in the
  trailing 24h are all `01:07:44Z → 01:58:14Z`, i.e. the tail of the Docker
  incident recovery, ending exactly at the window start — not a new event.
- Zero ERROR/panic/reorg/rollback lines on all three; container
  `RestartCount` 0 on all three.
- Memory: node1 131.5, node2 111.9, node3 114.7 MiB (under the <150 MiB
  budget; node1's staker high-water is the known glibc arena effect).
- BTC-regtest SPV: connected, height 140, `synced: true`, balance
  4,999.48 mBTC; VTR SPV header chain at 13058.

Earliest sign-off: **2026-09-28 after 01:58Z**. Next daily observation due
2026-09-23.

## 2026-09-23 — fleet redeploy `13489d4` → `4d1ae47` (redb cache bound)

Operator-approved redeploy to test whether bounding redb's default 1 GiB cache
fixes the RSS growth. **It did not** — recorded here as a disproven hypothesis.

**Preflight.** All three agreed at height 14705, node1 staking (3014 blocks),
node2/node3 healthy. Volumes backed up (stopped, read-only copy) to
`.ops-backups/redb-cache-20260923-FzjyQ/` (0700): `node1/`, `node1-vtorrent/`,
`node2/`, `node3/`.

**Image.** Built from `4d1ae473b110e7bd7d32325f2dd5591c6c69fcec` (includes the
cache bound `65c897f`). Binary SHA-256
`2e47d80e41e2a3383e10463293f2fe11cef924e3e27b96a5d368c409862853b1`, image ID
`sha256:809294d6968b0e88512cbec36a90ec5489b25c5374b4aeafb92acc0e940f33bf`,
tag `vtorrent/node:4d1ae47`. Verified in the running container after deploy.

**Rolling recreate** (one at a time, followers first):
- node3: stop 05:03:2xZ, start 05:03:41Z; replayed genesis→tip in ~3m20s, zero
  errors, `RestartCount` 0.
- node2: stop 05:07:4xZ, start 05:08:0xZ; replayed in ~3m, zero errors.
- node1: stop 05:12:3xZ, start 05:12:4xZ; replayed in ~3m30s; wallet
  auto-unlocked at 05:15:54Z and staking auto-resumed; first stake within a
  minute. `VTORRENT_WALLET_PASSPHRASE_FILE` was set explicitly for the recreate
  (the compose default path does not exist on this host).
- All three agreed at 14716 by 05:18Z; fresh blocks propagate to all three.

**Result — hypothesis disproven.** Growth continued at the same rate on the
fixed binary:

| Node | 05:53 → 06:21 (28 min) | Rate |
|---|---|---|
| node1 | 152212 → 152724 kB | ~1097 kB/h |
| node2 | 134168 → 134452 kB | ~609 kB/h |
| node3 | 133768 → 134060 kB | ~626 kB/h |

node1 settled at ~152.7 MiB (VmHWM 188.9 MiB), over the 150 MiB budget. The
growth is in the `[heap]` region (115.9 MiB of 149.4 MiB total). The 64 MiB
cache bound is retained (a 1 GiB cache on a 150 MiB-budget node is a
misconfiguration) but is **not** the cause. Root cause remains open; next step
is heap profiling on a non-production copy.

**Soak impact.** This is the fourth interruption of the window (after the host
reboot, the Docker-daemon incident, and the host suspend). The seven-day window
resets to the first post-redeploy stake at `2026-09-23T05:16Z`. Earliest
sign-off is now **2026-09-30 after 05:16Z**.

## 2026-09-23 (later) — RSS budget raised 150 → 180 MiB (stopgap)

Operator decision following the RSS-growth investigation (§7.2–7.3 of
`docs/memory-observability-design.md`). The budget is documented, not enforced
by any alert or script.

**Rationale.** node1's staker had reached 160.4 MiB RSS, above the old 150 MiB
figure, and was still climbing.

**This is a stopgap, not a fix.** An earlier claim that growth had plateaued was
based on a 36-minute lull (14:57→15:33Z) and has been **retracted**. Measured
over 18.35 h post-redeploy the rate is **~450 kB/h, sustained**; at that rate
180 MiB is breached in ~2.2 days, projecting ~222 MiB at sign-off. The
underlying growth remains an open finding — two hypotheses (redb cache, BTC SPV)
were tested and disproven/retracted.

**Peak vs steady state.** The budget is stated against **RSS**. node1's peak
`VmHWM` is 188.9 MiB, above the new 180 MiB figure; it is tracked separately.

## 2026-09-24 — redeploy with malloc mmap/trim tunables (RSS level fix)

Operator-approved rolling redeploy to apply the validated allocator fix
(`MALLOC_MMAP_THRESHOLD_=131072`, `MALLOC_TRIM_THRESHOLD_=131072`). Same image
`vtorrent/node:4d1ae47`; **env-only change**, no new binary. Followers first,
node1 last with `VTORRENT_WALLET_PASSPHRASE_FILE` set explicitly. Backups to
`.ops-backups/malloc-tune-20260924-DN0zyQ/` (0700).

- node3 stop/start, replayed to tip, 0 errors. node2 same. node1 replayed,
  wallet auto-unlocked at 06:58:51Z, staking resumed.
- All three agree at height 16230; `syncing=false`; 0 ERROR/panic on any node.

**Result — level down, rate unchanged.** Over a 70-minute steady-state window
(07:30–08:40Z):

| | Pre-fix | Post-fix |
|---|---|---|
| node1 RSS level | ~163 MiB | **~133 MiB** |
| Steady-state rate | ~450 kB/h | **~477 kB/h** |
| Blocks produced | — | 59/h (~61 s) |

The tunables returned the 30 MiB startup-replay transient (the level win the
head-to-head probe predicted), **but did not change the steady-state rate.**
At 477 kB/h / 59 blocks = **~8 kB per block — the growth is chain-proportional**,
i.e. the in-memory `Chain` retaining every block and index, plus allocator
overhead on that churn. Not the staking transient alone.

**Consequence.** At 477 kB/h from ~133 MiB, the 180 MiB budget is breached in
~4.5 days; sign-off is ~6 days out, projecting ~197 MiB. The fix helps but does
not carry the window on its own.

**Next (real fix, pre-mainnet):** bound the in-memory chain — keep the full
index but prune old block bodies, or move the block/tx index to the store.
That is the only fix for unbounded chain-proportional growth. See
`docs/memory-observability-design.md` §7.6–7.7.

**Soak impact.** Fifth interruption. Window resets to the first post-redeploy
stake at `2026-09-24T06:58:52Z`. Earliest sign-off is now **2026-10-01 after
06:58Z**.

## 2026-09-24 (later) — RSS budget raised 180 → 220 MiB (stopgap)

Operator decision after the post-deploy measurement: **keep the env fix, raise
the budget so the current window can be observed, and fix the real cause
(chain-proportional growth) in the post-soak batch.**

**Rationale.** Post-deploy the level is ~133 MiB but the rate is unchanged at
~477 kB/h (§7.7 of the memory design), which is chain-proportional — the
in-memory `Chain` retains every block body, ~8 kB/block, and is unbounded.
Over the 7-day window that projects 133 + (477 × 168 / 1024) ≈ **211 MiB**. A
220 MiB budget covers that plus the ~189 MiB restart transient (`VmHWM`).

**This is a stopgap, not a fix.** The budget is documented, not enforced (no
Prometheus alert), so the raise changes the target we grade against, not
runtime behaviour. The unbounded growth remains a **mainnet blocker** tracked
in `docs/mainnet-readiness.md`; the real fix is pruning in-memory block bodies
(keep the full index, drop old bodies — reorg only needs `max_reorg_depth`).

Budget history: 150 MiB (original) → 180 MiB (2026-09-23) → **220 MiB
(2026-09-24)**. See `docs/memory-observability-design.md` §7.

## 2026-09-24 (later still) — deploy block-body pruning `1e254b8`

Operator chose to merge and deploy `feat/block-body-pruning` immediately rather
than after sign-off (we were only ~9 h into the current window, so the window
reset is cheap). Merge `1e254b8`; image `vtorrent/node:1e254b8` (binary
`c5953eb7cb5fae1598031c71c1b40baa1c291beff5bc69630bfd155bebd0d0be`),
`--block-body-cache 4096`. Rolling recreate; node1 unlocked 16:31:14Z, staking
resumed. Backups `.ops-backups/pruning-20260924-NSak1w/`.

**Functional result — good.** All three agree at 16788, `syncing=false`,
0 ERROR/panic. Store fallback works: `/api/v1/blockchain/block/height/5000`
(pruned from memory) serves from disk. Bodies are capped: heights below
tip−4096 are gone from memory, headers and indexes retained.

**Memory result — level improves, rate does NOT.** RSS dropped ~13 MiB
(136 → ~123 MiB, consistent with ~12.7k × ~1 kB bodies no longer resident),
but the growth rate did not fall and looks *worse* for the staker:

| Window | node1 (staker) | node2 (non-staker) |
|---|---|---|
| pre-pruning (4d1ae47, post-malloc) | ~477 kB/h | — |
| 16:32→18:33 (this build, 2 h) | **~1455 kB/h** (declining: 1835 → 1264) | ~600 kB/h (last 30 min) |

So capping bodies did **not** address the driver of the rate. The body data
is only ~1 kB/block ≈ 60 kB/h; the observed 500–1500 kB/h is allocator churn
(the staking full-UTXO merkle tree) plus, likely, added free-churn from
evicting a body per block. Pruning bounds body *residency* (important for a
long-running node — previously ~0.5 GB/yr of bodies) but the RSS *rate*
remains dominated by the staking tree churn, still an open item (a separate
"cache the UTXO merkle tree between blocks" change).

**Budget risk.** At ~1264 kB/h from ~126 MiB, sign-off in ~6.5 days projects
~320 MiB — over the 220 MiB budget. node2 at ~600 kB/h projects ~223 MiB
(borderline). This deploy therefore **worsens the soak-budget outlook** even
though it improves the long-run asymptote. Decision pending: keep (bounded
bodies, accept rate) or roll back to `4d1ae47` (known ~477 kB/h). Window resets
to 2026-09-24T16:31:14Z → sign-off 2026-10-01 after 16:31Z.

## 2026-09-25 — dhat profile of the staking path (post-pruning)

Isolated staking probe on a copy of node1 data (network-none, 35 min, 35 blocks
staked) with the `heap-profile` image. Ranked dhat allocation points by peak
live and total churn:

| Allocation point | peak live | total for 35 builds |
|---|---|---|
| `ProofMerkleTree::build` (pre-apply tree) | 4.19 MiB | ~294 MiB |
| `compute_post_apply_root` (2nd full tree) | ~2.5 MiB ×3 | ~258 MiB |
| redb page reads (bounded 64 MiB cache) | 47–59 MiB | store cache |
| `apply_block_journaled` BTreeMap splits | 10.9 MiB | genesis/UTXO load |

Findings:
- **The staking path is the largest *repeated* allocator**: ~11 MB churn per
  staked block × ~60/h ≈ **660 MB/h churn**, even though the peak is only
  ~4 MiB. The big Vecs are >128 KiB so mmap-backed and returned; the retained
  ~0.5–1.5 MB/h is consistent with the many *small* allocations per build
  (`HashSet`/`BTreeMap`/clones in `build_from_kernel_with_proof` +
  `compute_post_apply_root`) fragmenting the heap.
- `build_from_kernel_with_proof` builds the UTXO tree **twice per block**: the
  inclusion tree over `ordered_utxos`, then `compute_post_apply_root` re-walks
  the entire UTXO set to derive the post-apply root.
- redb's largest peaks are the store cache + open-time checksum verification
  (bounded), not a leak.

Target for the rate fix: cut the per-build allocation churn — reuse scratch
buffers (leaves/tree/`seen`/`removed`/`added`) across attempts and derive the
post-apply root from the pre-apply leaves instead of a second full traversal.

## 2026-09-25 — deploy UTXO-commitment scratch `1d67618`

Merged `perf/utxo-scratch` and deployed (image `vtorrent/node:1d67618`, binary
`2b262a2c3dab0076f9da3104dd4057290370321ee49c86ffd343a229beccb9a0`). Rolling
recreate; node1 unlocked 05:47:35Z, staking resumed; all three agree, 0 errors.
Backups `.ops-backups/utxo-scratch-20260925-RRxMcQ/`.

**Controlled probe — the fix works.** Isolated dhat probe, identical to the
earlier ones (35 min, 35 blocks staked):

| build | staking-path alloc | dhat RSS growth /35 min |
|---|---|---|
| `1e254b8` (baseline) | 637.6 MB | +40.4 MB |
| `perf/staking-churn` | 430.3 MB | +32.9 MB |
| **`1d67618`** | **30.7 MB (−95%)** | **+6.1 MB (−85%)** |

The per-block UTXO commitment now allocates nothing once warm; remaining top
allocations are redb's bounded cache and the one-time Argon2 unlock.

**Fleet — much smaller improvement than the probe.** VmRSS over 2.7 h
(05:48→08:28):

| node | start | end | rate |
|---|---|---|---|
| node1 (staker) | 142.5 MiB | 145.5 MiB | ~1133 kB/h |
| node2 (non-staker) | 133.2 MiB | 135.8 MiB | ~941 kB/h |

Both nodes now grow at a similar ~900–1100 kB/h — i.e. the staking-specific
delta (previously ~670 kB/h between them) is gone, consistent with the probe.
But the **shared** ~900–1000 kB/h remains, so the growth is dominated by a
non-staking component the isolated probe excludes: the redb store cache filling
toward its 64 MiB bound, BTC SPV (node1), or P2P/mempool. Not yet attributed.

**Budget.** At ~1000 kB/h from ~145 MiB, 7 days projects ~310 MiB — still over
220 MiB. The scratch did not, on its own, make the window pass; the shared
component must be attributed (a fleet-build dhat probe, or longer observation to
see whether the redb cache plateaus). Window resets to 2026-09-25T05:47:35Z →
sign-off 2026-10-02 after 05:47Z.

## 2026-09-25 (later) — 5 h RSS trend after `1d67618`; node1 confounded

Detached 5-minute sampler (`/tmp/opencode/rss-watch.tsv`), 09:12→14:01Z:

| node | whole window | last hour | role |
|---|---|---|---|
| node1 | 614 kB/h | **1226 kB/h** | staker + **BTC SPV** + 2 peers |
| node2 | 475 kB/h | 550 kB/h | apply-only, 1 peer |
| node3 | 229 kB/h | **192 kB/h** | apply-only, 1 peer |

Two conclusions:

1. **The scratch removed the staking/commitment churn.** node1 plateaued for
   ~85 min mid-window, and node3 (apply-only) settled at ~192 kB/h — over 7 days
   from ~134 MiB that projects ~161 MiB, **under the 220 MiB budget**.
2. **node1's residual is confounded.** It is the only node with BTC SPV
   (`--btc-regtest/--btc-peer/--btc-seed`; node2/3 have none) *and* the only
   staker *and* has an extra peer. Its ~1.2 MB/h cannot be attributed to the
   UTXO commitment (proven eliminated) — most likely the BTC SPV path and/or the
   block-production path, not the chain commitment.

**So the window's outcome hinges on node1 only.** node2/3 are on track. Next:
attribute node1's residual (BTC SPV vs production) — the cheapest test is to
compare node1 with the BTC args removed, or a fleet-like dhat probe with SPV.
Sampler continues to 09:12Z tomorrow for the full 24 h.

## 2026-09-25 (later) — node1 BTC-SPV attribution test

Per the plan, node1 was restarted **without** `--btc-regtest/--btc-peer/--btc-seed`
(node2/3 never had them); temp compose change, backups retained.

**Result: BTC SPV was holding ~40 MiB and dominated node1's level.**

| node | before | after node1 BTC removed |
|---|---|---|
| node1 level | ~145–148 MiB | **dropped to ~97–105 MiB** |
| node2 (no BTC) | 138.5 MiB | 140.5 MiB, ~652 kB/h |
| node3 (no BTC) | 135.5 MiB | 136.2 MiB, ~245 kB/h |

Two structural facts fall out:

1. **node1's high level was largely BTC SPV, not the chain.** Removing it freed
   ~40–47 MiB (the RSS fell from 144 → 97 MiB at the 15:31 trim). The chain-only
   footprint of the staker is ~100 MiB — *lower* than node2/3.
2. **RSS is sawtoothed, not monotonic.** A single `malloc_trim` released 47 MiB
   at 15:36, then it re-grew ~4 MiB/h toward the previous working set. Any rate
   computed over a window that straddles a trim is misleading — several of the
   earlier "rate" numbers are contaminated by this.

**Core chain is slow-growing:** the cleanest node (node3, apply-only, no BTC)
sits at ~245 kB/h → 7 days from 136 MiB ≈ **171 MiB, under the 220 MiB budget.**
node2's ~652 kB/h is the remaining outlier and needs the full 24 h sample.
node1 (staker, no BTC) re-warms to ~4 MiB/h after a trim but has not been
observed through a full trim cycle.

**Decision needed:** keep node1 BTC-less for a cleaner core-chain soak, or
restore the BTC args (swap-testing role). Sampler runs to 09:12Z tomorrow.

## 2026-09-26 — fleet-like follower dhat probe

Follower probe on a copy of node1 data (connected to node2, **no wallet → no
staking**, so no fork risk), 35 min under `heap-profile`:

- RSS +1012 kB/35 min (dhat-inflated) vs the isolated **staker** probe's
  +6124 kB/35 min — so P2P/apply adds far less than block production.
- Top allocation points are **redb reads**: `verify_checksum_helper` (76 MB),
  `visit_pages_helper` (66 MB), `get_page_extended` (51 MB) — i.e. **store
  open-time checksum verification and startup replay**, a one-time cost, not a
  steady-state leak.
- `recompute_utxo_root` shows only **121 allocations over 35 min** — the scratch
  is working (no per-block allocation).

**Why attribution keeps stalling:** short probes are dominated by startup
(replay + checksum verify), and RSS is sawtoothed by allocator trims, so a 35-min
window cannot isolate the steady-state residual. Doing so needs a mid-run heap
snapshot (dhat `SIGUSR1` dump) over a long window — the daemon does not yet
implement SIGUSR1, so that would need a small, soak-safe code change
(`dhat::Profiler` + a signal handler under the `heap-profile` feature).

**Where this leaves the window:** the UTXO-commitment churn is fixed and
verified; BTC SPV (~40 MiB) is out; the cleanest node projects under budget. The
remaining ~300–1700 kB/h across nodes is unattributed but bounded-ish and
dominated by startup/store-cache effects rather than an obvious live leak.
