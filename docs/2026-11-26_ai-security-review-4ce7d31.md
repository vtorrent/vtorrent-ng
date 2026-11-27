# RC4 security review: hold publication

**Candidate:** `4ce7d315e3d5cb03c1f2c7c40a555cb87fe2e79e` (`v2.0.0-rc4`)
**Reviewer:** GPT-6.2 Astra, frontier AI review under the [RC4 review policy](release-candidate-rc4.md#review-policy-for-200)
**Date:** 2026-11-26

**Verdict: do not publish.** Two Critical defects allow a remote peer to stop any
node and let the bootstrap claimer mint about three years of rewards in seconds.
A third defect risks the taker's VTR leg in an atomic swap. Fixing them changes
the daemon binary, so this candidate's soak cannot carry forward.

**Independence:** this reviewer helped prepare RC4 and its release evidence. That
is disclosed in the release notes. This is not an independent human audit.

## Method

Reviewed the candidate source for the scope emphasis in
`docs/security-review-request-2026-10-27.md`: wallet cryptography and TOTP, RPC
authorization, consensus and checkpoints, BTC SPV and swap settlement, torrent
incentive accounting, and the desktop lifecycle. Findings below marked
**verified** were confirmed by reading the cited code in this candidate.
Findings marked **reported** come from a focused sub-review and were not
independently re-executed.

## Blocking findings

### C1. Unauthenticated handshake kills the daemon (verified)

`vtorrent-node/src/node/peering.rs:52` truncates the peer-supplied `user_agent`
with `String::truncate(128)`. Rust panics when the index is not a UTF-8
character boundary. A valid 129-byte user agent places a continuation byte at
byte 128 and triggers the panic. The panic ends the node task;
`vtorrent-daemon/src/main.rs` then observes `node_handle` completion, logs
"P2P node exited unexpectedly", and returns from `main`.

The peer task has the same defect at `vtorrent-p2p/src/peer.rs:164`
(`truncate(256)`), where the panic also skips the inbound-count decrement at
`vtorrent-p2p/src/peer_manager.rs:271-293`. Sixty-four crafted packets then
exhaust `MAX_INBOUND` until restart.

Any host that can reach the P2P port can do this. No key, wallet, or chain state
is needed; the attacker only has to send the public protocol version first.

**Fix:** truncate on a character boundary. The RPC crate already has
`truncate_chars`; move or duplicate a char-safe helper into the P2P and node
layers, and decrement the inbound counter on every peer-task exit path.

### C2. Block time is not anchored to launch time (verified)

`validate_block_inner` enforces only `timestamp > parent` and
`timestamp >= parent + MIN_BLOCK_SPACING`, plus the upper bound
`timestamp <= now + 5` (`vtorrent-node/src/consensus.rs:471-486`). There is no
lower bound against real time, and `GENESIS_TIMESTAMP` is `1_700_000_000`
(November 2023, `vtorrent-node/src/genesis.rs:24`), about three years before
launch.

Fork choice is cumulative work where each block adds exactly one
(`vtorrent-node/src/chain.rs:1256,1348`). At bootstrap the claimer is the whole
staked supply, so every kernel timestamp wins. A claimer can walk chain time
forward in six-day steps, stay under the wall-clock bound, satisfy stake age,
and produce about 182 coinstakes in seconds. Staking pays 5% per year on chain
time, so this captures about 15% of the claim in early rewards, and the honest
network adopts the longer chain. The 20,000,000 VTR cap still holds and no third
party's coins are taken; the loss is the intended reward schedule and an unfair
bootstrap.

**Fix:** add a deterministic launch-time floor, for example a consensus
`LAUNCH_TIME` constant set before launch and a rule that every block at height
1 or above has `timestamp >= LAUNCH_TIME - MAX_PAST_DRIFT`. Decide separately
whether to bound the forward jump per block. Because genesis and the snapshot
commitment are frozen, do not change `GENESIS_TIMESTAMP`.

### H1. BTC claim observation requires the minimal true selector (verified)

`vtorrent-btc/src/sync/swap_observation.rs:80-97` treats a spend as a claim only
when the witness contains an element exactly equal to `[1]`. A maker can claim
the BTC HTLC with a truthy selector such as `0x02` or `0x81`. That spend is
consensus-valid for P2WSH; minimal-IF is a relay policy rule, so a motivated
maker can get it mined.

The taker's scanner then records the HTLC as spent elsewhere, stops monitoring,
and never stores the preimage, so it never claims VTR. The maker refunds VTR at
expiry and keeps both legs. The VTR-side parser has the same fixed-selector
assumption at `vtorrent-rpc/src/swap_reconciliation.rs:82-87`.

**Fix:** identify a claim by the hash-lock match alone. Scan every witness
element for a 32-byte preimage whose SHA-256 equals the hash lock, and treat a
match as a claim regardless of selector. Keep the selector check only as a
tie-break or diagnostic. Add a regression with a `0x02` selector and one with
an extra element.

## High findings

- **H2. A stalled peer head-of-line blocks the node event loop (verified).**
  `send_to` awaits a one-second timeout inside the single node loop
  (`vtorrent-p2p/src/peer_manager.rs:861-877`). A peer that completes the
  handshake and stops reading fills its queue, then each send waits the full
  second. The same bug was fixed for `disconnect` but not here. Use `try_send`
  and drop on full, as `broadcast` already does.
- **H3. VTR claim retries ignore a conflict (reported).** The retry path
  re-submits the saved claim and ignores `Ok(false)` from `submit_vtr`, so a
  claim replaced by a competing refund is never fee-bumped and the taker can
  lose the VTR leg after paying BTC (`vtorrent-rpc/src/swap_recovery.rs:500-527`).
  Record the conflict and bump, or fail the swap loudly.
- **H4. VTR refund can be built over a pending taker claim (reported).** The
  refund path does not check the observation or mempool for a competing claim,
  and `submit_vtr` broadcasts even when local admission fails. A refund can win
  the race and also blocks later `btc_claim` paths. Require a clean observation
  and a successful local admission before broadcasting a refund.

## Medium findings

- Wallet: Tauri authorization compares the cached passphrase with `==` instead
  of re-deriving the KDF (`vtorrent-wallet/src/wallet.rs:702`). Use a
  constant-time compare or re-derive from the passphrase.
- Wallet: `disable_2fa` needs only a six-digit code, with no passphrase and no
  attempt limit (`vtorrent-tauri/src/commands/wallet.rs:751-771`). Require the
  passphrase and rate-limit failures.
- Wallet: 2FA is not part of the at-rest KDF, so a stolen file plus passphrase
  yields every key without a TOTP code. Document this clearly or wrap the key
  with a TOTP-derived factor.
- Legacy raw-WIF hot wallets silently lose 2FA after restart, because the
  older payload stored only the encrypted WIF. Warn on unlock or require
  re-enrollment.
- `torrent/add` can trigger incentive settlement that signs VTR with the hot
  key and no passphrase re-auth, unlike `wallet/send`. Require an
  authorization step for the spend path.
- Reorg application does not re-check `checkpoint_accepts` per block
  (`vtorrent-node/src/chain/chain_reorg.rs:961-1039`). Not reachable in the
  daemon today, but latent.
- The producer does not taper rewards as supply approaches the cap, so the
  network halts at 20,000,000 VTR instead of converging. Decide the desired
  behavior before the cap is near.
- `set_checkpoint` accepts an unverified future height and hash; a typo stalls
  the chain at that height. Require the hash to come from two independent
  sources or a signed file.

## Sound areas

- Every state-changing RPC route is under the API-key layer with authentication
  outermost; keyless operation is refused on mainnet and on non-loopback
  binds.
- The VTR HTLC script enforces `OP_SIZE 32; OP_SHA256; OP_EQUALVERIFY`, and the
  script runs at admission and at block application, so a taker cannot claim
  without the preimage and a maker cannot refund before expiry.
- The PoS kernel, stake age, coinstake reward ceiling, and the 20,000,000 VTR
  supply check are enforced during block application, not only by the producer.
- Wallet encryption uses Argon2id with 64 MiB, t=3, p=4, and a fresh
  ChaCha20-Poly1305 nonce and salt per save. No nonce reuse or KDF parameter
  problem was found.
- Incentive settlement credits only hash-verified downloaded bytes and is
  capped by the node's own measurement.

## Required before a new candidate

1. Fix C1, C2, H1, H2, H3, and H4 with regression tests. C2 needs a recorded
   consensus decision, not only a code change.
2. Build a new candidate, record its image and binary digests, and start a new
   168-hour window. RC4's window is evidence for RC4 only.
3. Re-run this review scope on the new SHA. Update this record and the review
   policy; the release notes must name the critical findings and their fixes.
4. Rehearse the launch-time floor on a fresh regtest chain: a bootstrap claim
   with an old timestamp must be rejected, and an honest claim must be
   accepted.

No blocking finding depends on an unverified test; C1 and C2 follow from the
code paths above. H1 depends on the maker getting a non-standard selector
mined, which relay policy discourages but consensus allows.

## Remediation preview

The blocking fixes are implemented in `2efd07c4727fb46be2aeb18f675c995c49cd38b2` on
`fix/rc5-security`, built from RC4.

| Finding | Change |
|---|---|
| C1 | Peer strings truncate on character boundaries; a drop guard releases the inbound slot on every peer-task exit, including a panic. |
| C2 | Production blocks must be dated at or after `LAUNCH_TIME - MAX_PAST_DRIFT`. `LAUNCH_TIME` is `1_795_651_200` (2026-11-26T00:00:00Z), set no later than launch; regtest and test fixtures bypass the floor through `allow_pow`. |
| H1 | The BTC observer identifies a claim by the hash-lock match alone. The VTR parser searches for the 32-byte preimage push instead of requiring `0x51`. |
| H2 | `send_to` uses `try_send`; a stalled peer no longer blocks the node event loop. |
| H3 | The VTR claim retry records the conflict when local admission fails. |
| H4 | A VTR refund is refused when the observation already shows a claim or when any mempool transaction spends the funding output. |

Evidence: workspace suite **1,558 passed, 0 failed**; `cargo fmt` and workspace
Clippy with `-D warnings` clean. Preview image
`sha256:ce23bdabc8d4f5019cd4ee6693e73c76fa2c0b2bba242712be0947b1ddad2608`,
daemon SHA-256
`f0c24865b548ac52236f7fff434b3c9c03241134096fa167b40bb4bf629e3d99`.

The C1, C2, H1, H2, and H3 fixes landed in `v2.0.0-rc5`; the strict refund fix
for H4 landed in `v2.0.0-rc7`. See `docs/release-candidate-rc7.md` for the image,
window, and desktop evidence. The RC4 window does not transfer.
Publication still requires the scoped re-review of the fix diff to pass and the
RC7 window to complete.

## H4 closure

The RC6 re-review found one remaining Medium: a refund that failed local
admission stayed recorded and blocked the maker's `btc_claim`. RC7 clears the
recorded refund and persists before returning the admission error. The scoped
re-review verified the rollback path and found no new Critical or High issue.
Remaining Low findings are recorded in the re-review and are accepted for this
release.
