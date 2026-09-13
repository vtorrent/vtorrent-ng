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
