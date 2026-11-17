# AI supplementary security review — second pass, 2026-11-17

> **AI-generated supplementary review. It does not satisfy the independent
> human security-review gate.** No code was shared externally. This is defect
> finding and regression hunting, not sign-off.

**Reviewed tree:** `b03eae7` (after the first pass fixes), plus the fixes below.
**Method:** five parallel read-only passes — consensus/staking, P2P/overlay/SPV,
RPC/storage, torrent/file handling, and a regression review of the first-pass
fixes. Findings below were re-derived against the source; fixes were written
test-first and their focused tests were observed to fail before the fix.

A third pass is recorded in
`docs/2026-11-17_ai-supplementary-review-pass3.md`.

## Fixed in this pass

| Sev. | Finding | Status |
|---|---|---|
| High (fund drain) | Torrent incentive credit was granted for any verified, written piece, even when the peer had not requested it and the node already held it. A peer could replay one valid piece repeatedly, each time crediting its claimed `ut_vtr` address, and the daemon paid `owed` every settlement. | Fixed in `71b025f`: a piece is claimed for credit at most once, and a failed disk write releases the claim. Replay no longer credits. |
| High | Tracker-supplied peers in the main spawn loop bypassed `tracker_target_allowed` (the magnet/metadata loop applied it). A malicious tracker could make the client connect to loopback, private, link-local, or metadata addresses. | Fixed in `71b025f`: one `peer_target_allowed` policy is used by both loops; port 0 and non-public addresses are skipped. |
| Medium | `--wallet-passphrase-file` was checked only for being a regular file. A group/world-readable file plus keyless loopback RPC allowed a local user to auto-unlock and drain the wallet. | Fixed in `ee62976`: Unix mode must be owner-only (`0600`); regression test included. |
| Medium | Desktop `bump_vtr_refund` still authorized through the RPC hot-wallet helper, which cannot parse the desktop wallet, so VTR refund replacement always failed. | Fixed in `d76669f`: desktop authorizes its own wallet and calls `bump_with_wif`. |
| Low | Desktop swap commands synced the RPC wallet (setting a non-expiring unlocked WIF) before checking the passphrase, so a wrong credential still left the wallet unlocked for other commands. | Fixed in `d76669f`: authorization precedes `sync_swap_wallet`; order placement no longer populates the signing key. |
| Low | The HTTP `/swap/vtr-claim` route silently discarded `taker_wif`, producing a misleading auth error for existing callers. | Fixed in `d76669f`: a non-empty raw key is an explicit `BadRequest` unless the node is regtest. |
| Low | 2FA enrollment errors were rendered only inside the confirmation form, so a wrong passphrase on “Enable 2FA” looked like a no-op. | Fixed in `d76669f`: the idle state renders the error and Cancel clears the passphrase. |

Focused verification after these fixes: 110 torrent tests, 31 Tauri tests, 204
RPC tests, 4 daemon passphrase tests, 44 frontend tests, Cargo Clippy on the
changed crates, and frontend lint.

## Open findings (verified by code reading)

### Consensus and staking

1. **High — the stake kernel is evaluated at a timestamp the block producer
   chooses.** `consensus.rs` accepts any timestamp up to `now + 7200` and only
   requires it to exceed the parent; the kernel is computed at that timestamp
   and the modifier is a function of the parent. A staker can test the whole
   7200-slot window and keep its first winner, so a 0.1% staker can reliably
   beat a 10% staker. This contradicts the documented per-tick proportionality.
   Fix: derive the timestamp deterministically from the slot
   (`genesis + slot·60`) and bind `prev_block_hash` into the kernel preimage.
2. **High — the coinstake signature does not commit to the block header.**
   `Transaction::sighash` covers only transaction fields. A third party can take
   a broadcast coinstake and rebuild a header at a different timestamp that also
   satisfies the kernel. Because the next block must be strictly later and no
   more than two hours ahead, a header stamped at `now + 7200` stalls the chain
   for up to two hours. Fix: sign the header (or a header commitment) in the
   coinstake, and reject future timestamps.
3. **Medium — claim verification is a 59k-row linear scan performed before the
   signature check**, and claims are fee-exempt. A garbage claim burns CPU at
   roughly 10³× traffic amplification. Build an address index and verify the
   signature first.
4. **Medium — P2CS locktimes at or above `0x8000_0000` are permanently
   unspendable** by the cold key because the script engine decodes the CLTV
   operand as sign-magnitude while the parser reads a `u32`. Reject those values
   in the constructors/classifier.
5. **Medium — block-size accounting differs between producer and validator.**
   The producer sums transaction sizes; the validator measures the serialized
   block (header plus vector prefix). A nearly full mempool produces templates
   that always fail validation and staking retries forever.
6. **Medium (regtest only) — the PoW path has no difficulty check, no coinbase
   value bound, and skips the UTXO commitment.** Regtest deliberately reuses the
   mainnet magic, so a LAN-exposed regtest node can be poisoned. Gate PoW on an
   explicit chain id and validate the commitment for every block type.
7. **Low — fork tips are admitted without coinstake/reward/P2CS checks**, and a
   failed reorg removes only the newest block. Also, the supply counter ignores
   burns, so the reported supply drifts high (conservative for the cap).

### P2P, overlay, and SPV

8. **High — `inv` fan-out amplification.** One peer sends up to 1000 unknown
   hashes; the node broadcasts a matching `getdata` to every peer, with no
   outbound budget. Request only from the announcer, dedupe, and cap outbound
   requests.
9. **High — inbound sybils suppress outbound dialing (eclipse).** The outbound
   target is compared against a count that includes inbound peers; eight
   attacker connections stop all dialing. Count outbound separately, require
   distinct network groups, and bucket the address book by /16.
10. **High — compact-block handling serializes the whole mempool per message**
    before validation. Build the short-id map once per header/nonce and cap
    short-id count.
11. **High — DHT bootstrap can hang forever.** The inner receive loop never
    re-checks the deadline, so a node streaming wrong-`t` datagrams wedges
    bootstrap and leaks a thread/socket.
12. **Medium — DHT probes private address space** returned by hostile nodes;
    filter to global unicast and exclude our own address.
13. **Medium — overlay peer keys are derived from ~33 bits of a synthetic
    address**; a grinded key can collide and suppress a victim's overlay
    traffic. Key peers by full node id.
14. **Medium — an unauthenticated `PUNCH_CONFIRM` removes a pending handshake
    before address/MAC validation**, and the session table is evictable.
    Validate before removal and add a global rate limit.
15. **Medium — the SPV chain accepts an unanchored genesis** (`height == 0`
    skips linkage and no genesis hash is compared) and stores headers without
    bound. Pin the genesis/checkpoints, enforce retargeting, and cap retention.
16. **Medium — overlay peers are effectively unbannable**; bans use a synthetic
    address the overlay path never checks, and disconnect does not evict the
    session.
17. **Low/medium — filter-header “agreement” needs only two cheap sybils**, and
    `is_anon_addr` uses `contains(".onion")`, so hostnames like
    `x.onion.example.com` are misrouted to Tor. Require checkpoint agreement and
    parse onion addresses strictly.

### RPC and storage

18. **Medium — unauthenticated local callers can pin the chain lock for
    minutes.** Earnings scans up to 200,000 blocks (staking history 5,000) while
    holding `chain.lock()`; the daemon defaults to no API key and loopback is
    exempt from the IP limiter. Bound the scan, snapshot what is needed, and
    apply the limiter to protected routes.
19. **Medium — store truncation is committed before the rebuild is proven.**
    A replay error truncates above the last good height, clears derived state,
    then rebuilds; a failure mid-rebuild loses every block above the cut with no
    backup. Rebuild side-by-side or require explicit opt-in.
20. **Medium — a failed `append_block` is logged and dropped.** Store and
    memory diverge until a restart; only broadcast lag triggers reconciliation.
    Set a persistent store-lag flag and refuse staking/serving after repeated
    failures.
21. **Low/medium — swap refunds are unauthenticated when no VTR wallet exists.**
    A BTC-seed-only node can refund without a credential; funds go to the
    recorded address, so there is no theft, but the path is inconsistent with
    the VTR leg.
22. **Low/medium — WebSocket frames are capped only after deserialization**
    (64 MiB transport default, unbounded subscription vectors, 128 connections).
    Cap message size to 64 KiB and reject oversized arrays.
23. **Low — wallet metadata and staking intent use predictable temp names and
    follow symlinks**, and staking.json is written with default permissions.
    Reuse the wallet’s unique `create_new`/`0600`/fsync pattern.
24. **Low — cold-stake export accepts an arbitrary path**, returns it, and the
    in-memory script set is unbounded.
25. **Low — filesystem errors are re-wrapped as `BadRequest`**, leaking paths
    and OS details.
26. **Low — WIF fields in claim/bootstrap request models are not zeroized.**

### Torrent

27. **Medium — no connect/read timeouts and ineffective cancel.** A peer that
    completes the handshake and stalls blocks its task forever; the engine
    awaits all peer tasks, so it never finishes, and removal leaks the task.
28. **Medium — each upload request re-reads the entire piece.** 64 one-byte
    requests trigger 64 full-piece reads/allocations; `begin` is unaligned and
    unrestricted.
29. **Medium — metainfo paths are validated at write time, not parse time**,
    and duplicate/prefix/case-colliding paths are accepted; unwritable pieces
    are retried forever.
30. **Medium — the Tauri/RPC torrent commands create an arbitrary
    frontend-supplied directory** and base64-decode an unbounded metainfo.
31. **Low/medium — the tracker peer list is not capped at parse time**
    (up to ~700k entries from a 4 MiB body).
32. **Low — the outstanding-request counter leaks on the seed-ratio early
    return**, eventually refusing a peer permanently.
33. **Low — dust payouts ignore `MIN_PAYMENT_BYTES`**, and `settle` books
    `total_paid` even when payment construction fails.
34. **Low — a UDP tracker error reply bypasses the transaction-id check.**
35. **Low — DHT lookups have no global deadline.**
36. **Low — our wallet address is sent to every peer before any authentication.**

### Regression review of the first-pass fixes

37. **Low — `start_staking` still reads the RPC wallet WIF without a
    credential.** After this pass, authenticated commands no longer populate it
    with a wrong passphrase, but unauthenticated order placement/cancel calls
    can still sync the wallet. Require authorization to start staking or stop
    syncing the signing key from non-signing commands.
38. **Low — `disable_2fa` has no passphrase gate**, and desktop
    `verify_passphrase` is a plain byte comparison rather than the RPC gate's
    Argon2-backed check. The latter is a local timing oracle; the former is
    asymmetric with the newly authenticated enrollment.

## Launch implications

- The **torrent incentive payout path should stay disabled** until receipts or
  proof-of-possession are enforced. `71b025f` stops replay credit, but a peer can
  still send never-before-held valid pieces unsolicited and claim an
  unauthenticated address.
- The **consensus timestamp issues (1 and 2) are mainnet blockers**. They change
  block validity and must be settled before the genesis/fork parameters are
  frozen.
- The **store truncation path (19)** needs a backup or opt-in guard before
  production: it can silently discard the chain tail.
- P2P amplification and eclipse hardening (8–11) should be treated as launch
  hardening, not optional polish.

## Verdict

This pass found no new desktop fund-theft path after the first-pass fixes, but
it did find a fund-drain path in torrent incentives (contained), several
availability attacks, and two consensus-design defects. None of this substitutes
for the independent human review; items 1, 2, 8, 9, 15, and 19 should be in that
reviewer's scope explicitly.
