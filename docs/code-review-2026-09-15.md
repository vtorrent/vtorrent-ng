# Codebase bug & edge-case review — 2026-09-15

> **Fix status (2026-09-16).** Most findings below are now fixed; see
> `docs/code-review-2026-09-15-fix-status.md` for the per-finding ledger.
> The one deliberately unfixed item is **C1** (stake-kernel target
> saturation), which is a consensus-rule change requiring a design
> decision — see that document for why a mechanical fix is not possible.

Read-only review of the whole workspace (19 Rust crates, ~67k LOC, plus the
React/TS frontend). Four parallel deep passes (consensus/chain, RPC/Tauri,
wallet/migrate/crypto, P2P/overlay/onion) plus independent verification of
every critical/high finding by the reviewer. No files were modified and no
fleet action was taken.

Every finding below was traced to a reachable call path and, where practical,
reproduced. Findings are ranked by severity. Line numbers are exact at
`main` = `734055f`.

## Critical

### C1. Stake-kernel target saturates at `u32::MAX` — large UTXOs win every slot
**CWE-682** · `vtorrent-node/src/consensus.rs:132-133`, producer at
`vtorrent-node/src/staking.rs:382`

```rust
let target = (utxo.value / 1000).min(u32::MAX as u64) as u32;
kernel_val <= target
```

`kernel_val` is a `u32`, so once `utxo.value >= (2^32-1)*1000` satoshis
(**42,949.67 VTR**) the target clamps to `u32::MAX` and the comparison is
**always true** — that UTXO meets the kernel at any timestamp, so its owner
can produce every block.

Reachability verified by decoding `vtorrent-node/src/genesis_snapshot.bin`
independently: **72 legacy addresses hold ≥42,949.67 VTR, totalling
9,898,080.28 VTR = 85.4% of the legacy supply.** The largest single holder
(540,182.73 VTR) can mint every block on its own. Any post-launch whale over
the threshold has the same power. Producer and validator share the formula,
so this is a design flaw, not a chain split.

**Fix:** scale the target against total network stake/weight and never let
per-attempt probability reach 1 (compare the full 256-bit kernel hash against
`value/weight × 2^256`, or cap `target` at a small fraction of `u32::MAX`).

## High

### H1. Unauthenticated remote panic via non-ASCII hex (byte-slice truncation)
**CWE-20 / CWE-125** · `vtorrent-rpc/src/handlers/mod.rs:63-70`

```rust
&value[..value.len().min(64)]
```

`value` is attacker-controlled and this is a **byte** slice. If byte 64 lands
inside a multi-byte UTF-8 character, Rust panics. Reproduced locally: 63 `a`s
+ `é` (65 bytes) panics with "end byte index 64 is not a char boundary".

Reachable **unauthenticated, non-rate-limited** on
`GET /api/v1/blockchain/block/:hash` (`server.rs:166`) and
`GET /api/v1/blockchain/tx/:txid` (`server.rs:167`). The same pattern appears
at `handlers/blockchain.rs:175`, `handlers/btc.rs:84`,
`handlers/dex.rs:161,163,278,313`, `handlers/wallet.rs:369,394,427`. No
`CatchPanicLayer` is installed, so the handler task unwinds and the connection
drops (instability/log-spam, not RCE).

**Fix:** char-safe truncation (`value.chars().take(64).collect::<String>()`,
as already done in `regtest.rs:67`) or `value.get(..min)`.

### H2. Wallet/staking/DEX read endpoints are not API-key protected
**CWE-306 / CWE-862** · `vtorrent-rpc/src/server.rs:142-157`

`public_routes` carries only `ip_rate_limit`, not `require_api_key`, and
includes `/api/v1/wallet/balance`, `/wallet/addresses`, `/wallet/utxos`,
`/wallet/transactions`, `/staking/status`, `/staking/rewards`, `/dex/orders`.
With `--rpc-addr` non-loopback (which forces an API key, `config.rs:188-201`),
**anyone** can read the hot wallet address, balances, UTXO set and tx history
without the key — contradicting the documented behaviour at `config.rs:96-101`
("wallet, staking … endpoints reject requests that do not include the matching
X-API-Key"). The existing test (`server/server_tests.rs:854`) only covers
`/wallet/unlock` and `/info`, so the gap is untested.

**Fix:** move wallet/staking read routes into `protected` (or apply `auth` to
`public_routes`) and extend the test.

### H3. Global 5-permit semaphore runs before auth — unauthenticated starvation
**CWE-770 / CWE-400** · `vtorrent-rpc/src/server.rs:89-91,139-140`

`.layer(auth).layer(rate)` makes `rate_limit` the **outer** layer (verified
against axum 0.7.9 `Router::layer` semantics: each call wraps the previous
stack). Every protected request — including ones with no/invalid key — first
consumes one of 5 global permits held for the whole handler. Five concurrent
requests to `POST /api/v1/wallet/unlock` (Argon2id 64 MB × 3 iters) saturate
the pool and return 429 to the legitimate owner. There is no per-IP limit on
protected routes.

**Fix:** apply `auth` outside `rate` (swap the `.layer` order) and add a
per-IP limit to protected routes.

### H4. DHT bencode length overflow panics the node
**CWE-190 / CWE-617** · `vtorrent-p2p/src/dht.rs:216`

```rust
let len: usize = len_str.parse().ok()?;   // parses usize::MAX
if start + len <= data.len() {            // overflow → panic
```

`[profile.release] overflow-checks = true` (`Cargo.toml:95`), so this is a
hard panic, not a wrap. Reproduced locally with `start + usize::MAX`.

Reachable from any host that can deliver UDP to the node's ephemeral DHT
socket: `discover_peers` ignores the response source address and only checks a
2-byte transaction id (`dht.rs:325`), and the same parser runs in `announce()`
every 30 minutes. A malicious DHT node returns a `get_peers` response
containing `5:nodes18446744073709551615:`.

**Fix:** `checked_add` and reject `len > data.len() - start` before any
arithmetic; bound the numeric string length before `parse`.

### H5. Mempool is bounded by transaction count, not bytes
**CWE-770** · `vtorrent-node/src/mempool.rs:68,224`; `total_bytes()` exists at
`:490` but is used only by metrics.

`max_mempool` defaults to 10,000 (`node/mod.rs:167`) and admission checks only
`entries.len()`. `validate_transaction` imposes no size cap and the P2P path
accepts up to `MAX_BLOCK_SIZE` (1 MB) per tx, so a peer can hold ~10 GB
resident. At the 1 sat/byte floor a ~1 MB tx costs ~0.17 VTR of real inputs —
cheap remote memory exhaustion.

**Fix:** enforce a byte budget (`total_bytes() + size <= max_bytes`) and cap
per-tx relay size.

### H6. Staker can build a child-before-parent block and wedge staking
**CWE-696 / liveness** · `vtorrent-node/src/staking.rs:280-297`,
`mempool.rs:380-384`, `node/staking_loop.rs:77-85`

`get_transactions()` sorts by fee-rate descending; `build_from_kernel_with_proof`
only rejects input conflicts against a `seen` set and never topologically
orders parent→child. A child paying a higher fee rate sorts before its parent,
so `assemble_block` emits `[coinstake, child, parent]`. On apply
(`chain/chain_reorg.rs:216-272`) the child's input is not yet in the UTXO set
→ `Err`, `attempt_stake` fails, and the same ordering repeats every tick:
persistent staking wedge. Trivial for any user to create.

**Fix:** topologically sort candidates (parents first) or drop txs whose
inputs are not resolvable in the pre-state.

### H7. Duplicate inputs accepted — fabricated fee, permanently unconfirmable
**CWE-682 / CWE-20** · `chain.rs:364-375` (`compute_tx_fee`),
`consensus.rs:279-329` (`validate_transaction`)

A tx listing the same outpoint twice passes `validate_transaction` (no
duplicate check). `compute_tx_fee` looks the UTXO up twice and
`saturating_add`s its value twice, fabricating a high fee; `verify_tx_scripts`
verifies each occurrence independently. It is admitted with the inflated fee
but can never be mined (second lookup returns `None` on apply). Fee-priority
poisoning plus a staker template that fails if included.

**Fix:** reject duplicate outpoints in `validate_transaction` and/or count each
input once in fee/script checks.

### H8. `vtorrent-migrate --json` dumps every WIF in cleartext
**CWE-532 / CWE-312** · `vtorrent-migrate/src/main.rs:74-78`,
`types.rs:108-118`

The human-readable path hides keys behind `VTORRENT_SHOW_WIF`
(`main.rs:96-102`), but the JSON path serializes `ExtractedKey` directly and
`wif: String` derives `Serialize`. `vtorrent-migrate wallet.dat --json` writes
all decrypted WIFs to stdout/logs, defeating the env-var gate.

**Fix:** redact `wif` in the `Serialize` impl unless an explicit opt-in is set.

### H9. Legacy decryption leaves key material in non-zeroized heap
**CWE-226 / CWE-316** · `vtorrent-migrate/src/crypter.rs:36-55,222-270,274-314`;
`extractor.rs:125,144-151`

Despite `Zeroize` derives on the result types, the intermediates are plain:
`derive_key_method0` builds `passphrase || salt` as a plain `Vec`;
`decrypt_master_key` holds `derived_key`/`key`/`iv` as plain locals;
`decrypt_private_key` returns a plain `Vec<u8>` holding the raw secp256k1
scalar; `candidates: Vec<String>` holds the raw and OTP-mixed passphrases.
Secrets persist in freed heap after use.

**Fix:** wrap all of these in `Zeroizing`; return `Zeroizing<[u8;32]>` from
`decrypt_private_key`.

### H10. `VTORRENT_MIGRATE_DEBUG` prints the derived AES key + IV
**CWE-532** · `vtorrent-migrate/src/crypter.rs:235-243`

Prints the AES-256-CBC key material protecting the wallet master key. Any
captured log lets an attacker decrypt mkey/ckeys. **Fix:** delete the block.

## Medium

- **M1. DEX `dexorder` gossip: unauthenticated amplification, event-loop
  stall, unbounded dedup set** — `node/handler.rs:1911-1934`,
  `node/mod.rs:238`. Any peer sends `dexorder`; the node re-broadcasts to every
  connected peer sequentially with `await` (1 s timeout per full queue), and
  `seen_orders` is never pruned (verified: only ever inserted). Amplification
  = peer count; sybils can stall the single node task. The rebroadcast also
  sits outside the `if let Some(book)` guard. Fix: cap payload/fields,
  `try_send` with bounded fanout, cap/evict `seen_orders`.
- **M2. `getdata` bandwidth amplification** — `node/handler.rs:766-854`. 500
  block hashes (~16 KB in) → up to 500 MB out, no per-peer upload accounting.
  Fix: per-peer bandwidth credits and byte caps.
- **M3. Overlay punch token-bucket map unbounded** —
  `vtorrent-overlay/src/overlay.rs:239,259-287`. Prune interval 300 s but TTL
  600 s, so peak size = all distinct source IPs in a 600 s window; no cap.
  Fix: hard-cap/LRU the map.
- **M4. Ban map unbounded** — `vtorrent-p2p/src/ban_manager.rs:141`. `scores`
  is capped at 50k but `bans` has no cap; each ban holds a heap `reason`.
  Fix: cap `bans` with LRU/by-expiry eviction.
- **M5. PEX address-book flooding / single-peer eclipse** —
  `vtorrent-p2p/src/pex.rs:202-244`. No per-peer contribution quota; a peer
  can fill `MAX_ADDR_BOOK_SIZE` and evict legitimate entries. Fix: per-peer
  quotas and provenance diversity.
- **M6. DHT responses not source-validated; predictable tid** —
  `dht.rs:312,325,387`. Sequential `u16` tid starting at 1, source ignored.
  Enables address injection and reaches H4. Fix: verify source, random tid.
- **M7. Overlay relay tags unauthenticated/unrate-limited** —
  `overlay.rs:308-354`, `relay.rs:67-152`. Only punch tags are token-bucketed;
  relay requests forward to registry members with no rate limit. Fix:
  authenticate requester, apply the bucket, cap payload/rate.
- **M8. SSRF via torrent tracker URLs** — `handlers/torrent.rs:42-59` →
  `vtorrent-torrent/src/tracker.rs:125`. User-controlled `announce`/`tr=` URLs
  fetched by the daemon (HTTP and UDP) with no scheme/host allow-list. Fix:
  restrict schemes, reject private/loopback/link-local targets.
- **M9. Unbounded torrent sessions/tasks** — `handlers/torrent.rs:63-78`,
  `tauri/commands/torrent.rs:88-113`. `SessionManager` has no cap and each
  `add_torrent` spawns a task. Fix: cap concurrent sessions.
- **M10. Wallet import silently overwrites the hot wallet** —
  `handlers/wallet.rs:246-253`. No confirmation, no existing-wallet check.
  Fix: refuse unless an explicit overwrite flag is set.
- **M11. Expensive full-chain scans under the `chain` mutex** —
  `handlers/wallet.rs:553-572`, `handlers/staking.rs:194-247`. `get_transactions`
  walks from height 0; `get_staking_rewards` scans up to 5000 blocks, both
  under the lock shared with P2P block processing. Fix: index address→txids or
  bound by time budget.
- **M12. `btc_fund` spends the node's BTC wallet with only the shared key** —
  `handlers/swap.rs:242-360`. No binding between caller and taker. Fix:
  separate operator credential / per-swap approval.
- **M13. WIF/secret copies unzeroized in the signing path** —
  `vtorrent-core/src/keys.rs:28-30,55,72`,
  `vtorrent-wallet/src/tx_builder.rs:162,204-207,240-248`. `from_wif` leaves
  `decoded`/`payload`/`key_bytes` in heap; `wif_keys: Vec<String>` and
  `key_pairs: Vec<([u8;32], Vec<u8>)>` hold secrets unzeroized, negating the
  caller's `Zeroizing` WIF. Fix: zeroize all copies.
- **M14. Predictable, symlink-followable wallet temp file; no fsync** —
  `vtorrent-wallet/src/wallet.rs:158-173`. Deterministic `wallet.json.tmp`
  opened without `O_EXCL` (symlink race) and no `sync_all()` before rename, so
  power loss can yield a truncated wallet despite the "atomic" claim. Fix:
  `NamedTempFile` + fsync file and directory.
- **M15. TOTP replay + wide window + secret copies** —
  `vtorrent-wallet/src/otp.rs:93-97,117-129`. `skew = 1` accepts ±1 step
  (~90 s) with no used-code cache, so an observed code can be replayed. Fix:
  track used time-steps, narrow the window.
- **M16. `fork()` in a multithreaded process with non-async-signal-safe child
  work** — `vtorrent-migrate/src/bdb.rs:232-281`. The pointers are sound, but
  the child calls `CString::new(...).unwrap()` (malloc/panic) and `libc::open`
  between `fork` and `execvp`; if another thread held the allocator lock the
  child can deadlock. `waitpid` return ignored. Fix: prefer
  `std::process::Command`; if unavoidable, pre-allocate before `fork` and use
  only raw syscalls in the child.
- **M17. Quadratic descendant-eviction scan** — `mempool.rs:205-220,242-257`.
  O(n²) CPU with `max_size` up to 10k. Fix: reverse parent→children index.

## Low

- **L1. `lowest_fee_rate_txid` tie-break contradicts its comment** —
  `mempool.rs:582-587`; equal rate+size evicts nondeterministically. Fix: add
  txid as the final key.
- **L2. Public `Mempool::add_transaction` trusts a fabricated fee** —
  `mempool.rs:91-94`, `block.rs:182-189`. Unreachable in production (all paths
  use `admit_with_chain_fee`) but a dangerous public API. Fix: `pub(crate)`.
- **L3. Startup lock order reversed** — `vtorrent-daemon/src/main.rs:164-165`
  locks mempool then chain, against the documented `chain → mempool` order.
  Safe today (runs before tasks spawn). Fix: acquire chain first.
- **L4. `assemble_block` hardcodes `bits`, under-estimates tx size** —
  `staking.rs:521,499-508`. A claim-heavy pending tx can push a self-built
  block over `MAX_BLOCK_SIZE`. Fix: use parent `bits` and `serialized_size()`.
- **L5. Legacy claim may claim less than the snapshot balance** —
  `consensus.rs:356` checks only `>`. The remainder becomes permanently
  unclaimable. Fix: require exact equality or document partial claims.
- **L6. Conflicting legacy claims can both enter the mempool** —
  `chain.rs:391-394` checks chain state only; claims have no inputs so
  `find_conflicts` misses them. Fix: track pending claim addresses.
- **L7. IPv4 multicast/CGNAT/reserved addresses accepted into PEX** —
  `pex.rs:107-127`. Fix: add `is_multicast`/`is_shared`/reserved rejection.
- **L8. Overlay session send-counter overflow panic** —
  `holepunch.rs:429` (`+= 1` with overflow-checks). Fix: `checked_add` + rekey.
- **L9. `TorTransport::new_circuit` bypasses the control-port timeout** —
  `vtorrent-onion/src/tor.rs:189,192`. Fix: wrap in `timed_io`.
- **L10. Connection-failure escalation can ban honest seeds** —
  `ban_manager.rs:220-258`. 5 transient failures → 5 min, escalating to 1 h.
  Fix: exempt configured seeds from escalating IP bans.
- **L11. `.onion`/`.i2p` dialing never reaches the Tor/I2P transport** —
  `peer_manager.rs:251-260`; OS DNS resolution fails first. Fix: detect
  anonymous addresses before resolution.
- **L12. Overlay `ingest` allocates/broadcasts before the rate-limit check** —
  `overlay.rs:269`. Fix: rate-limit at the top of the loop.
- **L13. `inv` handler uses `broadcast` instead of `broadcast_except`** —
  `node/handler.rs:199-201`; echoes back to the sender. Fix: use
  `broadcast_except`.
- **L14. `hex_decode` can panic on non-ASCII input** —
  `vtorrent-migrate/src/bdb.rs:323-338`. Fix: iterate `as_bytes()` in pairs.
- **L15. HD seed/mnemonic copies not zeroized** — `vtorrent-wallet/src/hd.rs:62-67`,
  `wallet.rs:383-391`. Fix: return `Zeroizing`.
- **L16. Recipient/change addresses not network-prefix validated** —
  `vtorrent-wallet/src/tx_builder.rs:112-116`; a Bitcoin mainnet address is
  accepted. Fix: call `validate_p2pkh`.
- **L17. WebSocket: unbounded subscribers, no auth, no idle timeout** —
  `ws.rs:141-146`, `server.rs:174`. Fix: connection cap, idle timeout, auth.
- **L18. SPV header chain grows without bound** — `handlers/mod.rs:250-300` →
  `vtorrent-spv/src/spv_chain.rs:471-488`. Fix: cap stored headers/bytes.
- **L19. IP rate limiter details** — `ratelimit.rs:35-47,61-77`. Loopback
  bypass means a loopback reverse proxy exempts all clients; O(distinct IPs)
  `retain` per request; top-level routes have no limiter. Fix: trusted-proxy
  parsing, bounded LRU, global limiter.
- **L20. Constant-time compare leaks key length** — `server.rs:20-29`.
  Acceptable; noted for completeness.

## Verified clean

- **Reorg bookkeeping** (`chain/chain_reorg.rs`): `reorganize_to` snapshots and
  restores `height_index`/`tx_index`/`utxo_set`/`claimed_addresses`/`journals`/
  `total_supply`; the `add_block` error path cleans up the inserted fork block;
  partial-journal rollback is correct.
- **Reward/supply arithmetic**: `compute_pos_reward` uses `u128` and caps age;
  `MAX_SUPPLY` enforced on mint and apply; production rejects PoW/coinbase.
- **Legacy claim signature**: recover-and-compare-address correctly requires
  the legacy key; no forgery path.
- **Merkle/UTXO root**: `compute_utxo_root_ordered` matches the sorted
  commitment; parity test exists.
- **New wallet crypto** (`encryption.rs`): Argon2id m=64 MB/t=3/p=4, 12-byte
  `OsRng` nonce per encryption, AEAD failures collapse to
  `IncorrectPassphrase`, no downgrade path.
- **Address/WIF validation**: strict length, SHA256d checksum, foreign testnet
  WIF rejected.
- **P2P codec/message** (`codec.rs`, `message.rs::decode_v2`): length-prefix
  validated before allocation, bounded bincode, checksum verified.
- **Handshake** (`peer.rs`): fixed deadline, pre-handshake allow-list, exact
  protocol version, self-connection nonce check.
- **STUN** (`stun.rs`): strict length/cookie/txid and source validation.
- **P2P crypto** (`crypto.rs`): domain-separated transcripts, per-direction
  nonces, constant-time MAC compare.
- **Torrent path traversal** (`engine_disk.rs:131-148`): `..`, absolute, `:`,
  separators all rejected.
- **Swap recovery** (`swap_recovery.rs:62-121`): symlink-safe, size-capped,
  identity/secret re-derived.
- **Faucet/debug endpoints**: regtest-gated, supply-capped, cooled-down.

## Suggested priority

1. **C1** — consensus fairness; 85.4% of legacy supply can mint every block.
2. **H1, H4** — remote panics; trivial fixes.
3. **H2, H3** — auth boundary; the documented guarantee does not hold.
4. **H5, H6, H7** — remote memory exhaustion and staking liveness.
5. **H8, H9, H10** — key material exposure in the migration tool.
6. Medium/low as capacity allows.

## Not fully verified

- `vtorrent-script` opcode semantics vs Bitcoin/PPCoin (consensus-critical if
  divergent).
- BIP-152 compact-block reconstruction equivalence (`compact.rs` +
  `handler.rs:494-763`).
- Cross-chain HTLC ordering in `vtorrent-btc` + `vtorrent-wallet-service` and
  the RPC swap orchestration (only the node-side builders were audited).
- Production deployment topology (whether a reverse proxy sits in front of
  RPC, which determines exploitability of H1–H3 and L19).
- Tauri v2 capability/permission enforcement (no capability files present).
