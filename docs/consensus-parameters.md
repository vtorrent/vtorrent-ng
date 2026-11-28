# Consensus Parameter Sign-Off

**Date:** 2026-08-26
**Status:** Frozen for mainnet. These values are consensus-critical after launch.

## Network Identity

| Parameter | Value | Source |
|---|---|---|
| Network magic | `0x56 0x54 0x52 0x58` (`"VTRX"`) | `vtorrent-core/src/network.rs:43` |
| P2P port | `22526` | `vtorrent-core/src/network.rs:46` |
| RPC port | `22527` (daemon default `22525`) | `vtorrent-core/src/network.rs:49` |
| Protocol version | `3` (bincode wire; hard boundary for UTXO-committed headers) | `vtorrent-p2p/src/message.rs:102` |

## Address Format

| Parameter | Value | Source |
|---|---|---|
| Address prefix | `70` (Base58Check → `V...`) | `vtorrent-core/src/network.rs:37` |
| WIF prefix | `198` (→ `W...`) | `vtorrent-core/src/network.rs:40` |
| Checksum | Double-SHA256 first 4 bytes | `vtorrent-core/src/address.rs` |

## Consensus Rules

| Parameter | Value | Source |
|---|---|---|
| Target block time | 60 seconds | `vtorrent-node/src/consensus.rs:53` |
| Annual reward rate | 5% (5/100) | `vtorrent-node/src/consensus.rs:28` |
| Min stake amount | 1 VTR (100,000,000 sats) | `vtorrent-node/src/consensus.rs:43` |
| Min stake age | 6 hours (21,600 s) | `vtorrent-node/src/consensus.rs:31` |
| Max stake age | 6 days (518,400 s) | `vtorrent-node/src/consensus.rs:34` |
| Max supply | 20,000,000 VTR (2×10¹⁵ sats) | `vtorrent-node/src/consensus.rs:46` |
| Max block sigops | 20,000 (`MAX_BLOCK_SIZE / 50`, legacy rule) | `vtorrent-node/src/consensus.rs:22` |
| Max future block drift | 5 seconds (`MAX_FUTURE_BLOCK_DRIFT`), relative to `max(wall clock, parent timestamp)` | `vtorrent-node/src/consensus.rs` |
| Minimum block spacing | 55 seconds (60 − 5s drift); enforced for production blocks, waived for test/regtest fixtures | `vtorrent-node/src/consensus.rs` |
| Difficulty | Fixed: each block's `bits` must equal its parent's | `vtorrent-node/src/consensus.rs` |
| Genesis bits | `0x1e0fffff` | `vtorrent-node/src/genesis.rs:27` |
| Genesis timestamp | `1700000000` | `vtorrent-node/src/genesis.rs:24` |

## Genesis Block

| Property | Value |
|---|---|
| Hash | `36ca792aa45ca7850f2789ff2e62ec13e91bd5f2770d6fea8df81bc2da1da8f8` |
| UTXO root | `65185f8a5c055c17bf7053c6b6c42993565bb5586689a8508017005b842f9105` |
| Snapshot addresses | 59,375 |
| Snapshot total supply | 11,589,746.63 VTR (1,158,974,663,136,877 sats) |
| Snapshot format | Binary blob (`genesis_snapshot.bin`, 2,493,754 bytes) |

## Checkpoints

`--checkpoint HEIGHT:HASH` pins one main-chain block. The daemon rejects any
block at that height with a different hash and refuses a reorg whose fork point
is below it. It is not a consensus rule and is optional on testnets; mainnet
nodes should use the checkpoint published after launch
(`docs/checkpoints.md`). Without it, a new node can be fed a long-range forged
chain from genesis, which minimum block spacing limits in rate but cannot
prevent.

## Minimal CLTV scripts

New P2CS and VTR HTLC scripts encode locktimes with `OP_0`/`OP_n`/minimal
pushes. The parser accepts the legacy fixed `PUSH4` shape, but pre-change nodes
do not recognize the minimal shape. Mainnet starts on the new code; for an
existing testnet/soak chain, upgrade every node before creating or spending a
new cold-stake or swap script. A mixed fleet can classify the same output
differently.

## Fork choice

The main chain is the branch with the greatest cumulative work (one unit per
PoS block). If a fork has **equal** work at the same height, the branch whose
tip block hash is lexicographically lower wins; a node reorgs to it. Without
this tie-break, two stakers producing at the same height can build on their own
tips indefinitely, because neither branch has strictly greater work. Nodes
request headers from equal-height peers and relay validated fork blocks, so the
tie-break can run after a partition, not only for blocks a node happens to
receive directly.

## Verification

- Genesis hash independently verified via RPC: `GET /api/v1/blockchain/block/height/0`
- Standalone Python verifier (`scripts/verify-genesis.py`) parses the snapshot
  without the Rust decoder, validates Base58Check prefix 70, rebuilds both
  genesis transactions, and recomputes the txids, merkle root, UTXO root, and
  block hash. It matches the Rust values and runs in CI.
- Snapshot sum/count/uniqueness verified by `test_snapshot_sum_matches_documented_supply`
- Snapshot binary roundtrip verified by `test_snapshot_binary_roundtrip`
- Address/WIF prefix verified end-to-end via wallet import + send on soak
- Release build with `overflow-checks = true` verified clean

## Launch-time floor

Production blocks must be dated at or after `LAUNCH_TIME - MAX_PAST_DRIFT`:

- `LAUNCH_TIME = 1_796_428_800` (2026-12-05T00:00:00Z)
- `MAX_PAST_DRIFT = 7_200` seconds

`LAUNCH_TIME` must never be later than the actual launch. Rebuilding with a later value after blocks exist is a consensus change. Test and regtest fixtures bypass the floor through `allow_pow`. This rule exists because genesis is years older than launch; without it, the bootstrap claimer can walk chain time forward in six-day steps and mint the early reward stream in seconds. See `docs/2026-11-26_ai-security-review-4ce7d31.md`.
