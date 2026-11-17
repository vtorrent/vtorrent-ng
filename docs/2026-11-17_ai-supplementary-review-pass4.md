# AI supplementary security review — fourth pass, 2026-11-17

> **AI-generated supplementary review. It does not satisfy the independent
> human security-review gate.** No code was shared externally. This pass covers
> consensus/staking, wallet signing, torrents/incentives, P2P/SPV,
> RPC/daemon/store, and BTC/atomic swaps, plus a direct check of the frontend
> boundary.

**Reviewed tree:** `c7ebe44`, plus the fixes in `b02dd33`, `3d32e8c`, and
`6c2193b`.
**Method:** six parallel read-only reviews plus a direct frontend check. Every
fixed item was traced against the source and covered by a focused test; open
items are marked by verification level.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| Critical | Automatic VTR incentive payouts went to a self-reported `ut_vtr` address with no receipt, identity binding, or per-peer cap. Any torrent peer could farm VTR until the hot wallet emptied. | Disabled by default behind `--torrent-incentive-payments`; `ut_vtr` now requires a valid VTR address and a 128-byte payload cap. Receipts are still not enforced, so the flag is unsafe until that work lands. |
| High | Imported uncompressed legacy WIFs produced an address the signer could not spend: the wallet stored `hash160(uncompressed_pubkey)` but signing always rebuilt a compressed pubkey. | `TxBuilder`, `sign_custom_transaction`, `sign_input_over_subscript`, and P2CS spending now honor the WIF compression flag. Test covers an uncompressed import and a 65-byte pubkey in the scriptSig. |
| High | P2SH scriptSig was not required to be push-only. A scriptSig with `OP_DROP` before the redeem script executed successfully but returned zero counted sigops, bypassing `MAX_BLOCK_SIGOPS` and diverging from BIP16. | `Engine::execute` rejects non-push-only P2SH scriptSigs before executing the redeem script; tests cover the bypass and the push-only predicate. |
| High | Block templates budgeted only transaction bytes, omitting the 120-byte header and transaction-vector prefix. A full mempool could make every produced block exceed `MAX_BLOCK_SIZE` on validation. | `BLOCK_FRAMING_BYTES = 128` is deducted in `select_block_transactions` and `assemble_block`; a test pins it to the bincode block encoding. |
| High | The cold-stake spending-key export was never decrypted after writing. A passphrase typo locked the only copy while the wallet advertised the script as fundable. | `create_cold_stake` now decrypts the export, compares WIF and address, and deletes the file and refuses to record the script on any mismatch. |
| Medium | Staking-intent persistence wrote a predictable `staking.json.tmp` and followed a pre-existing symlink, allowing `wallet.json` to be truncated and replaced. | Replaced with `NamedTempFile::new_in` + `sync_all` + `persist` + directory fsync. |
| Medium | A failed RPC bind logged an error and exited 0, so the process manager treated a dead node as a clean shutdown. | The RPC task now exits non-zero on server failure, allowing `Restart=on-failure`. |
| Medium | Expensive read routes (block/mempool/tx/SPV proof) bypassed the IP rate limiter and deserialize full objects per request. | Those routes now carry the same per-IP limiter as the public write routes. |
| Medium | VTR HTLCs with an expiry at or above `0x8000_0000` (2038-01-19) produce a negative CLTV number and an unrefundable maker output. | Construction rejects expiry above `MAX_SCRIPT_LOCKTIME`; test covers `with_expiry`. Full post-2038 support still requires a script-number change. |
| Medium | Torrent `piece length` was unbounded and `piece count` could reach `u32::MAX`, forcing whole-piece allocations and huge bitfields. | `MAX_PIECE_LENGTH` is 16 MiB, `MAX_PIECE_COUNT` is 1,000,000, and multi-file totals use checked addition. Oversized-piece test added. |
| Low | Nested wallet types (`WalletKeyEntry`, `ColdStakeKeys`, `OtpConfig`, `HdAccount`) still accepted unknown fields. | `deny_unknown_fields` added so future fields fail to load instead of being silently stripped on re-save. |
| Medium | A stale buffered `NewBlock` could regress the store tip and leave the height index inconsistent, bricking startup. | `append_block` now accepts only the next height (or an identical tip append); rebuild/replay resets metadata before reappending. Store tests cover divergence healing, tail reconciliation, and missing/extra UTXO repair. |
| Medium | `verify_backup` consumed the TOTP step, so Verify followed by Restore within 30 seconds failed as a replay. | Added `Wallet::load_verify_only`; backup preview verifies the code without consuming it, while restore still consumes the step. |
| Medium | `Wallet::load` treated a failed TOTP re-save as fatal after the step was consumed, leaving the wallet unopenable on a read-only disk. | The wallet is returned with a warning; the account-scoped replay sidecar still enforces one-time use. |
| Medium | Unsolicited torrent `Piece` messages were accepted and credited without a matching request. | The engine now drops a block unless its `(piece, begin)` is leased; a scheduler test covers requested versus unsolicited offsets. |
| High | SPV accepted any height-0 header and verified inclusion proofs against side-fork headers. | The chain pins the first genesis hash and rejects conflicting height-0 headers; inclusion now requires the header to be an ancestor of the best tip. Tests cover both. Difficulty-schedule validation remains open. |
| High | Peer dials ran sequentially inside the node event loop with 30-second transport timeouts, so a poisoned PEX list could stall block/stake processing for minutes. | Each dial is bounded to 3 seconds and at most four are attempted per tick. Fully asynchronous dialing remains a follow-up. |
| Medium | Torrent `recv` had no deadline and accepted repeated bitfields up to 16 MiB, each costing a full per-bit walk. | A message must complete within 180 seconds; only the first bitfield is accepted and it may not exceed `ceil(piece_count/8)` bytes. |
| Low | `/ws` skipped its origin allow-list when the header was absent, and the Tauri seed probe could scan loopback/LAN services. | `/ws` now requires an allowed Origin. The probe rejects loopback, private, link-local, multicast, and unspecified targets; its test asserts loopback filtering. |
| Medium | Two concurrent matches could select the same confirmed VTR outpoint because coin selection ignored the mempool. | Funding selection and admission are serialized by a dedicated lock, and candidates spent by a mempool transaction are filtered out. The order is still marked `Matched` before admission, so a later non-conflict admission failure remains possible. |

## Open findings

### Consensus and staking

1. **Critical — stake timestamp grinding remains.** `check_stake_kernel_v2`
   hashes a producer-chosen timestamp (consensus.rs:291-308); validation allows
   any value in `(parent, now + 7200)` (consensus.rs:431, 439). A staker can try
   every legal timestamp and multiply its per-height win probability by up to
   7,200. A deterministic slot (parent timestamp + target block time) or median
   time-past is required. This is a consensus change and belongs in the human
   review.
2. **High — CLTV/CSV use the producer-controlled block timestamp** rather than
   parent MTP; refunds can open up to two hours early. Same root cause as (1).
3. **Medium — `total_staked` includes stake that cannot win** (aged out beyond
   six days or P2CS outputs whose staking key is lost), so the aggregate block
   rate decays as honest coins age.

### P2P, overlay, and SPV

4. **High — unauthenticated overlay punches can fill all 125 peer slots** for
   the one-hour session TTL, isolating the node.
5. **Partially fixed — peer dials are now bounded** to 3 seconds and four per
   tick; fully asynchronous dialing is still needed to remove the event-loop
   stall.
6. **Partially fixed — SPV genesis and best-chain ancestry are enforced.**
   Difficulty-schedule and checkpoint validation remain open.
7. **Medium — bans are enforced at accept/handshake, not dispatch**, and flood
   budgets are per socket, so one IP with many connections keeps its budget.
8. **Medium — control responses are unbounded** (`getheaders`/`getaddr`), and
   `inv` expands to `getdata` for every peer.

### Torrent protocol

9. **Partially fixed — unsolicited pieces, repeated bitfields, and idle
   receives are handled.** Per-peer cancel ownership remains open.
10. **Medium — a 16 MiB bitfield costs ~1 s of CPU per message**, and `Cancel`
    clears other peers' leases.

### BTC and swaps

11. **Partially fixed — funding selection is serialized and mempool-aware**,
    but a non-conflict admission failure after `Matched` can still wedge the
    order.
12. **Medium — the bounded 1,008-block scan is treated as proof of no BTC
    funding**, and a confirmed funding tx below the window can cause the refund
    record to be deleted.
13. **Medium — no automatic VTR claim after the maker's BTC claim.** An idle
    taker can lose the BTC leg.

### RPC, daemon, and wallet

14. **Fixed — store append monotonicity.** See the fixed table.
15. **Medium — backups containing BTC state cannot be restored**; the restore
    path refuses both the existing-state and fresh-state branches by design but
    with the same terminal error.
16. **Fixed — backup verification no longer consumes the TOTP step.**
17. **Fixed — a failed TOTP re-save is non-fatal.**
18. **Fixed — `/ws` requires an allowed Origin, and the seed probe rejects
    non-public targets.** See the fixed table.

## Verification

Focused verification: 97 script tests, 247 node tests, 115 torrent tests, 73
wallet tests, 203 RPC tests, 33 Tauri tests, 10 wallet-service tests, 16 store
tests, 44 SPV tests, and `cargo fmt`/Clippy on the changed crates. The full
workspace suite passed **1,435 tests, 0 failures, 11 ignored**.

## Verdict

This pass found two fund-safety defects in the new incentive and legacy-key
paths, a P2SH consensus-divergence bypass, and a template liveness bug; all are
fixed with tests. The consensus timestamp/kernel issue (1) and the P2P/SPV
findings (4-6) remain the highest-risk open items and require independent
review before launch. None of this is independent review.
