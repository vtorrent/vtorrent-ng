# vTorrent-NG Improvements — Design Spec

**Date:** 2026-08-26
**Sequence:** B → A → C (Foundation-first, then Polish, then Continuous slices)
**Scope:** "All of the above (phased)" — user-visible polish as done criteria, breaking allowed pre-launch
**Status:** Approved in brainstorming, awaiting implementation plan via `writing-plans`

## 1. Architecture Overview

vTorrent-NG is a Cargo workspace with 17 crates + `vtorrent-ui` (React + TS + Tailwind, Tauri 2). The last 44 commits since `v2.0.0-beta.2` were heavy on critical fixes (mempool eviction, reorg persistence, swap lifecycle, store self-heal). Two *god files* remain at the seams: `vtorrent-node/src/node.rs` (2649L) and `vtorrent-rpc/src/handlers.rs` (2282L), plus `vtorrent-tauri/src/commands.rs` (2034L) duplicating wallet logic. Everything else is well-bounded with existing tests (531 tests, 4 fuzz targets, audit clean).

Goal: cut those seams into single-purpose modules communicating through existing `AppState`/`Chain`/`Mempool` interfaces — no new runtime dependencies, no behavior change beyond noted breaking P2P bump.

## 2. Phase 1 — Foundation (build first)

### A. `node.rs` split
```
vtorrent-node/src/node/
  mod.rs              — Node struct, run loop, select!
  chain.rs            — handle_block, reorganize_to wiring, persistence events
  p2p.rs              — handle_peer_event, request_blocks_from_peers, seed re-dial, our_addr
  staking.rs          — staking loop delegates to vtorrent-node::staking (kept)
  mempool_bridge.rs   — handle_confirmed_block, pending-tx filtering
```
- Lock order `chain → mempool` stays explicit and linted (ABBA fix from 2026-08-26).
- `reorg_persistence.rs` store integration test remains the oracle.

### B. `handlers.rs` split
```
vtorrent-rpc/src/handlers/
  mod.rs, wallet.rs, swap.rs, torrent.rs, staking.rs, prelude.rs
```
- `prelude.rs` re-exports `validate_p2pkh`, `require_swap_stage`, `btc_txid_hex`.
- Re-exports keep `server.rs` route table unchanged.

### C. Shared `wallet-service` crate
New crate `vtorrent-wallet-service` extracting the duplicated payment path:
- `TxBuilder::new().min_absolute_fee(MIN_ABSOLUTE_FEE_SATS)` + `select_coins` fee logic
- `fee_satoshis` calc (selected inputs only)
- Both `vtorrent-daemon::build_incentive_payment` and `vtorrent-tauri::commands::send_vtr` become `wallet_service::build_payment(utxos, recipient, change, fee_rate)`.
- Eliminates the 892 vs 1000 sat divergence fixed twice this session.

### D. Genesis blob
- `vtorrent-node/src/genesis.rs` 59k-line `LEGACY_SNAPSHOT` array → `genesis_snapshot.bin` + `include_bytes!` + `serde` decode at build or `const` include.
- Existing `test_snapshot_sum_matches_documented_supply` stays green (sum/count/uniqueness).
- Cuts ~59k lines parsed every `cargo fmt`/IDE pass.

## 3. Phase 2 — Polish (after foundation, breaking window)

### P2P wire JSON→bincode
- Add `V2` command set behind `PROTOCOL_VERSION` bump (magic `0x56 0x54 0x52 0x4E` unchanged). Old seeds ignore unknown commands (already validated).
- 47 `serde_json` call sites in `node.rs` block/tx inv path become `bincode` (2–5× smaller). Requires coordinated seed restart via `deploy/provision-seed.sh`.
- Fallback: unknown-version peers get JSON path for one release.

### Staking dashboard
- Replace 5s polling in `vtorrent-ui/src/hooks/useStakingStatus` with WS push (`vtorrent-node/src/events.rs` already emits `StakingReward`/`NewBlock`). Frontend subscribes once via `ws.rs` broadcast.

### Torrent UX
- Empty-state illustrations, progress bar determinism, resume-file integrity badge (SHA1 already verified in `PieceAssembler`).

## 4. Phase 3 — Continuous slices & hardening

Thin weekly slices, each shippable:
- Explorer deferral announcement + backup policy (docs)
- Benchmark gate `>25%` in CI (criterion baselines already in `vtorrent-node/benches/consensus_hotpath.rs`)
- `cargo machete` in CI for unused deps
- Remaining `mainnet-readiness.md` items: CI billing, wallet.dat fixture, IONOS rotation

## 5. Data Flow & Error Handling

- **No new data flows** in Phase 1 — splits preserve existing `Arc<Mutex<Chain>>`/`Arc<Mutex<Mempool>>`/`AppState` boundaries.
- **P2P bump:** version negotiation in `MsgVersion`; mismatch → peer kept on JSON path, no disconnect.
- **Store:** `BlockStore::open` genesis backfill + `META` repair (added 2026-08-26) stays the crash-recovery story; no change.
- Errors remain `thiserror` in libs, `anyhow` in bins; new `wallet-service` uses `thiserror`.

## 6. Testing & Rollout

- Phase 1: no behavior change — existing 531 tests + `reorg_persistence.rs`/`store_healing.rs`/`fee_floor` tests stay green; `cargo fmt`/`clippy --workspace --all-targets --all-features -- -D warnings` green.
- Phase 2: new bincode path behind version gate has dedicated unit test (encode → decode round-trip for `Block`/`Transaction`/`InvMsg`).
- Rollout: Phase 1 lands as 45th commit since beta.2, no seed restart needed except genesis blob (build-only). Phase 2 ships with coordinated seed restart (existing provisioning script), soak verifies block 8 inclusion.

## 7. Open Questions Resolved

- Breaking P2P allowed pre-launch? Yes (user).
- Success = user-visible polish? Yes.
- Order = B → A → C? Yes.
