# Code review fix status — 2026-09-16

Per-finding ledger for the two 2026-09-15 reviews
(`docs/code-review-2026-09-15.md`, `docs/code-review-2026-09-15-second-pass.md`).

All fixes were made on `main`, verified with `cargo test --workspace`
(47 test binaries, 0 failures), `cargo clippy --workspace --all-targets
--all-features` (clean) and `cargo fmt --all -- --check`. No fleet action was
taken; the soak was not interrupted.

## Fixed

| Finding | Commit | Notes |
|---|---|---|
| H1 RPC panic on non-ASCII hex | `dfce409` | Added `truncate_chars`; applied at all 10 byte-slice sites |
| H4 DHT bencode length overflow | `dfce409` | Bounded numeric prefix + checked/saturating math |
| S7 snapshot `decompress_amount` overflow | `dfce409` | Returns `Option`, `checked_mul`; regression test |
| S8 torrent `piece_length` overflow | `dfce409` | Checked math + index bounds; `FileLayout` saturating |
| L14 migrate `hex_decode` panic | `dfce409` | Iterates bytes, no str-slicing |
| S3 CLI treats 403 as success | `dfce409` | Any non-2xx errors; reads body `message` |
| H2 wallet/staking/DEX reads unprotected | `ecd4c17` | Moved 7 routes into `protected`; test extended |
| H3 rate limiter before auth | `ecd4c17` | `auth` is now the outer layer |
| H5 mempool bounded by count not bytes | `7b805da` | 32 MB byte budget; oversized tx rejected |
| H6 child-before-parent block | `7b805da` | Kahn topological sort in `get_transactions` |
| H7 duplicate inputs | `7b805da` | Rejected in `validate_transaction` |
| L1 nondeterministic eviction tie-break | `7b805da` | txid added as final key |
| L2 fee-trusting `add_transaction` | `7b805da` | Now `#[cfg(test)]` |
| H8 migrate `--json` dumps WIFs | `064b01c` | Manual `Serialize` redacts unless `VTORRENT_SHOW_WIF` |
| H9 unzeroized decryption intermediates | `064b01c` | `Zeroizing` throughout; `decrypt_private_key` returns `Zeroizing` |
| H10 debug print of AES key+IV | `064b01c` | Deleted |
| M13 unzeroized signing-path copies | `064b01c` | `from_wif`, `wif_keys`, `key_pairs` |
| L15 unzeroized HD seed | `064b01c` | `to_seed` returns `Zeroizing<[u8;64]>` |
| S2 torrent metadata pin (~64 GB) | `65b76bd` | Running byte total + index/size validation |
| S6 SPV PoW bypass | `65b76bd` | Overflowing targets rejected; regression test |
| S9 DHT pending frontier growth | `65b76bd` | Capped at 1024 |
| S10 staking reward overstatement | `65b76bd` | `Chain::coinstake_reward` = outputs − principal |
| M1/M4 DEX gossip validation + `seen_orders` | `65b76bd` | Field validation + cap |
| M3 overlay punch map unbounded | `65b76bd` | Hard cap with oldest-eviction |
| M4 ban map unbounded | `65b76bd` | Capped like `scores` |
| S1 legacy-claim signature not bound to outputs | `9fe7429` | v2 hash commits to outputs; replay test |
| M14 wallet temp-file race / no fsync | `b5166c7` | `O_EXCL` unique name + fsync file and dir |
| S13 BTC claim dust | `b5166c7` | Require `target_amount > fee + 546` |
| L16 foreign-network address accepted | `b5166c7` | `validate_p2pkh` in `address_to_hash160` |
| S19 Tauri staking false success | `b5166c7` | Propagates send error |

## Deliberately not fixed

### C1 — stake-kernel target saturates at `u32::MAX`

`vtorrent-node/src/consensus.rs:132-133`. Any UTXO ≥ 42,949.67 VTR passes the
kernel unconditionally. Independently verified: 72 legacy addresses hold 85.4%
of the legacy supply and could each mint every block.

**Why it is not fixed here.** The same function runs in the block producer
(`staking.rs:382`), the validator (`chain_reorg.rs:343`) and the store's
startup replay (`store.rs:641` → `chain.add_block`), and there is no
activation-height / fork mechanism in the codebase. Changing the rule is
therefore a hard fork.

**Why a mechanical fix does not work.** The per-tick probability is
`P = min(value/1000, 2^W−1) / 2^W`. Widening the integer does not move the
saturation point unless the target is also scaled by `2^(W−32)`, which puts it
back at 42,949.67 VTR. Raising the scale constant to avoid saturation within
`MAX_SUPPLY` (needs `S > 465,661`) drops the current 503 VTR staker from one
block per ~85 ticks to one per ~39,690 ticks. Capping the target at
`u32::MAX/2` still lets a whale produce ~1 block/min while leaving the staker
untouched. The linear model cannot represent stake weights far above the
minimum without either saturating or starving small stakes.

**Recommended direction.** Normalize by total staked supply
(`P = value / total_staked` per tick), which yields `P = 1.0` for the current
sole staker (preserving ~60s blocks) and a proportional share once multiple
stakers exist. This needs: a tracked `total_staked` on `Chain`, a decision on
the tick model (per-second retry vs per-slot), a coordinated fleet upgrade
with a fresh chain (the current soak chain would fail replay under the new
rule), and a test that a whale cannot exceed its proportional share.

**Impact if left.** No effect on the current fleet: the sole staker holds
503.9 VTR, far below the 42,949.67 VTR threshold, and the genesis distribution
outputs are OP_RETURN (unspendable) until claimed. The exposure begins when a
holder above the threshold claims and stakes. This is a mainnet-launch
blocker, not a soak blocker.

## Not addressed (lower priority, no fix attempted)

M2 (`getdata` bandwidth accounting), M5 (PEX per-peer quota), M6 (DHT source
validation — the torrent DHT already validates source and tid), M7 (overlay
relay auth), M8 (tracker SSRF), M9 (torrent session cap), M10 (wallet import
overwrite), M11 (full-chain scans under the chain mutex), M12 (`btc_fund`
authorization), M15 (TOTP replay), M16 (`fork()` safety), M17 (quadratic
eviction), and the remaining low-severity items. These are documented in the
review and are candidates for follow-up work.

## Correction to the review

**L13 was a false positive.** The review claimed the `inv` handler should use
`broadcast_except` because the derived `getdata` "echoes back to the sender".
It does not: the `getdata` is a *request* broadcast to all peers, and the
announcer is typically the peer that holds the block. Applying the suggested
change stalled block propagation and failed the daemon reorg tests; it was
reverted within `65b76bd`.
