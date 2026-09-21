# Code review third pass — fix status (2026-09-20)

Per-finding ledger for `docs/code-review-2026-09-20.md`.

All fixes verified with `cargo test --workspace` (47 test binaries, 0
failures), `cargo clippy --workspace --all-targets --all-features` (clean),
`cargo fmt --all -- --check`, and `cargo machete`.

## Fixed

| Finding | Commit | Notes |
|---|---|---|
| T1 failed reorg leaves `total_staked` drifted | `TBD` | Restore it in the error path; test proven non-vacuous |
| T2 `i64` overflow in `staked_delta` | `TBD` | `checked_add`/`checked_sub` with a clean error |
| T5 `getdata` budget checked only before the loop | `TBD` | Now checked inside the loop |
| T6 inverted map prunes (getdata, PEX, relay) | `TBD` | Evict by count, not window age |
| T7 relay quota keyed by `SocketAddr` | `TBD` | Keyed by `IpAddr` |
| T8 P2P DHT missing pending cap + source check | `TBD` | Both ported from the torrent DHT |
| T9 torrent session cap never released | `TBD` | Terminal sessions don't count and are evictable |

### T1 — failed reorg left `total_staked` drifted

`reorganize_to` (`chain_reorg.rs`) snapshotted and restored six fields on error
but not `total_staked`, while `reorganize_to_inner` mutates it twice before it
can fail. A node that took a failed reorg would validate future blocks against
a different kernel denominator than a freshly-synced node with an identical
UTXO set — a silent consensus split.

**The guarding test was vacuous**, which is why this shipped: its fixture
minted `1_000_000` sat, below `MIN_STAKE_AMOUNT` (`100_000_000`), so
`total_staked` was 0 throughout and the assertion never ran. The test now
mints a stakeable output via `mint_to_address` and asserts the value is
non-zero before the reorg. **Verified non-vacuous**: with the fix reverted the
test fails; with it applied it passes.

### T2 — `i64` overflow in `staked_delta`

`journal.staked_delta -= utxo.value as i64` / `+= output.value as i64`
accumulated unchecked before the `total_output > total_input` check, so a
crafted block with tens of thousands of `MAX_MONEY` outputs panicked the
block-processing path under `overflow-checks = true`. Both sites now use
`checked_add`/`checked_sub` and return a clean `InvalidTransaction` error.

### T5 — `getdata` egress budget

The budget was consulted once before the item loop, so a single 500-item
request could return ~500 MB before the budget was checked again. It is now
checked at the top of each iteration.

### T6 — inverted map prunes

`retain(|_, (_, start)| now - start < WINDOW)` keeps the *freshest* entries —
exactly what a flood produces — so the cap never engaged. All three sites
(getdata, PEX, relay) now evict by count, dropping the oldest down to 75% of
the cap.

### T7 — relay quota keyed by `SocketAddr`

UDP source ports are attacker-chosen, so keying the per-requester quota by the
full `SocketAddr` let a requester rotate ports for a fresh quota each time. Now
keyed by `IpAddr`.

### T8 — P2P DHT pending cap and source validation

The `MAX_PENDING` cap and the `src != node_addr` check added to the torrent DHT
were absent from the P2P DHT, which is the one `bootstrap_via_dht` actually
uses. Both are now ported, with duplicate-address suppression on push.

### T9 — torrent session cap never released

`add_session` rejected at 64, but the engine never calls `remove_session`, so
64 `add_torrent` calls permanently locked out new sessions. The cap now counts
only *active* sessions and evicts the oldest completed one to make room.
`SessionState::is_terminal` defines "completed" as `Seeding`/`Error`/`Stopped`
(the engine's peer tasks have exited in those states).

Tests: completed sessions do not block new ones; active sessions still hit the
cap.

## Open

### T3 — genesis bootstrap: `total_staked == 0` rejects every kernel

`check_stake_kernel_v2` returns `false` when `total_staked == 0`, and genesis
has zero stakeable UTXOs (all distribution outputs are OP_RETURN; the coinbase
is 0). Production rejects PoW, and the only production paths that create
spendable outputs are staking (needs `total_staked > 0`) and `mint_to_address`
(regtest-only faucet). So from a fresh mainnet genesis, staking can never
start.

The same condition predates C1 (v1 also could not build a coinstake), so there
is likely an out-of-band launch/bootstrap procedure not in-repo. **Not fixed**:
this needs a product decision on the bootstrap path (a launch block, a
height-1 staking allowance, or a seeded spendable output), not a mechanical
edit. Flagged as a mainnet-launch blocker.

### T4 — `is_stakeable` over-counts outputs that can never stake

`is_stakeable` accepts any non-OP_RETURN output ≥ `MIN_STAKE_AMOUNT`, but the
engine only stakes P2PKH outputs matching its own script. P2SH/P2MS/P2PK/HTLC/
`NonStandard` outputs ≥ 1 VTR count in the denominator but can never win a
kernel, so a holder can park coins to dilute every honest staker's hit
probability. **Not fixed**: the correct fix is to restrict `is_stakeable` to
the script class the engine can spend, which is a consensus-rule change and
needs the same coordinated-upgrade treatment as C1. Dilution only (no split).

### Low findings (T10–T17)

Not fixed; documented in the review. T10 (overlay eviction is O(100k) per
packet), T11 (32-bit synthetic-key collisions), T12 (`is_anonymous_address`
case-sensitivity), T13 (tracker SSRF DNS-rebinding TOCTOU + blocking resolve),
T14 (PEX IPv4-mapped gap; `is_bootstrap_seed` ignores DNS seeds), T15 (ban cap
only enforced during prune), T16 (v1 claim signatures now unverifiable — a
compatibility note), T17 (multi-input coinstake accounting — not exploitable).
