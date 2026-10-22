# Bitcoin SPV Soak Plan

Status: LAUNCH GATE (swaps enabled at mainnet launch)
Scope: `vtorrent-btc` SPV path as enabled by `--btc-regtest/--btc-peer/--btc-seed`.

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
1. Restore the `--btc-regtest/--btc-peer/--btc-seed` args on node1 (the compose
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
probe, not the fleet soak.

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
  funding output disappears after restart. Separate maker and taker VTR nodes
  remain open.
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

- `vtorrent-daemon/src/config.rs:128,135,139` `--btc-seed/--btc-regtest/--btc-peer`.
- `vtorrent-daemon/src/main.rs:331-370` SPV init; `:824+` sync loop.
- `vtorrent-btc/src/headers.rs` header store; `htlc.rs`; `wallet.rs`
  (`with_persistence` → `btc_utxos.json`).
- `docs/soak-log.md` 2026-09-25 (BTC-SPV attribution); `docker/testnet/docker-compose.yml`
  (node1 args + the "restore before swap testing" comment).
