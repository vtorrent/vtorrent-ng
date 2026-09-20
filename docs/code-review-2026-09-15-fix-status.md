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
| **M2 getdata bandwidth amplification** | `506c344` | Per-peer egress byte budget (64 MB/hour) |
| **M5 PEX address-book flooding** | `506c344` | Per-peer address quota (2000/hour) |
| **M7 overlay relay abuse** | `506c344` | Per-requester relay quota (60/min) |
| **M9 unbounded torrent sessions** | `506c344` | Cap of 64 concurrent sessions |
| **M11 full-chain scans under the lock** | `506c344` | Bounded scan depth (200k blocks) |
| **M17 quadratic eviction scan** | `506c344` | Frontier is a HashSet (linear per level) |
| **M16 `fork()` safety** | `a2d56a1` | Replaced raw fork/execvp with `std::process::Command` |
| **L7 PEX accepts multicast/CGNAT/reserved** | `bdc6868` | Added the missing IPv4 range checks |
| **L8 overlay send-counter overflow** | `bdc6868` | `checked_add` instead of `+= 1` |
| **L10 seed ban escalation** | `bdc6868` | Bootstrap seeds exempt from failure bans |
| **L18 unbounded SPV header batch** | `bdc6868` | Batch capped at 2000 headers |
| **DNS_SEEDS missing seed3** | `bdc6868` | Added `seed3.vtorrent.org` (deferred item) |
| **L5 partial legacy claim strands funds** | `TBD` | Claim must match the snapshot balance exactly |
| **L6 conflicting claims both admitted** | `TBD` | Mempool tracks pending claim addresses |

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

M6 (DHT source validation — the torrent DHT already validates source and tid)
and the remaining low-severity items (L3, L4, L9, L11, L12, L17, L19, L20).
These are documented in the review and are candidates for follow-up work.

## L5 / L6 — fixed (legacy-claim fund safety)

Two legacy-claim edge cases that could strand funds:

- **L5 partial claim.** `validate_legacy_claim` accepted any amount up to the
  snapshot balance. A claim for less than the full balance would permanently
  strand the remainder, because the address is marked claimed and can never be
  claimed again. The check now requires an exact match. Both claim builders
  (RPC `submit_claim`, Tauri `claim_legacy`) already used the full snapshot
  balance, so the stricter rule is compatible with the existing flow.
- **L6 conflicting claims.** Legacy claims have no inputs, so the mempool's
  spent-input conflict detection could not see two claims for the same
  address; both could sit in the mempool even though only one can confirm.
  The mempool now tracks pending claim addresses and rejects a competing
  claim, clearing the entry when the claim leaves.

Tests: an under-claim and an over-claim are both rejected while an exact
claim passes; a competing claim for a pending address is rejected, a claim
for a different address is accepted, and the address frees up once the first
claim leaves the mempool.

## Low-severity batch (L7, L8, L10, L18) + deferred seed3

Four low-severity findings with real (if minor) consequence, plus the
long-deferred `DNS_SEEDS` gap:

- **L7 PEX address filtering.** IPv4 multicast, carrier-grade NAT
  (100.64/10), benchmarking (198.18/15), and reserved (240/4) ranges were
  accepted into the address book, letting a peer poison it with unusable
  entries. Added the missing checks.
- **L8 overlay send-counter overflow.** `session.send_counter += 1` would
  panic under `overflow-checks` after 2^64 messages; now `checked_add` with a
  rekey error.
- **L10 seed ban escalation.** Five transient connection failures to a
  configured bootstrap seed escalated to an hour-long ban, cutting the node
  off from bootstrap. Seeds are now exempt.
- **L18 unbounded SPV header batch.** `POST /api/v1/spv/headers` accepted an
  unbounded header list, each retained in the chain. Capped at 2000.
- **`DNS_SEEDS` missing seed3.** The deferred item from the post-soak batch:
  `seed3.vtorrent.org` was absent, so a DNS-only bootstrap could never reach
  the third seed. Added (the A record was already live).

Tests: bootstrap seeds are recognised and non-seeds are not; `DNS_SEEDS`
covers all three; mainnet PEX rejects multicast/CGNAT/benchmarking/reserved.

## M16 — fixed

`vtorrent-migrate/src/bdb.rs`. The legacy-wallet extractor shelled out to
`db5.3_dump` using a raw `libc::fork`/`execvp`, and the child called
`CString::new(..).unwrap()` and `libc::open` between `fork` and `execvp`. In a
multithreaded process only async-signal-safe functions are legal there:
allocation can deadlock on a lock held by another thread at fork time, and a
panic in the child unwinds into an undefined state.

Replaced with `std::process::Command`, which sets up the stdio redirection
before forking and execs directly. Behaviour is unchanged (stdout captured to
a temp file, stderr to /dev/null, non-zero exit surfaced as an error), and the
now-unused `libc` dependency was removed (`cargo machete` clean).

Tests: a non-BDB input and a bad flag both return an error rather than
panicking.

## DoS-hardening batch (M2, M5, M7, M9, M11, M17)

Six resource-exhaustion findings fixed together. None is security-critical
(no fund or key exposure), but each lets a remote peer or caller consume
disproportionate resources:

- **M2 `getdata` amplification.** A 500-item `getdata` is ~16 KB in but can
  request ~500 MB out; the message-count limiter did not bound egress. Added a
  per-peer served-byte budget (64 MB/hour), with the map bounded and pruned.
- **M5 PEX address-book flooding.** One peer could fill the 10k-entry address
  book and evict legitimate entries (single-peer eclipse). Added a per-peer
  contribution quota (2000 addresses/hour).
- **M7 overlay relay abuse.** Relay requests were unauthenticated and
  unrate-limited at that layer. Added a per-requester quota (60/min).
- **M9 unbounded torrent sessions.** `add_session` had no cap, and each
  session spawns a task. Now capped at 64; both the RPC and Tauri callers
  reject with a clear error.
- **M11 full-chain scans under the chain mutex.** `get_transactions` walked
  from height 0, holding the chain lock shared with P2P block processing. The
  scan is now bounded to 200k blocks. A full address→txids index remains the
  proper fix for unbounded history.
- **M17 quadratic eviction scan.** The descendant-eviction frontier was a
  `Vec` scanned with `contains` per entry; now a `HashSet`, making each level
  linear.

Tests: the torrent session cap rejects past the limit.

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
