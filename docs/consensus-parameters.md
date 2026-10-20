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
| Difficulty adjustment | Every 2016 blocks | `vtorrent-node/src/consensus.rs:61` |
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
