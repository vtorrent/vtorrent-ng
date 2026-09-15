# Codebase review — second pass (2026-09-15)

Addendum to `docs/code-review-2026-09-15.md`. The first pass covered the
consensus/chain, RPC/Tauri, wallet/migrate/crypto, and P2P/overlay/onion
crates. This pass covers the areas that review explicitly listed as
**not fully verified**, plus the remaining crates:

- `vtorrent-script` (opcode semantics vs Bitcoin/PPCoin — consensus-critical)
- `vtorrent-btc` + `vtorrent-wallet-service` + RPC swap orchestration
- `vtorrent-store`, `vtorrent-spv`, `vtorrent-torrent`, `vtorrent-snapshot`,
  `vtorrent-cli`, `vtorrent-tauri`

Read-only; no files modified, no fleet action. Every critical/high finding was
re-verified by the reviewer; the three panics and the SPV PoW bypass were
reproduced locally. Line numbers are exact at `main` = `781351e`.

## Critical

### S1. Legacy-claim signature is not bound to outputs — front-running theft
**CWE-345 / CWE-347** · `vtorrent-node/src/consensus.rs:337-381`, signature
verify at `:396-444`; reached from `chain.rs:387-395` and
`chain/chain_reorg.rs:276-297`

The claim signature signs **only the address string**
(`claim_message_hash(claim_addr)`, `consensus.rs:377`); the transaction's
outputs are not committed. `validate_legacy_claim` checks only
`tx.total_output() <= snapshot_balance` (`:354-361`).

**Trigger:** the owner broadcasts a valid claim for address `A`. The signature
`S` is now public in the mempool. An attacker copies `claim_address = A`,
`claim_signature = S`, replaces `outputs` with a P2PKH to themselves, and
re-broadcasts. It passes `validate_transaction` (claims need no inputs), passes
signature verification, passes the balance check, and `claimed_addresses` then
permanently locks out the real owner (`chain_reorg.rs:287-295`).

This is the most serious finding in either pass: it is a direct theft of any
unclaimed legacy balance (up to 11.59M VTR) by any mempool observer, with no
key compromise required.

**Fix:** bind the signature to a commitment over `(claim_address, recipient
script, amount)` — e.g. sign the txid with the signature field blanked, or a
domain-separated hash of the outputs. The address-only scheme cannot be safe
while outputs are attacker-chosen.

## High

### S2. `ut_metadata` fetch lets a remote peer pin ~64 GB
**CWE-770** · `vtorrent-torrent/src/engine.rs:872-903`,
`vtorrent-torrent/src/metadata.rs:59-80`

`metadata_size` is capped at 64 MiB, but that only bounds the *number* of
requests (`piece_count = size/16 KiB ≤ 4096`). Each `ut_metadata` data message
is a `PeerMessage::Extended` capped at `MAX_MESSAGE_LENGTH = 16 MiB`
(`peer_wire.rs:138`). `parse_data` validates neither `piece < piece_count` nor
the data length, and `pieces: HashMap<u32, Vec<u8>>` has no byte cap until
`reassemble_metadata` runs **after** the loop (`metadata.rs:154`). A hostile
peer answers each of up to 4096 requests with a distinct forged `piece` index
carrying 16 MiB → **4096 × 16 MiB ≈ 64 GiB** resident.

Reachable from `POST /api/v1/torrent/add` (magnet) and Tauri `add_torrent`.

**Fix:** accumulate a running byte total and abort past `metadata_size`; reject
`piece >= piece_count` and `data.len() > PIECE_LEN` in `parse_data`.

### S3. CLI treats HTTP 403 as success — false "Sent!" for money/staking/DEX
**CWE-391** · `vtorrent-cli/src/client.rs:102-110`

```rust
if !status.is_success() && status.as_u16() != 403 { return Err(...) }
Ok(resp_body)
```

The daemon returns **403** for `RpcError::WalletLocked` and `Forbidden`
(`vtorrent-rpc/src/error.rs:39,43`). A locked wallet therefore makes
`vtorrent-cli send` print `Sent! TXID: unknown` (`main.rs:366-368`), and
`dex buy/sell`, `staking start`, and swap calls report success when the server
rejected them.

**Fix:** remove the 403 special-case; surface the body's `message` on any
non-2xx.

### S4. Maker's BTC claim cannot be fee-bumped — reveal-then-lose
**CWE-362 / HTLC** · `vtorrent-btc/src/htlc.rs:226-243`,
`vtorrent-rpc/src/handlers/swap.rs:620-645`

The claim tx hardcodes `Sequence::MAX` and `lock_time: ZERO`, so it does not
signal RBF; there is no CPFP and no raw-claim retention (`SwapState` has
`btc_refund_raw`/`btc_funding_raw` but no `btc_claim_raw`). The taker's refund
uses a separate key and can carry an arbitrarily higher fee.

**Trigger:** the maker broadcasts the claim (preimage now public) at a low
feerate near `btc_expiry`; it stalls; at expiry the taker broadcasts a
higher-fee refund and wins. The taker now knows the preimage and calls
`vtr-claim`, receiving **both** the BTC refund and the VTR.

**Fix:** make the claim RBF-signalling (`Sequence::ENABLE_RBF_NO_LOCKTIME`),
implement CPFP/replacement, and persist the raw claim like the refund.

### S5. Cross-node taker flow is non-functional; preimage handoff missing
**CWE-1059** · `vtorrent-node/src/atomic_swap.rs:459-495`,
`node/mod.rs:447`, `handlers/swap.rs:391-397`

`OrderAnnouncement::from_order` drops `funding_txid`/`taker_address`/`preimage`
(all set to `None`), and `broadcast_order` has no production caller. A taker on
a second node therefore always sees `funding_txid == None`, and `vtr_claim`
requires it. No code extracts the preimage from the maker's BTC claim witness.
The two-party flow only works when both roles share one wallet/node.

**Fix:** gossip `funding_txid` and the taker address post-match, and extract the
preimage from the observed BTC claim (the SPV scan already downloads the
spending tx).

## Medium

- **S6. SPV PoW target check returns "always passes" for `exponent ≥ 33`** —
  `vtorrent-spv/src/spv_chain.rs:108-113`. `low_zeros + 3 > 32` returns `true`
  even when the real target is far below `2^256`. **Reproduced:**
  `hash_meets_target(&[0xff;32], 0x2100_0001) == true` while the real target is
  `2^240`. `add_header_inner` also never checks `header.bits == parent.bits` for
  untrusted headers, so difficulty is attacker-chosen. Reachable via
  `POST /api/v1/spv/headers`. Fix: only short-circuit when the out-of-range
  mantissa bytes are non-zero; otherwise bound `low_zeros` to 29.
- **S7. `decompress_amount` overflow panic on crafted chainstate** —
  `vtorrent-snapshot/src/leveldb_reader.rs:161-179`. Unbounded base-128 varint
  exponent; `for _ in 0..e { n *= 10 }` panics under `overflow-checks`.
  **Reproduced** with `x = 184_467_440_740`. Reachable when parsing a supplied
  `--chainstate` dir. Fix: `checked_mul`, reject `e > 9` and amounts above
  `MAX_SUPPLY`.
- **S8. `piece_length` multiplication overflow panic from a peer `Piece`
  index** — `vtorrent-torrent/src/engine_disk.rs:39-43`, reached at
  `engine.rs:737`. `index as u64 * metainfo.piece_length` is unchecked and
  `index` is never validated against `piece_count`; `piece_length` is never
  capped. Fix: `checked_mul`/`checked_add`, validate indices, cap
  `piece_length` and file lengths at parse time.
- **S9. DHT `get_peers` pending queue grows without bound** —
  `vtorrent-torrent/src/dht.rs:277-320`. `queried` is only populated on pop, so
  the same addresses are re-pushed by multiple responses; a hostile node
  returning 2500 fresh nodes per reply grows `pending` by ~2499 per 3 s round
  while `peers.len()` stays 0. Fix: dedupe and hard-cap at insert time.
- **S10. "Staking rewards" reports the staked principal as reward** —
  `vtorrent-rpc/src/handlers/staking.rs:177-182` (mirrored in Tauri). A
  coinstake is `outputs[0] = 0` marker, `outputs[1] = stake + reward`
  (`staking.rs:419-422`), so the sum overstates the reward by the full staked
  amount. The node's own `StakingReward { reward_sats }` computes the true
  value, so the two disagree. Fix: subtract the spent input value.
- **S11. No VTR input reservation across orders; failed admission leaves a
  phantom `VtrFunded`** — `handlers/swap.rs:136-143,191-219`. Funding picks the
  largest UTXO with no reservation, so two orders can select the same outpoint;
  the second tx is rejected but the swap is already persisted as `VtrFunded`
  and `release_funding` cannot clear it. Fix: reserve the outpoint and roll
  back on `submit_vtr` failure.
- **S12. Remote panic via unvalidated P2P order fields** —
  `handlers/dex.rs:158-164`; `node/handler.rs:1911-1915`. Gossiped orders are
  never validated, and `cancel_dex_order` byte-slices `maker_address` — the
  same class as first-pass H1 but sourced from unauthenticated gossip. Fix:
  validate gossiped fields on ingest and use char-safe truncation.
- **S13. BTC claim output can be dust for small "valid" orders** —
  `vtorrent-wallet-service/src/swap_policy.rs:66-68`. Policy only requires
  `target_amount > 1000`, so `1001..=1545` yields a claim output below the
  546-sat dust limit; the claim is policy-rejected and the maker can never take
  the preimage branch. Fix: require `target_amount > 1000 + 546`.
- **S14. Two incompatible snapshot binary formats; shipped reader rejects the
  shipped snapshot** — `vtorrent-snapshot/src/snapshot_reader.rs:12` (magic
  `VTR\x01`) vs the deployed `VTRS`/count-first layout in
  `vtorrent-tauri/utxo_snapshot.bin` and `vtorrent-node/src/genesis_snapshot.bin`.
  `snapshot_writer` emits the former, so its round-trip test passes, but the
  real artifacts use the latter and `parse_binary` errors on them. The balances
  themselves are sound (verified byte-equivalent). Fix: unify the format.

## Low

- **S15. Script engine divergences from Bitcoin** (consensus-relevant, not
  directly exploitable with a single implementation) — `vtorrent-script/src/`:
  disabled opcodes `OP_MUL`/`OP_DIV`/`OP_MOD` are implemented
  (`engine.rs:628-669`); no 520-byte push limit during execution
  (`engine.rs:133-139`); P2SH verifies over the scriptPubKey not the redeem
  script (`engine.rs:102-111`); alt stack is not reset between scriptSig and
  scriptPubKey (`engine.rs:84`); opcode budget is 200 and counts pushes
  (`engine.rs:34`); stray `OP_ELSE`/`OP_ENDIF` not rejected (`engine.rs:166-174`);
  `OP_RETURN` fails in unexecuted branches (`engine.rs:176-179`); CLTV/CSV
  semantic differences (`engine.rs:494-597`); arithmetic results capped at 4
  bytes (`engine.rs:24-32`); `Script::push_int` malformed for `n > 16`
  (`script.rs:81-98`, currently unreachable); HTLC expiry ≥ 2^31 encodes
  negative (`standard.rs:183`).
- **S16. `restore_btc_swap` only runs on the refund path** —
  `handlers/swap.rs:860-926`, called only at `:679`. Fix: call it in the
  BTC-leg handlers.
- **S17. Book eviction can drop funded orders** —
  `vtorrent-node/src/atomic_swap.rs:799-809`. Fix: never evict non-`Open`.
- **S18. `set_peer_bitfield` does O(message bytes) work** —
  `vtorrent-torrent/src/scheduler.rs:70-81`. Fix: bound by `piece_count`.
- **S19. Tauri `start_staking` marks enabled before confirming the command** —
  `vtorrent-tauri/src/commands/staking.rs:37-46`. Fix: propagate the send error.
- **S20. CLI injects unencoded user strings into RPC URLs** —
  `vtorrent-cli/src/main.rs:310,477,564`. Fix: percent-encode.
- **S21. Incentive account map can exceed its cap** —
  `vtorrent-torrent/src/session.rs:156-184`. Fix: hard cap with LRU.

## Verified clean (this pass)

- **`vtorrent-store`**: every mutation is a single redb transaction and redb
  3.1.3 defaults to `Durability::Immediate`, so no torn writes; reorg undo is
  applied tip-first with matching heights; UTXO key encoding is collision-free.
- **SPV stake-proof path**: fails closed by design; all slices bounds-checked;
  `hash_utxo`/`sighash`/`txid` bit-for-bit tested against the node.
- **Torrent metainfo parsing**: depth-guarded, negative ints rejected,
  `piece length == 0` rejected, `pieces` cross-checked, `sanitize_path` rejects
  `..`/absolute/`:`/separators.
- **Tracker / peer-wire framing**: 4 MiB response cap, txid and source
  validated, 16 MiB message cap, all payload slices length-checked.
- **HTLC script construction**: VTR and BTC claim/refund scripts are consistent
  (`OP_SIZE 32`, `OP_SHA256`, CLTV only on refund); witness construction and
  branch selectors correct; VTR funding script agrees across fund/claim/refund.
- **Expiry ordering**: `min(now+48h, vtr_expiry−6h)` with a ≥1 h floor, and the
  maker re-verifies the actual funded BTC script and asserts
  `btc_expiry + 6h ≤ order.expiry` before revealing.
- **BTC funding verification**: isolated scan, 6 confirmations, exact
  txid/vout/amount/script, rejects confirmed spends and immature coinbase,
  requires 2 distinct peer IPs off-regtest.
- **Recovery journal**: symlink-safe, size-capped, secret re-derived, refuses
  contract/lineage replacement, zeroizes preimages on drop.
- **RBF refund lineage**: preflight against a mempool copy, fee-rate bump
  enforced, append-only storage.
- **Script engine panics**: none reachable — the only `unwrap`s are guarded.

## Priority

1. **S1** — direct theft of unclaimed legacy balances; highest severity in the
   whole review.
2. **S2, S3** — remote memory exhaustion and false success reporting on money
   operations.
3. **S4, S5** — HTLC loss vector and a non-functional documented flow.
4. **S6, S7, S8** — remote/parse panics and an SPV PoW bypass.
5. Remaining medium/low.

## Still not fully verified

- Bitcoin Core's exact per-`EvalScript` altstack scoping (S15) — no Core source
  vendored to cite.
- BIP-158 filter semantics against a live mainnet node (fail-closed, not theft).
- redb durability under true power loss (directory fsync on first creation).
- Whether a reverse proxy sits in front of production RPC (affects S3/S12
  reachability).
