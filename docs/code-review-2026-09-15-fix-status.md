# Code review fix status — 2026-09-16

Per-finding ledger for the two 2026-09-15 reviews
(`docs/code-review-2026-09-15.md`, `docs/code-review-2026-09-15-second-pass.md`).

All fixes were made on `main`, verified with `cargo test --workspace`
(47 test binaries, 0 failures), `cargo clippy --workspace --all-targets
--all-features` (clean) and `cargo fmt --all -- --check`. No fleet action was
taken for the code fixes; the soak was not interrupted by them.

**All critical findings are fixed**, including C1. **All high findings are now
fixed**, including S4 and S5. The remaining open items are the lower-priority
medium/low findings listed at the end.

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
| **C1 stake-kernel target saturation** | `a3dd177` | v2 rule normalizes by total staked supply; whale capped at its stake share |
| **S4 BTC claim not fee-bumpable** | `6edc2e3` | Claim signals RBF; `btc-claim-bump` RPC + persisted raw claim |
| **S5 preimage handoff missing** | `c37121f` | Scan extracts the claim preimage; `vtr-claim` accepts an observed preimage |
| **M8 tracker SSRF** | `e62415e` | Scheme allow-list + non-public address rejection (HTTP and UDP) |
| **M10 wallet import overwrite** | `e62415e` | Requires explicit `overwrite: true` |
| **M15 TOTP replay** | `e62415e` | Matched time-step tracked; a used step is rejected |
| **M12 BTC spend authorization** | `e346879` | `btc/send` and `btc-fund` now require an unlocked wallet |

## C1 — fixed (`a3dd177`)

`vtorrent-node/src/consensus.rs`. The v1 target `min(value/1000, u32::MAX)`
saturated for any UTXO worth at least 42,949.67 VTR, letting its owner produce
every block. Replaced by a proportional rule:

```
P(hit) = value / total_staked        (per tick)
kernel_val * total_staked <= value * 2^32   (exact integer form)
```

Because the per-staker probabilities sum to 1, a staker's block share equals
its stake share; no stake size reaches probability 1 unless it is the entire
staked supply.

Supporting changes:
- `Chain::total_staked` is tracked incrementally, counting only stakeable
  UTXOs (>= `MIN_STAKE_AMOUNT`, not OP_RETURN) so the unspendable genesis
  distribution cannot dilute the denominator. The delta is journaled and
  reversed on both rollback paths.
- The validator uses the pre-block `total_staked` (the field is only updated
  after the transaction loop), so producer and validator agree exactly.
- The producer threads `total_staked` through the staking engine.

**No activation height is required.** v2 is strictly easier than v1 whenever
`total_staked` is below 42,949.67 VTR, so every historical block on the soak
chain (`total_staked` ≈ 504 VTR) still validates on replay. A large chain gets
the proportional guarantee from genesis.

**Upgrade note.** This is a consensus-rule change. It is replay-compatible
with the current chain, but a fleet running the old binary would diverge once
`total_staked` exceeds 42,949.67 VTR, so the rollout must be coordinated (all
nodes upgraded together, or a fresh chain). Deployed to the testnet fleet on
2026-09-17 (`9affdf6`); see `docs/soak-log.md`.

Tests added: a 90%-of-stake whale hits ~90% of kernels (not 100%); a sole
staker still wins every tick; a 1% staker hits ~1%; v2 accepts every v1 hit
below saturation; zero `total_staked` rejects; `is_stakeable` excludes
OP_RETURN and dust; `total_staked` is restored across a reorg and tracks
mint/UTXO changes.

## S4 — fixed

`vtorrent-btc/src/htlc.rs`. The claim was built with `Sequence::MAX`, so it
did not signal RBF and a stalled claim could not be replaced. Because the
claim reveals the preimage, a taker's higher-fee refund could then win the
race at expiry, costing the maker both legs (reveal-then-lose).

Changes:
- The claim input now uses `Sequence::ENABLE_RBF_NO_LOCKTIME` (BIP-125). The
  claim branch has no CLTV, so locktime stays disabled.
- The refund keeps `ENABLE_LOCKTIME_NO_RBF` (it needs CLTV). This asymmetry is
  what prevents a refund from replacing a claim: BIP-125 requires the
  *replacement* to signal RBF, and the refund does not.
- The signed claim is persisted (`SwapState::btc_claim_raw`) so it can be
  rebuilt after a restart, and fee-approved replacements are recorded
  (`btc_claim_replacements`, append-only, capped at 8).
- New `POST /api/v1/swap/btc-claim-bump` (`vtorrent-rpc/src/btc_claim_bump.rs`)
  rebuilds the claim at a higher fee. It requires explicit fee approval and
  durable recovery, enforces BIP-125 rule 4 (strictly higher absolute fee),
  rejects stale parents and superseded retries, and refuses once the claim
  window is too close to refund eligibility.

Tests: the claim signals RBF and the refund does not; the bump requires
approval and durable recovery; a non-increasing fee is rejected; a valid bump
produces an RBF-signalling replacement paying the higher fee; a stale parent
is rejected.

## S5 — fixed

The review's core claim was correct: no code extracted the preimage from the
maker's observed BTC claim, so a taker on a different node could not complete
the swap.

Investigation corrected the review on one point: `OrderAnnouncement`
deliberately omits `funding_txid`/`taker_address` (its doc comment says they
"must never leave the maker's node"), and `broadcast_order` has no production
caller. That is a *separate, larger* feature — gossiping matched-order terms —
which is not required for the preimage handoff and is left as follow-up.

The preimage handoff itself is implemented:
- `SwapScanTracker::record` inspects the witness of the transaction spending
  the BTC HTLC funding output and extracts any 32-byte element whose SHA-256
  equals the hash lock. A refund witness has no such element, so it yields
  nothing.
- `SwapScan` carries the extracted `preimage`; `btc_reconciliation` stores it
  in `SwapState::preimage` **only once the claim is confirmed** (an
  unconfirmed spend could be replaced). It is deliberately *not* placed in
  `BtcSwapObservation`, which is asserted to stay secret-free.
- `VtrClaimRequest::preimage` is now optional. When empty, `vtr-claim` uses
  the preimage recovered from the taker's own observation, so the taker does
  not need it out of band.

Tests: a valid claim witness yields the preimage; a mismatching 32-byte
element is ignored; a refund witness yields nothing; `vtr-claim` succeeds with
an empty `preimage` once the observation recorded it; and it is rejected when
neither is available.

## Not fixed

### Lower-priority medium/low

M2 (`getdata` bandwidth accounting), M5 (PEX per-peer quota), M6 (DHT source
validation — the torrent DHT already validates source and tid), M7 (overlay
relay auth), M9 (torrent session cap), M11 (full-chain scans under the chain
mutex), M16 (`fork()` safety), M17 (quadratic eviction), and the remaining
low-severity items. These are documented in the review and are candidates for
follow-up work.

## M12 — fixed (broader than reported)

The review described M12 as "`btc_fund` spends the node's BTC wallet with only
the shared key". Investigation found the exposure was wider: **`POST
/api/v1/btc/send` had no wallet-unlock gate either**, so any holder of the RPC
API key could drain the node's BTC wallet directly — the key was effectively a
BTC spending credential.

Both paths now require an unlocked wallet, matching the existing VTR behaviour
(`send_vtr` already returns `WalletLocked`):

- `send_btc` (`vtorrent-rpc/src/handlers/btc.rs`) checks
  `is_wallet_unlocked` before signing.
- `fund_btc_with_broadcast` (`vtorrent-rpc/src/handlers/swap.rs`) does the
  same.
- The Tauri `send_btc` command talks to the wallet directly and would have
  bypassed the RPC handler, so it checks too.

**Deliberately not gated:** the refund path. `docs/rpc-api.md` documents that
"BTC refund is independent of VTR expiry and VTR wallet unlock", and a refund
must remain possible when the wallet is locked. The existing test
`btc_refund_is_independent_and_retry_preserves_raw_transaction` locks the
wallet and asserts the refund still works; it still passes.

Tests: `btc/send` and `btc-fund` both return 403 with a locked wallet.

## Security batch (M8, M10, M15)

Three security-relevant medium findings were fixed together:

- **M8 tracker SSRF.** Tracker URLs come from untrusted `.torrent` files and
  magnet links, and were fetched with no scheme or host restriction. Added
  `validate_tracker_url`, which requires `http`/`https` and rejects any host
  resolving to a loopback, private, link-local, CGNAT, benchmarking, reserved,
  multicast, or IPv6 unique-local/link-local address. Redirects are disabled
  so a public tracker cannot redirect to an internal target. The UDP path
  (which resolves its own address) uses the same `tracker_target_allowed`
  check. Tests and local deployments can opt out via
  `set_allow_private_tracker_targets`.
- **M10 wallet import overwrite.** `import_wallet` replaced the encrypted hot
  wallet with no confirmation, so one call destroyed the previous key and any
  funds it controlled. It now refuses unless the request sets
  `"overwrite": true`.
- **M15 TOTP replay.** Codes are valid for a ±1-step window (~90s) with no
  used-code tracking, so an observed code could be replayed. `verify_step` now
  returns the matched time-step and the RPC auth path records the highest
  accepted step, rejecting any step at or below it.

Tests: non-HTTP schemes rejected; loopback/private/link-local/CGNAT/IPv6
targets rejected; public literals allowed; the explicit override works; a
second import without `overwrite` is refused and succeeds with it; a TOTP code
cannot be replayed while still inside its validity window.

## Correction to the review

**L13 was a false positive.** The review claimed the `inv` handler should use
`broadcast_except` because the derived `getdata` "echoes back to the sender".
It does not: the `getdata` is a *request* broadcast to all peers, and the
announcer is typically the peer that holds the block. Applying the suggested
change stalled block propagation and failed the daemon reorg tests; it was
reverted within `65b76bd`.
