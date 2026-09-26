# Bitcoin SPV Soak Plan

Status: PLAN (post-soak; not part of the current core-node window)
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
  entry), grown by the sync loop (`main.rs:824+`).
- **Wallet UTXO state** — persisted to `<data-dir>/btc_utxos.json`
  (`BtcWallet::with_persistence`, `main.rs:345`).
- **HTLC / swap monitoring** — `htlc.rs`, exercised only when a swap runs.

The header map is the memory risk: it grows with BTC height and is **not**
observed to be bounded or persisted. On mainnet (~870k headers × ~120 B+overhead)
that is a ≥100 MiB standing cost before any swap state.

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
- Decide whether the header store must be **persisted/bounded** (e.g. prefix
  commitments + on-disk headers) before mainnet — likely yes if Phase B shows
  linear growth.

## 5. Acceptance criteria

- RSS stable under the BTC-SPV budget across the window (headers at a steady
  rate), no ERROR/panic in the SPV sync, swap completes correctly.
- If header memory grows linearly and is not bounded/persisted, that becomes a
  **mainnet blocker** (a new item in `docs/mainnet-readiness.md`), not a soak
  failure.

## 6. Risks / open questions

- **Unbounded header chain** — the biggest risk; quantify in Phase B and decide
  persist/bound.
- **Regtest block generation** must be controlled (don't spam); use
  `generatetoaddress` with a fixed count and pace.
- **Swap driving** needs the BTC-funding wallet and VTR liquidity; reuse the
  existing regtest swap tooling.
- **Config coupling** — the passphrase env var / bind-mount must be supplied to
  `docker compose up` for node1 (learned 2026-09-25).

## 7. Non-goals

- Not part of the current core-node window; do not restart node1 until the
  core-node sign-off decision is made.
- Not changing swap protocol or HTLC semantics.

## 8. References

- `vtorrent-daemon/src/config.rs:128,135,139` `--btc-seed/--btc-regtest/--btc-peer`.
- `vtorrent-daemon/src/main.rs:331-370` SPV init; `:824+` sync loop.
- `vtorrent-btc/src/headers.rs` header store; `htlc.rs`; `wallet.rs`
  (`with_persistence` → `btc_utxos.json`).
- `docs/soak-log.md` 2026-09-25 (BTC-SPV attribution); `docker/testnet/docker-compose.yml`
  (node1 args + the "restore before swap testing" comment).
