# Code review third pass — fix status (2026-09-20)

Per-finding ledger for `docs/code-review-2026-09-20.md`.

All fixes verified with `cargo test --workspace` (47 test binaries, 0
failures), `cargo clippy --workspace --all-targets --all-features` (clean),
`cargo fmt --all -- --check`, and `cargo machete`.

T3 and T4 are consensus-rule changes that are **no-ops on every existing
chain** (the soak chain is P2PKH-only and its height-1 block is a regtest
faucet coinbase, not a claim), so they replay unchanged. They ship with the
post-soak batch, not the running soak fleet. See
`docs/superpowers/specs/2026-09-21-genesis-bootstrap-and-stakeable-script-design.md`.

## Fixed

| Finding | Commit | Notes |
|---|---|---|
| T1 failed reorg leaves `total_staked` drifted | `d39d902` | Restore it in the error path; test proven non-vacuous |
| T2 `i64` overflow in `staked_delta` | `d39d902` | `checked_add`/`checked_sub` with a clean error |
| T5 `getdata` budget checked only before the loop | `d39d902` | Now checked inside the loop |
| T6 inverted map prunes (getdata, PEX, relay) | `d39d902` | Evict by count, not window age |
| T7 relay quota keyed by `SocketAddr` | `d39d902` | Keyed by `IpAddr` |
| T8 P2P DHT missing pending cap + source check | `d39d902` | Both ported from the torrent DHT |
| T9 torrent session cap never released | `d39d902` | Terminal sessions don't count and are evictable |
| T3 genesis bootstrap: `total_staked == 0` rejects every kernel | `09c3674`, `3f6294d`, `e9fc213` | Height-1 bootstrap claim block + `POST /api/v1/blockchain/bootstrap`; see design spec |
| T4 `is_stakeable` over-counts non-P2PKH outputs | `f4d1ad0` | Restricted to P2PKH (the class the engine can spend) |

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

### Low findings (T10–T17)

Not fixed; documented in the review. T10 (overlay eviction is O(100k) per
packet), T11 (32-bit synthetic-key collisions), T12 (`is_anonymous_address`
case-sensitivity), T13 (tracker SSRF DNS-rebinding TOCTOU + blocking resolve),
T14 (PEX IPv4-mapped gap; `is_bootstrap_seed` ignores DNS seeds), T15 (ban cap
only enforced during prune), T16 (v1 claim signatures now unverifiable — a
compatibility note), T17 (multi-input coinstake accounting — not exploitable).
