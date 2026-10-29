# Bitcoin SPV Soak Plan

Status: LAUNCH GATE (swaps enabled at mainnet launch)
Scope: `vtorrent-btc` SPV path as enabled by `--btc-regtest/--btc-peer/--btc-seed-file` or `VTORRENT_BTC_SEED`.

## Current repaired-build windows

The exact remediated-source probe runs
`9a22bcd8ccd9bbdb4caf9bec794640fc3c0111aa` on RPC 22575 and P2P 22576. It
started **2026-10-28T12:12:57Z**; the earliest 24 h signoff is
**2026-10-29T12:12:57Z**. The seed and API key are supplied through 0600 files,
not argv. It synced 973,549 headers by 12:23:15Z and scanned 100,001 BIP-158
blocks by 12:35:50Z. Five peer groups connected; scan-error count is zero.
RSS was 213,897 kB after the scan. An incremental filter download timed out and
the multi-peer failover recovered, scanning two new blocks by 12:41:31Z.
Binary SHA-256:
`ab55060f50c2d6355ecd67c1dd118ebf87a6146ade413df4fdb4db9a537f940d`.
Metadata and one-minute RSS samples are under
`/tmp/opencode/btc-spv-9a22bcd-20261028T121257Z`.

The `5f4c788` probe was stopped at 12:12:57Z after 11h25m, superseded by the
whole-project remediation. It recorded 686 samples with a 219,876 kB peak and
182,808 kB last sample. Its artifacts remain at
`/tmp/opencode/btc-spv-5f4c788-20261028T004706Z`. The `287a505` probe was
retired after its window; artifacts remain under
`/tmp/opencode/btc-spv-287a505-20261027T102313Z`.

`aeb103a` repairs the four findings in
`docs/2026-10-26_security-vtorrent-report.md`. Real Bitcoin Core testing then
exposed an additional restart-lineage case, repaired in `8639a7f`. `c3b3847`
and `c3a79fa` keep restored outputs locked until a complete authenticated
rescan; `5f4c788` returns canonical Bitcoin txids. `8ba6331` remediates the
whole-project review, including secret files, BTC-send reauthorization, torrent
symlink handling, and streamed tracker limits; `9a22bcd` tracks the release
lockfile. The full workspace suite passed 1,121 tests, zero failures, and eight
ignored tests.

A separate local Prometheus at `127.0.0.1:9095` scrapes the exact
remediated-source probe. Production notification delivery and a fleet-level
active soak remain open. The component-level 60-cycle active soak passed.

## 1. Why this is separate

BTC SPV is a **separable ~40 MiB component** that confounded the core-node RSS
measurements and was disabled on node1 for the current window
(`docs/soak-log.md` 2026-09-25). It is required for atomic swaps, so it must be
soaked and given its own memory budget before mainnet — independently of the
core chain, which is now clean (no live leak).

Measured (node1, same data): **~145 MiB with `--btc-*` → ~105 MiB without** ⇒
~40–47 MiB resident, released by an allocator trim.

## 2. What the path holds

- **Header chain** — `vtorrent-btc/src/headers.rs`: an in-memory
  `headers: HashMap<BlockHash, StoredHeader>` (80-byte header + height + work per
  entry), grown by the sync loop (`main.rs:824+`). Bounded by
  `--btc-header-retention` (default 100,000); pruning is amortized with a
  1,024-header slack.
- **Wallet UTXO state** — persisted to `<data-dir>/btc_utxos.json`
  (`BtcWallet::with_persistence`, `main.rs:345`).
- **HTLC / swap monitoring** — `htlc.rs`, exercised only when a swap runs.

The header map was the memory risk: it grows with BTC height. It is now bounded
by `--btc-header-retention` (default 100,000 headers, about 11 MiB) with
amortized pruning. The mainnet-scale test inserts 870,000 headers and keeps the
retained store within 100,000 plus the 1,024-header slack.

## 3. What was actually measured vs assumed

- Measured: ~40 MiB resident with a regtest bitcoind peer.
- **Not** measured: header-chain growth over time, mainnet-scale header count,
  or memory during an active swap. Those are this plan's job.

## 4. Plan

### Phase A — passive soak (restore, observe)
1. Restore the `--btc-regtest/--btc-peer/--btc-seed-file` args on node1 (the compose
   comment marks where; pre-change copy is
   `.ops-backups/` + `/tmp/opencode/compose.pre-nobtc.yml`).
2. Run the 5-minute RSS sampler (`/tmp/opencode/rss-watch*.sh`) for ≥24 h.
3. Record RSS delta vs the BTC-less baseline and the steady-state rate;
   in the interval `SIGUSR1` heap profile, confirm the BTC stacks and whether
   peak live grows (headers) or is transient.

**Probe started 2026-10-22T04:54Z:** a separate probe daemon (data dir
`/tmp/vtr-btc-probe`, RPC `127.0.0.1:22535`, P2P `127.0.0.1:22536`,
`--btc-regtest --btc-peer 127.0.0.1:18448`, retention 100,000) is running
without touching the core soak fleet. Initial sync: 471 headers / 472 blocks,
RSS 105,488 kB. After generating 100 regtest BTC blocks, the next 300 s sync
added 100 headers / 100 blocks; RSS was 106,276 kB (+788 kB). A second
100-block batch at 08:44Z synced at 08:49:47Z and scanned through height 671;
RSS was 106,328 kB. Across the first ~4 h the sampler recorded 106,160–106,328
kB with no ERROR/panic. The sampler `/tmp/vtr-btc-probe/rss-watch.sh` logs RSS
and BTC height every 60 s to `/tmp/vtr-btc-probe/rss.tsv` for 24 h. This is a
probe, not the fleet soak. At 2026-10-22T23:27Z the probe was at 684 headers
with RSS 106,348 kB. The sampler's PID selector was corrected to match the
probe's data dir after it briefly sampled the core-soak node1 process; the
empty RSS samples from 23:24–23:27 are the sampler bug, not the probe.

**24h result (2026-10-23T04:54Z):** 1,000 valid RSS samples over the window;
RSS ranged from 105,836 to 106,348 kB with a mean of 106,241 kB. BTC height
moved from 571 to 684 and stayed there after the active swap probe. No ERROR or
panic appeared in the probe log. The probe and sampler were stopped after the
window. This is a probe result; the fleet BTC SPV soak remains part of the final
release validation.

**Active swap probe (2026-10-22T08:57Z):** the real BTC SPV multi-node HTLC swap
test passed again at `6402d87` in 23.50 s (VTR funding
`0ea663df954c474da00a0a7b1a6e135a70ba79198858b81c88c65a223727af7d`, BTC funding
`36f52d3e601ca64267a199ccb151f3511be45cb0bf9b35de19ccf0a7cf7e892c`, BTC claim
`679b8632ac4d991fb8b216dee04dcf25f58f1fd0ae18ce93ee170d3deb55fadc`, VTR claim
`ac7ccdb43660dc978e40c261abb8750ac7dfe99034fc5eb7d7ddad1c2df137c7`). The probe
followed the test's 13 new BTC blocks at 08:59:47Z and scanned through height
684; RSS stayed 106,328 kB.

**Mainnet probe (2026-10-24):** a separate mainnet probe (data dir
`/tmp/vtr-btc-mainnet`, RPC `127.0.0.1:22545`, P2P `22546`, configured peer
`89.58.60.208:8333`, `--btc-header-retention 100000`) found a mainnet-only
defect: a fresh filter-header chain requires agreement from two
compact-filter peers, but the sync loop connected one, so the scan failed with
`compact-filter headers require agreement from 2 peers (have 1)`. `1901bb9`
fixes this by authenticating filter headers across up to two compact-filter
peers (configured peer plus DNS seeds, 5 s connect timeout) and by splitting
authentication from the filter download so the multi-peer scan does not
re-authenticate with one peer. After the fix, the probe synced 972,892 headers
(retained floor 872,892) and scanned 100,001 BIP-158 blocks in 14m52s with RSS
181,952–182,452 kB and no ERROR/panic. Two peers were connected. The probe
remains running for a 24 h window; this is a probe, not the fleet soak.
Because the scan starts at the retained floor, funds older than 872,892 need
`--btc-birthday`.

**Fresh probe on `513efde` (2026-10-24):** the `1901bb9` probe was stopped
and restarted on `513efde` (binary SHA-256
`1fc553a457006e394ba3b6ae4f90406d6b531eef6c4d643ccd4e5f1d6d3941a8`) at
13:14:05Z. The configured peer connected first, followed by two peers from
distinct network groups. The probe synced 972,921 headers by 13:20:56Z and
scanned 100,001 BIP-158 blocks by 13:25:29Z. RSS rose from 27,876 kB at launch
to a 182,076 kB maximum and held near 182,070 kB for the next 11 hours. Three
peer-agreement failures at 13:46Z, 14:30Z, and 23:34Z each recovered on the
next five-minute cycle; there were no panics. In each case two of the three
peers dropped during filter-header authentication, the configured peer
survived, and the next cycle found replacements. The resulting five-minute
delay affects wallet display, not consensus or the swap paths, which use
separate peer checks and retries. A five-peer target or in-cycle replenishment
is optional hardening and would require a new soak; both are implemented in `c465d1b`. The full workspace suite at
`513efde` passed 48 test groups, 1,105 tests, 0 failures, and 7 ignored tests.
The probe was stopped at 2026-10-25T01:29:27Z after 12h15m, before the 24 h
window completed, to deploy the hardened build. Its artifacts remain at
`/tmp/opencode/btc-spv-513efde-20261024T131405Z`; prior `1901bb9` artifacts
remain in `/tmp/vtr-btc-mainnet`.

**Hardened probe on `c465d1b` (2026-10-25):** binary SHA-256
`814fa22c720c90da5d129704db2fafd1c9576860e9a2f898688d91d46b48cf71`, started
01:29:27Z. The build keeps five mainnet peers across distinct network groups,
still connects the configured peer first, and retries a failed scan with
freshly connected peers before waiting for the next cycle. It synced 972,999
headers by 01:34:40Z and scanned 100,001 BIP-158 blocks by 01:40:24Z. RSS was
236,424 kB after the scan and held at 236,456 kB through 42 minutes, about
54 MB above the three-peer build but within the 256 MiB budget. The full
workspace suite at `c465d1b` passed 48 test groups, 1,105 tests, 0 failures,
and 7 ignored tests. The probe was stopped at 2026-10-25T15:20:41Z after
13h51m, before the 24 h window completed, to deploy the metrics build. Its
artifacts remain at `/tmp/opencode/btc-spv-c465d1b-20261025T012927Z`.

**Metrics build `6f617ce` (2026-10-25):** ran for 18 minutes. It exposed the
new gauges, but `/metrics` blocked while the scan held the wallet write lock;
the build was replaced by `7ec3b2f`.

**Metrics build `7ec3b2f` (2026-10-25):** binary SHA-256
`e10d1abf721a8f31cf75cf06afeb8a4b5b83f3bae641a9bb9654c292f7007e0a`, started
15:20:41Z. It keeps the five-peer target and adds atomic BTC metrics, so the
endpoint no longer waits for the wallet lock. It synced 973,089 headers by
15:26:37Z and scanned 100,001 BIP-158 blocks by 15:32:51Z. During the scan,
`/metrics` returned `vtorrent_btc_scan_in_progress 1`, `vtorrent_btc_peer_groups
5`, and `vtorrent_btc_scan_lag_blocks 973089`; after the scan it returned
`last_scanned_height 973090`, `scan_lag_blocks 0`, `scan_duration_milliseconds
374114`, and zero auth or scan errors. RSS was 220,816 kB after the scan. The
24 h window ends at **2026-10-26T15:20:41Z**.

**Peer cache build `2c381fa` (2026-10-25):** persists successfully connected
compact-filter peers in `btc_peers-<network>.json`, keeps up to 50, and tries
them before DNS seeds. In an isolated two-restart test, the first run wrote
five peers; the restart logged `Loaded 5 cached BTC peer(s)` and reconnected
all five in under four seconds. The main 24 h soak remains on `7ec3b2f`; the
cache build is committed at `2c381fa` and is part of the launch build.

**Final-source probe on `2c381fa` (2026-10-26):** started at 00:54:40Z on RPC
22555 and P2P 22556, while the `7ec3b2f` probe continued on 22545/22546. The
first cycle wrote `btc_peers-mainnet.json` with five compact-filter peers,
synced 973,154 headers by 01:01:01Z, and scanned 100,001 BIP-158 blocks by
01:08:15Z. During the scan `/metrics` returned `scan_in_progress 1`,
`peer_groups 5`, and `scan_lag_blocks 973154`; after it returned
`last_scanned_height 973155`, `scan_lag_blocks 0`,
`scan_duration_milliseconds 434237`, and zero auth or scan errors. RSS was
234,216 kB. The 24 h window ends at **2026-10-27T00:54:40Z**.

### Phase B — active soak (drive the header chain and a swap)
1. Generate regtest BTC blocks against the local bitcoind
   (`vtr-btc-regtest`) at a controlled rate; watch header count vs RSS. This is
   the mainnet-header-growth analogue at small scale.
2. Run one full HTLC swap (fund → claim, and a refund path) and watch the
   monitor's memory and correctness.
3. Measure `btc_utxos.json` size and reload behaviour.

### Phase C — budget and verdict
- Set a **BTC-SPV budget** (proposed: core budget + 64 MiB) and grade node1
  against it.
- **Multi-node SPV agreement (2026-10-19):** two independent daemons synced
  the same real regtest header tip, discovered the same funded outputs,
  persisted them across restarts, and followed a reorg that re-included the
  funding transactions. See `docs/btc-spv-multinode-reorg-test.md`. A full
  HTLC swap now settles against real BTC SPV, including the maker BTC claim
  and taker VTR claim, and a conflicting-spend test proves a reorged-out
  funding output disappears after restart. `taker_claims_vtr_on_a_separate_node`
  now broadcasts and confirms the taker claim on a second VTR daemon, and the
  maker recognizes the external claim. A 60-cycle one-hour active soak passed
  with no failures; a fleet-level soak after rebuild remains open.
- **Header store bounded (2026-10-08):** `HeaderChain` now has an opt-in
  retention window (`--btc-header-retention`, default 100_000 ≈ 11 MiB vs
  ~100 MiB unbounded on mainnet), pruning strictly below `best_height -
  retention` while keeping the retained set contiguous (difficulty validation
  and the UTXO scan both need that). Genesis is always kept. This bounds the
  mainnet-scale risk the regtest soak could not exercise.
- **Mainnet-scale retention test (2026-10-19):** an ignored test inserts
  870,000 headers and keeps the retained store within 100,000 plus the
  1,024-header slack. Pruning is amortized so the sync is not O(headers ×
  retention). The test passed in release mode in 17.39 s; re-run at `6402d87`
  on 2026-10-22 passed in 19.49 s.

## 4.1 Wallet birthday

A fresh BTC wallet starts scanning at the retained header floor. Set
`--btc-birthday <height>` to the wallet's creation height to scan older funds.
The scan clamps to the retained floor and warns when the birthday is below it.
Without a birthday, funds older than retained history are not discovered.

## 5. Acceptance criteria

- RSS stable under the BTC-SPV budget across the window (headers at a steady
  rate), no ERROR/panic in the SPV sync, swap completes correctly.
- The retained header store stays within `--btc-header-retention` plus the
  slack at mainnet-scale counts. The 870,000-header test covers the bound; the
  active swap soak covers the swap path.
- If header memory grows beyond the bound, that is a **mainnet blocker**, not a
  soak failure.

## 6. Risks / open questions

- **Header retention** — bounded and amortized; the mainnet-scale test covers
  the count. The remaining risk is the active swap soak and separate VTR nodes.
- **Regtest block generation** must be controlled (don't spam); use
  `generatetoaddress` with a fixed count and pace.
- **Swap driving** needs the BTC-funding wallet and VTR liquidity; reuse the
  existing regtest swap tooling.
- **Config coupling** — the passphrase env var / bind-mount must be supplied to
  `docker compose up` for node1 (learned 2026-09-25).

## 7. Non-goals

- The core-node 7-day soak is separate. Run the BTC SPV soak on a probe or after
  the core window; do not restart node1 during the core soak.
- Not changing swap protocol or HTLC semantics.

## 8. References

- `vtorrent-daemon/src/config.rs:128,135,139` `--btc-seed-file/--btc-regtest/--btc-peer`.
- `vtorrent-daemon/src/main.rs:331-370` SPV init; `:824+` sync loop.
- `vtorrent-btc/src/headers.rs` header store; `htlc.rs`; `wallet.rs`
  (`with_persistence` → `btc_utxos.json`).
- `docs/soak-log.md` 2026-09-25 (BTC-SPV attribution); `docker/testnet/docker-compose.yml`
  (node1 args + the "restore before swap testing" comment).

## Release fleet with BTC SPV on every node

A separate three-node candidate fleet started 2026-10-29T02:22:09Z with BTC
mainnet SPV enabled on all nodes. All three reached BTC header 973,632 and
scanned through 973,633 with lag 0. Each held about 237 MB RSS, above the
220 MiB `ProcessRssHigh` threshold. A four-peer fleet (`dd98176`) reached 267/246/241 MB during concurrent
initial scans; the fleet was stopped before post-scan steady state. A controlled
single four-peer node settled at 182 MB during and 190 MB after the scan, so the
fleet peak was scan-related. `ProcessRssHigh` now suppresses during an active
scan, while `BtcSpvScanStuck` covers a stuck scan. A three-node post-scan
measurement remains. Evidence: `/tmp/opencode/release-fleet-20261029T041537Z/`
and `/tmp/opencode/profile-btc-peer4-*`.
