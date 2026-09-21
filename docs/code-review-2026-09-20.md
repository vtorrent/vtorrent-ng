# Codebase review — third pass (2026-09-20)

Third read-only pass, scoped to the **46 commits / 4,138 insertions** landed
since the second pass (`6d1403a`). Most of that delta is the fix code from the
2026-09-15 reviews, which had never itself been reviewed — so this pass is
primarily a review of the fixes.

Three parallel deep passes (consensus/chain/mempool, RPC/daemon/wallet,
P2P/overlay/torrent) with independent verification of every critical/high
finding by the reviewer. No files modified; no fleet action. Line numbers are
exact at `main` = `e99401a`.

## Critical

### T1. Failed reorg leaves `total_staked` drifted → consensus split
**CWE-682** · `vtorrent-node/src/chain/chain_reorg.rs:413-431`

`reorganize_to` snapshots `height_index`, `tx_index`, `utxo_set`,
`claimed_addresses`, `journals`, and `total_supply` before calling
`reorganize_to_inner`, and restores all of them on error — but **not
`total_staked`**. Meanwhile the inner function mutates it twice before it can
fail: `rollback_one_block` reverses each rolled-back block's `staked_delta`
(`:98-106`) and `apply_block_journaled` applies each fork block's
(`:196-204`).

**Trigger (remote, cheap).** `add_block`'s fork branch runs only
`validate_block_inner` (`chain.rs:781`), which does **not** check the coinstake
kernel, script execution, stake age, or the post-state root — those are
enforced later inside `apply_transaction_journaled`. So a structurally-valid
block on a longer fork with a bad coinstake/script passes validation, the
reorg rolls back the whole main chain (mutating `total_staked`), then
`apply_block_journaled` returns `Err` and the restore leaves the denominator
wrong.

**Why it matters.** `check_stake_kernel_v2` divides by `chain.total_staked`. A
node that took a failed reorg validates future blocks against a different
denominator than a freshly-synced node with an identical UTXO set — permanent
fork or halt. The `saturating_*` helpers hide the drift, so it is silent.

**The guarding test is vacuous.** `test_invalid_fork_reorg_restores_original_state`
asserts `total_staked()` is restored, but `make_block` mints `value: 1_000_000`
(`chain_tests.rs:16`) while `MIN_STAKE_AMOUNT = COIN = 100_000_000`
(`consensus.rs:43`). Every fixture output is below the minimum, so
`total_staked` is 0 throughout and the assertion never exercises the path.

**Fix:** snapshot `chain.total_staked` alongside `total_supply` and restore it
in the `Err` arm; add a regression test with a fixture output ≥
`MIN_STAKE_AMOUNT`.

## High

### T2. Unchecked `i64` accumulation of `staked_delta` → remote panic
**CWE-190 / CWE-617** · `vtorrent-node/src/chain/chain_reorg.rs:291,338`

`journal.staked_delta -= utxo.value as i64` and `+= output.value as i64`
accumulate in `i64` with plain arithmetic. `validate_transaction` caps each
*output* at `MAX_MONEY` (`consensus.rs:350`) but not their sum, and neither it
nor `validate_block_inner` bounds the output count. A ~1 MB block fits ~130k
empty-script outputs; at `MAX_MONEY` each, the running sum exceeds `i64::MAX`
after ~4,600 outputs. `[profile.release] overflow-checks = true`
(`Cargo.toml:95`) makes that a panic.

**Trigger:** a `Standard` transaction (spending one real UTXO, so script
verification passes) with tens of thousands of `MAX_MONEY`-valued
empty-script outputs, delivered as a block. The `total_output > total_input`
check (`:346`) and the `MAX_SUPPLY` check (`:187`) both run **after** the
outputs loop, so the overflow happens first. There is no `catch_unwind` in
`vtorrent-node`/`vtorrent-daemon`, so this unwinds the block-processing path.

**Fix:** accumulate in `i128`, or use `checked_add`/`saturating_add`, or bound
`tx.total_output()` before the loop.

## Medium

### T3. Genesis bootstrap: `total_staked == 0` rejects every kernel
`vtorrent-node/src/consensus.rs:178-180`

`check_stake_kernel_v2` returns `false` when `total_staked == 0`. Genesis
contains **zero** stakeable UTXOs — all 59,375 distribution outputs are
OP_RETURN (`genesis.rs:86-89`), which `is_stakeable` excludes, and the
coinbase output is 0. Production rejects PoW blocks, and the only production
paths that create spendable outputs are staking (needs `total_staked > 0`) and
`mint_to_address` (regtest/test only — the faucet is regtest-gated). So from a
fresh mainnet genesis, `total_staked` can never become positive and staking is
permanently halted.

The same "no spendable UTXO at genesis" condition predates C1 (v1 also could
not build a coinstake), so there may be an out-of-band launch/bootstrap
procedure not in-repo. Flagged because the explicit `total_staked == 0 → false`
makes it a hard rule with no fallback. **Fix:** define and assert the bootstrap
path (a launch block, a height-1 allowance, or a seeded spendable output).

### T4. `is_stakeable` over-counts outputs that can never stake
`vtorrent-node/src/consensus.rs:147-151`

`is_stakeable` accepts any non-OP_RETURN output ≥ `MIN_STAKE_AMOUNT`, but the
staking engine only ever stakes a UTXO whose script equals its own P2PKH script
(`staking.rs:242`). P2SH / P2MS / P2PK / HTLC / `NonStandard` outputs ≥ 1 VTR
therefore count in the denominator but can never win a kernel. Any holder can
park coins in such an output and permanently dilute every honest staker's hit
probability (global block rate drops below 1/tick) while retaining
spendability. Consensus stays internally consistent, so it is dilution, not a
split. **Fix:** restrict `is_stakeable` to the script class the engine can
spend, or make the engine able to stake the counted classes.

### T5. M2 egress budget does not bound a single `getdata`
`vtorrent-node/src/node/handler.rs:808`

The budget is checked **once, before** the item loop, then every item is served
accumulating via `saturating_add`. One request of `MAX_GETDATA_ITEMS = 500`
items can return ~500 MB in a single call; the budget is only consulted on the
*next* request. With up to 125 peers, egress is tens of GB — the amplification
the comment claims to bound. **Fix:** check the budget inside the loop and stop
serving once `served >= GETDATA_BYTE_BUDGET`.

### T6. The "bound the map" prunes are inverted — three caps never engage
`vtorrent-node/src/node/handler.rs:891,1791`; `vtorrent-overlay/src/relay.rs:79`

All three use
`retain(|_, (_, start)| now.duration_since(*start) < WINDOW)`, which **keeps
fresh entries and drops old ones** — backwards for bounding a flood, whose
entries are all fresh. Under a sustained flood the map grows without bound.
**Fix:** evict by count (drop down to the cap), not by window age.

### T7. M7 relay quota keyed by `SocketAddr` (includes source port)
`vtorrent-overlay/src/relay.rs:50,69-77`

`request_counts` is keyed by the full `SocketAddr`; UDP source ports are
attacker-chosen, so rotating the port grants a fresh 60-request quota each
time. The relay path is unauthenticated UDP, so the per-requester limit is
effectively absent. Session dedup (`:172`) has the same weakness. **Fix:** key
by `from.ip()` (normalize IPv6 to /64).

### T8. P2P DHT lacks the pending cap and source validation
`vtorrent-p2p/src/dht.rs:298,335`

The `MAX_PENDING` cap added to the torrent DHT (`vtorrent-torrent/src/dht.rs:284`)
is **absent** from the P2P DHT, which `bootstrap_via_dht` actually uses. It
also discards the response source (`_from`) and matches only the 2-byte
transaction id, unlike the torrent DHT's `src != node_addr` check. The M6
ledger note ("the torrent DHT already validates source") does not cover this
caller. **Fix:** port both guards; use a `VecDeque` for O(1) pop.

### T9. M9 torrent session cap is never released on completion
`vtorrent-torrent/src/session.rs:239-244`

`add_session` rejects at 64, but `run_engine` never calls `remove_session` —
only the RPC/Tauri remove handlers do. After 64 `add_torrent` calls the node
can never add another until each is manually removed: a caller-reachable DoS
and a UX regression. **Fix:** reap terminal sessions automatically, or cap only
active sessions and evict the oldest completed one.

## Low

- **T10. Overlay `PUNCH_MAX_ENTRIES` eviction is O(100k) per packet** —
  `overlay.rs:288-299` scans the whole map with `min_by_key` on every
  new-source packet at the cap. Memory is bounded, CPU is not. Fix: amortize
  eviction on the prune tick, or use a FIFO ring.
- **T11. Anonymous synthetic key is collision-prone** —
  `peer_manager.rs:718-732` derives a `SocketAddr` from a 32-bit FNV-1a. Two
  `.onion` peers can collide; the second is rejected by dedup, and since
  failure bans are keyed on the synthetic IP, an attacker who grinds a
  colliding `.onion` can suppress a victim. Fix: 64-bit hash; don't let
  anonymous failures feed the IP ban table.
- **T12. `is_anonymous_address` disagrees with the transport** —
  `peer_manager.rs:711-716` is case-sensitive; `vtorrent-onion/src/addr.rs:125`
  lowercases. `abc.ONION:22526` goes through `lookup_host` and fails. Fix:
  reuse `vtorrent_onion::addr::is_anon_addr`.
- **T13. SSRF guard has a DNS-rebinding TOCTOU and blocks the executor** —
  `vtorrent-torrent/src/tracker.rs:156` uses blocking `to_socket_addrs` inside
  `async fn announce` (`:231`), and `reqwest` resolves independently on
  connect, so a low-TTL hostname can pass the check as public then resolve
  private. Fix: pin the validated IP via `ClientBuilder::resolve_to_addrs`, and
  use `tokio::net::lookup_host`.
- **T14. PEX IPv4-mapped IPv6 gap** — `pex.rs:126-134` lacks the
  `to_ipv4_mapped()` re-check that `tracker.rs` has, so `::ffff:10.0.0.1`
  passes on mainnet. Also `is_bootstrap_seed` (`peer_manager.rs:66-72`) matches
  only literal IPs, not `DNS_SEEDS` hostnames, so L10's exemption silently
  stops applying if a seed's A record changes.
- **T15. Ban cap only enforced during `prune`** — `ban_manager.rs:355` runs
  from the PEX tick every 600s, so `bans` can grow freely between prunes.
- **T16. v1 claim signatures are now unverifiable; `claim_message_hash` is dead
  code** — `consensus.rs:456` accepts only v2. Verified replay-safe (the chain
  has no legacy claims), so this is a compatibility note. The v2 construction
  is sound and both signing paths use it.
- **T17. Multi-input coinstakes: reward/`minted` only account for `inputs[0]`** —
  `chain.rs:381-390`, `chain_reorg.rs:279-288`. The producer only ever emits
  single-input coinstakes, so producer/validator agree in practice; not
  exploitable for inflation (extra input value makes `minted` exceed
  `max_reward` and the block is rejected). Worth an explicit "coinstake must
  have exactly one input" rule.

## Verified correct

- **`total_staked` exactly-once on the happy path** — `apply_block_journaled`
  commits both deltas only after the tx loop; errors inside the loop call
  `rollback_journal`, which deliberately does not touch `chain.total_staked`
  (correct, since it was not applied). The direct UTXO-root-mismatch path in
  `chain.rs:694-706` manually reverses both. Producer and validator both use
  the pre-block value. **Except for T1**, delta application is exactly-once.
- **Kernel math** — `(kernel_val as u128) * (total_staked as u128) <=
  (utxo.value as u128) << 32` cannot overflow and matches the algebra.
- **`get_transactions` topological sort** — Kahn over mempool-internal edges;
  duplicate input pairs counted and decremented consistently; `BTreeSet` ready
  order is deterministic; leftovers only for impossible cycles and are emitted,
  not dropped.
- **`pending_claims` cleanup** — every removal funnels through `remove_entry`,
  which clears the mapping only when it still points at the removed txid.
- **Mempool byte budget / eviction loop** — terminates; no panic; oversized tx
  rejected up front.
- **Duplicate-input rejection, `coinstake_reward` single-input subtraction, L4
  real serialized size, `GENESIS_BITS`, L3 lock order** — all correct.
- **S6 SPV PoW** — the `exponent > 3` rewrite is correct for all
  exponent/mantissa values; no panic; does not reject valid targets.
- **H4 bencode, S7 `decompress_amount`, S2 metadata cap, S8 `piece_length`,
  M16 `Command` rewrite, L12 ingest ordering (every tag), L11 routing, L18 SPV
  batch cap, L7 PEX ranges (except T14), L8 checked send-counter, `hex_decode`
  byte iteration** — all correct.

## Priority

1. **T1** — silent consensus split; the guarding test is vacuous.
2. **T2** — remote panic on a crafted block.
3. **T3** — mainnet launch blocker if no bootstrap path exists.
4. **T5, T6, T7, T8, T9** — the DoS bounds I added do not actually bound.
5. **T4** — stake dilution griefing.
6. Low items as capacity allows.

## Not fully verified

- Whether an out-of-band mainnet bootstrap/premine block exists (T3's severity
  depends on it).
- Whether any v1-scheme legacy claim exists on the live/persisted chains (T16's
  blast radius).
- Panic containment for T2 — no `catch_unwind` and no `panic = "abort"` found,
  so a panic likely kills the task or process; the exact blast radius is
  unconfirmed.
