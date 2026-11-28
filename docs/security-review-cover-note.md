# Security review — cover note

A ready-to-send message for the reviewer. The technical scope lives in
`docs/security-review-request-2026-10-27.md`; this note is the ask, the context,
and the specific questions.

---

## Message

Subject: Independent security review — vTorrent-NG mainnet candidate `b4c5c60`

Hello,

I'm asking for an independent human security review of vTorrent-NG before we
open the network with atomic swaps enabled. An internal model review has already
been done and does **not** count for this gate — we need someone who did not
write or previously review this code to look at it with fresh eyes.

**Repository:** https://github.com/vtorrent/vtorrent-ng
Review `b4c5c60218db5992098b9306bfa62be49c60a410` (`v2.0.0-rc14`), not `main`. See `docs/release-candidate-rc14.md` for the exact image, binary, soak and desktop evidence. Since `be78829` the candidate gained the RC5-RC14 fixes; the non-doc delta is 30 files covering consensus (launch-time floor), P2P (bounded sends, inbound slot guard, safe truncation), swap refund gates and stale-refund clear, PUSHDATA preimage parsing, torrent path validation, wallet-only backups, and desktop 2FA/restore/version fixes. Review `git diff be78829..v2.0.0-rc14 -- . ':(exclude)docs/**'` and the RC14 record. The exact daemon image is byte-identical to the soaked RC10 daemon.

**Scope:** `docs/security-review-request-2026-10-27.md` on the documentation branch (`main`).

That document lists the exact hashes, the verification commands, and what we
consider in scope. The Rust suite needs only a clone and a Rust toolchain; desktop builds need
the GTK/WebKit prerequisites listed in the scope document, and the ignored
real-Bitcoin tests need a disposable Bitcoin Core regtest container. No
credentials or live infrastructure access are needed.

**What changed since the last review round.** The earlier review covered up to
`dd98176`, and an earlier package covered `b729e36`. Since the previous
candidate `ba46fd6`:

- `91dd05b` — BTC state initialization is recorded separately from the state
  file, symlinked and hard-linked state paths are rejected, and a
  separate-process lock test covers a state with a pending send.
- `30f7224` — desktop backup takes the BTC state lock, includes the
  initialization record, and refuses to restore BTC, swap, or cold-stake
  recovery data automatically: a backup cannot prove those records are current.
  Wallet-only bundles remain restorable.
- `1008166` — backup takes the BTC lock before checking whether state exists,
  closing an initialization race, and the cross-process BTC writer test now
  runs in its own test binary instead of racing other unit tests.
- `6c833cf` — desktop node startup holds the node mutex through initialization,
  and wallet create/open/import/restore hold the stopped-node guard for the
  whole operation. Wallet locking takes the node guard before clearing wallet
  state.
- `060bedf` — desktop cold-stake funding now requires the wallet passphrase and
  a fresh TOTP code when 2FA is enabled; the UI collects and forwards both.
- `611d56b` — desktop send quotes are bound to the address that signed them; a
  quote prepared by one wallet cannot be broadcast with another wallet's
  passphrase, and wallet switches or restores clear pending quotes.
- `a202519` — desktop DEX matching, BTC swap funding/claiming, refunds, BTC
  sends, and rebroadcasts now authorize against the desktop wallet itself;
  maker orders must belong to the open wallet, and the HTTP claim route no
  longer accepts a caller-supplied signing key.
- `713fcc5` — an empty TOTP replay-state file left by a crash is treated as
  uninitialized instead of permanently rejecting every verification.
- `aace0e5` — BTC sends reject sub-dust recipients and absorb sub-dust change
  into the fee, so they cannot build an unrelayable transaction while holding
  the inputs; desktop 2FA enrollment now requires the wallet passphrase.
- `71b025f` — torrent incentive credit is granted for a piece at most once
  (replay no longer pays), and every tracker-supplied peer passes the same
  SSRF address filter as magnet metadata peers.
- `ee62976` — `--wallet-passphrase-file` must be owner-only (`0600`).
- `d76669f` — desktop refund replacement authorizes against its own wallet;
  swap commands authorize before populating the RPC signing key; the HTTP claim
  route rejects a raw key outside regtest; 2FA enrollment errors are visible.
- `b9b05e3` — test-only: the tracker address policy is passed explicitly so a
  parallel test override cannot race the new peer-filter test.
- `8f7fe14` — release workflow actions are SHA-pinned, top-level token is
  read-only, the release build is `--locked`, and the release notes no longer
  claim unsigned builds are signed. Seed provisioning verifies a published
  daemon digest and creates an owner-only RPC key; the alert topic moved to
  `VTR_NTFY_TOPIC`.
- `49d56d2` — multisig is capped at 20 keys; wallet saves use unique temps and
  clean up on failure; cold-stake WIFs are redacted from `Debug`; the desktop
  refund-fee form sends the passphrase and 2FA code.
- `aacf371` — legacy BDB page offsets corrected and dump parsing made
  section-aware; desktop import capped at 32 MiB.
- `20e3fe6` — BDB overflow values reconstructed and covered by real fixtures;
  TOTP replay state replaced atomically; torrent pieces publish only after a
  successful write; seed key ownership and a container test; multisig boundary
  tests.
- `089e7c1` — the legacy 20,000 block sigop limit is enforced during validation,
  transaction application (including same-block P2SH outputs), mempool
  admission, and template selection; oversized output scripts still count.
- `9f3d4e9` — cold-stake spending-key exports are in encrypted backups; future
  wallet formats and unknown fields are rejected; snapshots validate counts,
  totals, hashes, and duplicates; BTC staging requires all confirmations and a
  provenance sidecar; mainnet requires an RPC key and release CORS is
  Tauri-only; CSP is narrowed; `scripts/wallet-import-check.sh` compares real
  wallets with `db5.3_dump`.
- `f0013bb` — clearer wallet decryption errors, worst-case fee estimates,
  removal of the single-key signing fallback, honest 2FA migration messaging,
  TOTP secret clearing, correct HTLC classification and `push_int` encoding,
  fail-closed benchmark and backup procedures, and the dead-man path fix.
- `b02dd33` — automatic incentive payouts are off until receipts land; legacy
  uncompressed keys sign correctly; P2SH is push-only; block templates reserve
  framing bytes; cold-stake exports are round-tripped; staking intent is written
  atomically; RPC bind failure is fatal; expensive queries are rate-limited;
  post-2038 HTLCs are rejected; torrent piece sizes are capped.
- `3d32e8c` — the store rejects non-contiguous appends and resets metadata
  during rebuild; backup previews no longer consume the TOTP step; a failed 2FA
  re-save is non-fatal; unsolicited torrent pieces are rejected.
- `6c2193b` — SPV pins its genesis and requires best-chain ancestry for
  inclusion; peer dials are bounded; torrent receives have a deadline and
  bitfields are capped; `/ws` requires an allowed Origin; seed probes reject
  non-public targets; VTR funding selection is serialized and mempool-aware.
- `f897f5c` — torrent block leases are owned per peer; a partial BTC scan
  releases the input but keeps the refund contract; overlay sessions are capped
  per source IP.
- `db0902c` — testnet RPC and monitoring bind to loopback; Grafana requires a
  password and disables anonymous access; bitcoind RPC is scoped to the Docker
  network; the release artifact requires an explicit runtime image.
- `a514d5b` — future block drift is capped at five seconds, bounding stake
  timestamp grinding and CLTV drift.
- `99b02fc` — the kernel denominator now counts only stake inside the age
  window, preventing block-rate decay from aged-out coins.
- `8fed10c` — a failed funding admission reverts the order instead of leaving
  it stuck; the taker's VTR claim is submitted automatically after the observed
  BTC claim.
- `eddd565` — SPV pins the VTR genesis and fixed consensus difficulty, and
  requires best-chain ancestry for inclusion proofs.
- `4d32ab2` — signed bandwidth receipts gate incentive payouts; missing,
  disputed, or replayed receipts pay nothing.
- `5011acf` — peer dials run in parallel batches with per-attempt timeouts.
- `fce6495` — untrusted PEX dials run in detached tasks and are registered
  from the node event loop.
- `733e476` — release text is honest about unsigned builds, and a published
  release gets a generated and verified `SHA256SUMS` asset.
- `c874dac` — fifth-pass fixes: 55-second production block spacing, replay
  independent of clock steps, unconditional SPV genesis pin and reorg
  following, fully detached bootstrap dials, PEX failures no longer banning
  honest IPs, receipt-gated and retryable incentive settlement, bounded
  receipt verification, swap lock/refund fixes, BTC reservation release,
  cold-stake key handling, multi-key sends, and CI/ops hardening.
- `5cdd580` — follow-up closures: cumulative receipt accounting with epoch
  resets, account-wide TOTP replay checks in verify-only loads, append-only
  cold-stake registration, a source-built container image, a hardened
  non-root testnet overlay, frontend dependency auditing, a BTC HTLC fee
  override, and the documented external-taker claim path.
- `2a25e3f` — taker match import with local swap persistence and a desktop
  form; `--checkpoint HEIGHT:HASH` enforcement plus the launch checkpoint
  procedure; minimal P2CS/HTLC CLTV encoding with legacy-shape parsing.
- `1e42381` — optional minisign signing over `SHA256SUMS`, a release
  verification script, key-custody documentation, and import/checkpoint guard
  tightening.
- `29892b9` — canonical and two-phase match import, funded taker address
  echo, separate receipt signing/peer epochs with cumulative engine totals,
  persisted checkpoints, and strict release signature verification.
- `514fe9f` — post-persist import recheck, receipt domain v3 with explicit
  peer epochs, genesis-bound size-capped checkpoints, release checksum
  self-hash fix, unlisted-asset rejection, key/checkpoint tooling, and
  per-node image overrides.
- `9533a2c` — non-blocking BTC status/address reads while SPV sync holds the
  wallet lock; release-fleet BTC attestation and code-commit evidence.
- `dd7676f` — equal-work fork tie-break on the lower tip hash and SPV header
  rebuild from the stored chain on restart; the first candidate window was
  discarded after reproducing the split.
- `04b3c4a` — independent-context AI review remediation: SPV follows the same
  equal-work rule and rebuilds after lost events, equal-height peers reconcile
  tips, future checkpoints no longer ban peers, the desktop starts both swap
  reconcilers, automatic VTR claims retry until expiry, receipts settle across
  epochs, incentive payments stay owed until six confirmations and are capped,
  short downloads send a final receipt, and trusted historical SPV headers
  survive a clock rollback.
- `7a216ee` — second adversarial pass: removed the fork relay that amplified
  stateless forks into a ban vector, bounded pre-checkpoint reorgs to a
  recoverable distance, closed the SPV install race and served headers from the
  index, required fresh or final BTC evidence for auto-claims, persisted
  preimages earlier, accepted non-standard true-branch witnesses, protected
  incentive debt from eviction/removal, kept missing payments pending instead
  of double-paying, and paced the final receipt past the receiver rate limit.
- `c27daf2` — automated VTR claim fee replacement after a local conflict
  (bounded to three bumps), chunked SPV header snapshots with a chain-generation
  check, and pruning of SPV side-branch headers.
- `d790c16` — persistent incentive-debt snapshot and ledger across graceful
  restarts, including a persistent process budget.
- `f8ccbfe` — fsynced receipt-backed incentive-account journal; the full
  account survives SIGKILL and is settled and paid at startup. Bytes after the
  last peer receipt remain unverified and unrecoverable.
- `1f5fa81` — journal replay preserves the ledger's pending/in-flight/remaining
  debt, writes live accounts under their own epoch, compacts the triggering
  record, rejects stale writes by version, and reserves the budget before emit.
- `2b65c3a` — receipt-backed accounts are retained during removal/eviction, a
  failed journal write poisons the journal instead of leaving a corrupt middle
  line, and journal creation/compaction is directory-fsynced.
- `66bc705` — failed durable writes roll back in-flight reservations and the
  budget; payment results drain every five seconds; settled ledger debts are
  pruned once no journal record remains.
- `9594df4` — the signed payment transaction and txid are persisted before
  broadcast; startup confirms, rebroadcasts, or safely releases each
  reservation.
- `10c4671` — a pending recovered payment is no longer re-emitted every tick;
- `ef1d1a1` — pass 8. A block is marked received only after the assembler
  accepts it; owned pieces have a progress deadline with reclaim and cooldown;
  completion is checked at the top of the peer loop; a corrupt peer is dropped
  after three failures. BEP-10 ids are used per direction, receipts are
  throttled per peer address, inbound frames are capped, zero-value receipts
  are ignored, a per-IP connection cap is enforced, and a complete session
  advertises all pieces. Payments prefer non-staking coins, refuse to spend
  every available staking-eligible coin, write off dust, evict by the current
  settlement window, and the RPC validates and trims the wallet address. The
  desktop remove guard for a daemon-only settlement loop is removed. The
  manual E2Es now require the candidate binary and digest.
- `22056d4` — pass 9. An empty web/RPC wallet address falls back to the
  unlocked hot wallet's change address; each peer receive is sliced to 10 s
  and a 15-minute no-verified-progress watchdog stops a stalled download. The
  seeder throttles receipts per connection and per address, keeps Cancel-proof
  tombstones, trusts verified resume pieces and bounds other verification,
  prunes evicted registry entries, and aggregates IPv6 per /64. Payments
  preserve the largest stakeable outpoint, treat dust/zero/disputed receipts
  as evictable, and remove the periodic dust write-off. The scheduler scopes
  piece clearing to its owner, the engine drops a stale assembler after losing
  ownership, and the magnet/tracker-init paths end `Stopped`. The pass-9 live
  transfer paid 585,937 sats; a manual session DELETE reproduced the
  unsettled-account removal residual.
- `d376506` — pass 10. The daemon installs a ledger-aware torrent-removal
  guard: it reads the durable incentive ledger and refuses removal while the
  same info hash has an unpaid, pending, or in-flight debt at or above
  `MIN_PAYMENT_SATS`; dust stays removable. The desktop path refuses a payable
  receipt instead of discarding it. The live DELETE was refused 164 times
  while a 585,937-sat debt was outstanding and succeeded after settlement.
  The live transfer was single-seeder because node1's wallet was locked and
  the empty-address fallback returned a raw validation error; a clear error
  and a desktop add-torrent E2E are follow-ups. The pass-8 split remains the
  live multi-peer proof.
- `e091936` — pass 11. The release lock is updated for
  **RUSTSEC-2026-0594** (`cookie 0.18.3`) and **RUSTSEC-2026-0592**
  (`url 2.5.9`); `form_urlencoded` follows at 1.2.3. `cargo audit` exits 0
  with the five allowed warnings. The exact-image live DELETE was refused 184
  times while the 585,937-sat debt was outstanding and succeeded after
  settlement. The live transfer was again single-seeder because node1's
  locked wallet returns the raw empty-address error; the pass-8 split remains
  the multi-peer proof. Correction: the payment paid the **1,000-sat fee
  floor**, not zero, and spent the 200 VTR stake output `85a4…:1`, resetting
  its age. The ledger-backstop case and desktop settle/write-off path remain
  follow-ups.
- Pass 13 — the current remediation. The structural stake reserve (automatic
  and manual paths), live-journal removal guard, pending-debt eviction input,
  canonical torrent block geometry, payout-address binding, legacy-import BTC
  state/default-key binding, seeder IPv4-mapped keys, prune race, partial-piece
  watchdog, resume-bit distrust, sub-minimum staking status, and a deterministic
  two-peer split regression. The `review-e091936` window is invalidated by these
  code changes and is not release evidence.
- Pass 14 — the current remediation. The review found a manual-send reserve
  bypass when a structural outpoint is mempool-spent, a quote-time reserve
  bypass, a locked/imported torrent payout key gap, a desktop staking key
  mismatch, a multi-master legacy import drop, a permanent un-receipted-debt
  pin, a desktop lock/unlock lockout, and pre-pass-13 import startup failures.
  The fixes are in `a0724dd`: reserve intersection and quote revalidation,
  unlocked-WIF torrent add, default-key staking, all-master extraction,
  bounded debt grace, same-wallet lock/unlock, and surfaced node-start errors.
  The `review-80f9eef` window is invalidated by these changes.
- Pass 15 — the current candidate. Adds an attested `repair_btc_state` for
  pre-pass-13 imported wallets (passphrase/TOTP plus explicit no-prior-history
  attestation; refuses overwrite/legacy state) and an authenticated
  `force_remove_torrent` at `/torrent/:id/force` (session removal with the
  ledger debt retained). The desktop legacy BTC guard now covers the old
  network-only state names. The `review-a0724dd` window is invalidated.
- `17db693` — pass 7. Each partially downloaded piece is assembled by exactly
  one owning task, blocks are leased only to peers that advertised the piece,
  and received/expired leases no longer wedge the pipeline. The seeder
  rate-limits receipts, binds them to the torrent's info-hash, caps inbound
  frames, serves only requested byte ranges with a real duplicate window,
  cancels open connections on removal, registers post-engine magnet metadata,
  and announces promptly with a random peer id. The payment builder refuses to
  spend every available staking-eligible coin, and the staking loop re-checks
  the selected outpoint under chain+mempool locks; disputed receipts are
  cleared. The RPC lifecycle guard is in place; the desktop remove path no
  longer applies that daemon-only guard (the desktop runs no settlement loop).
- `d5e80e1` — the staking loop skips outpoints a mempool transaction already
  spends, so a node cannot invalidate its own incentive payment with the next
  coinstake; adds a unit test for the filter.
- `4b34b9e` — desktop torrent settings splits the editable request from the
  status response carrying `seeding`/`seeded_torrents` (UI only).
- `04115cb` — the engine records a peer's bitfield and `Have` messages seen
  while scanning for the BEP-10 extension handshake. A real peer announces its
  pieces immediately after the wire handshake; dropping that message left the
  scheduler with no peer availability and it requested nothing. The incentive
  E2E now announces bitfield-first.
- `3dd99a0` — testnet fleets accept private torrent tracker/peer targets under
  the same `--testnet` flag P2P uses, and the fleet script can open the seed
  listener on every node.
- `3e8b2cb` — inbound torrent serving. `--torrent-listen` opens a seed
  listener; only held info-hashes are answered and each piece is re-hashed
  before its first block, with connection, request, limiter, and seed-ratio
  bounds. The seeder signs a cumulative receipt per connection, records the
  peer's receipt in the same incentive account and journal, and refuses peers
  without BEP-10. The RPC registers completed torrents and stops serving a
  removed session; the desktop path is wired but the desktop does not start a
  listener. `GET /api/v1/torrent/settings` reports seeder status.
- `2fdd9aa` — pass 6. Torrent peer-wire frames are buffered across coalesced
  reads with bounded per-connection sizes and timeouts; lease keys and block
  lengths are validated before any credit; tracker, magnet, DHT, assembler, and
  retained-session resources are bounded. The legacy `incentive-live.json`
  import fails closed, journal compaction no longer rewrites on every mutation
  past 4,096 live records, a stale journal write is reported as not durable, a
  failed budget append rolls back, and an epoch change settles at `min(now,
  window_end)`. UTXO selection excludes mempool spends in incentive payments,
  ordinary sends, cold-stake funding, and the desktop paths; the regtest faucet
  moved to the API-key-protected router.
  the live recovery drill caught the regression.

Five internal AI-assisted supplementary reviews are recorded in
`docs/2026-11-17_ai-supplementary-review.md`,
`docs/2026-11-17_ai-supplementary-review-pass2.md`,
`docs/2026-11-17_ai-supplementary-review-pass3.md`,
`docs/2026-11-17_ai-supplementary-review-pass4.md`, and
`docs/2026-11-18_ai-supplementary-review-pass5.md`. They found and fixed the
desktop authorization defects above, the torrent credit replay, the multisig
DoS cap, release/seed hardening, the recovery fixes in `20e3fe6`, the block
sigop limit in `089e7c1`, and the remaining wallet, snapshot, staging, RPC, and
CSP fixes in `9f3d4e9`, the wallet, script, and operations batch in `f0013bb`,
the fourth-pass fixes in `b02dd33`, the store/backup/torrent hardening in
`3d32e8c`, the SPV/P2P/WS/funding hardening in `6c2193b`, the torrent/BTC/
overlay fixes in `f897f5c`, the container hardening in `db0902c`, the
timestamp-drift bound in `a514d5b`, the eligible-stake denominator in
`99b02fc`, the swap rollback/auto-claim in `8fed10c`, the SPV genesis pin in
`eddd565`, the receipt-gated incentives in `4d32ab2`, and the parallel peer
dials in `5011acf`, the detached untrusted dials in `fce6495`, the release
checksums in `733e476`, the fifth-pass fixes in `c874dac`, the follow-up
closures in `5cdd580`, the taker-import/checkpoint/CLTV work in `2a25e3f`, the
release-signing wiring in `1e42381`, the import/receipt/checkpoint fixes
in `29892b9`, the edge-case hardening in `514fe9f`, the BTC liveness/soak
  tooling in `9533a2c`, the fork/SPV fixes in `dd7676f`, and the review
  remediation in `10c4671`. They are not
independent review and do not count for this gate.

Earlier in this round, from `b729e36` through `ba46fd6`:

- `ff148fd` — a codebase-wide remediation pass (consensus, overlay, P2P, RPC,
  swap, torrent, UI). Findings and fixes are written up in
  `docs/2026-10-29_codebase-wide-review.md`; we want those fixes verified rather
  than re-derived.
- `35ea52e` — TOTP replay protection (accepted step bound to an encrypted wallet
  field plus a locked host-scoped file), torrent adoption and partial migration,
  and an authenticated dead-man script.
- `f9f1b03` — a BTC SPV reorg now resumes the wallet rescan at the fork height
  instead of re-deriving the whole retained header window.
- `aec1a76` through `d2e65a8` — a per-height BTC wallet undo journal, sends
  tracked outside the height journal, hidden balances during recovery, and
  fail-closed persistence for sends and reservation restores. Design and known
  limits are in `docs/superpowers/specs/2026-11-09-btc-undo-journal-design.md`.
- Fund-safety and authentication: authenticated VTR sends with passphrase and
  TOTP, authenticated swap refunds, DEX terms bound to verified order data,
  bounded order gossip, risk cleared only on confirmed settlement, legacy BTC
  state failing closed, and transient key/passphrase buffers zeroized.
- Cold-stake funding attestation: the offline signer now refuses to sign unless a
  pinned node key attests that the funding outpoint is confirmed, unspent, and
  free of mempool spends. Design in
  `docs/superpowers/specs/2026-11-15-cold-stake-funding-attestation-design.md`.
- BTC send recovery after `f8e0162`: confirmed sends carry their signed bytes
  into the journal so a covered reorg keeps the input held; corrupt journals
  fail closed; saves sync the parent directory and treat a post-rename failure
  as uncertain; `set_utxo_path` adopts a path only after a successful write.
- BTC wallet state fails closed when missing or from a pre-v3/future version.
  The daemon creates first state only under `--btc-attest-new-seed`; the desktop
  creates both network state files when it generates a wallet, before saving the
  wallet file. Wallets hold an OS-level exclusive lock on their state path; a
  second writer is refused. `btc-state-stage promote` requires exact archived
  bytes, the reviewed archive SHA-256, and explicit recovery preconditions; it
  remains experimental, and mainnet promotion is hard-disabled. Saves,
  promotion, and the peer cache use unique exclusive temporaries.
- BTC claim replacement validation now checks the raw copy, witness, preimage,
  fee, destination, and the order's amount and expiry.
- Broadcast status: a timeout without a reject is reported as submission, with
  tests for a silent peer, an explicit reject, and a disconnect.
- RPC transaction lookup resolves through the body source, so a pruned block
  body no longer returns 404.

Changes already described in the earlier package, up to `f8e0162`:

- BTC send recovery: a broadcast error no longer releases inputs. The signed
  transaction is retained, listed, and can be rebroadcast; corrupt pending state
  fails the wallet load.
- Cold staking: the spending key is written to a separate encrypted file with a
  dedicated passphrase and never crosses IPC or HTTP. Exports cannot overwrite
  an existing file. `vtorrent-cli cold-stake spend` builds and signs a P2CS
  spend offline; the two-daemon test withdraws a confirmed cold-stake output.
- BTC claim replacement lineage is validated on load, persist, and restore; a
  longer recovered chain adopts its latest signed raw transaction.
- Legacy BTC staging enforces a private seed file and has simulated crash-state
  coverage for both publication windows.
- Local DEX orders survive gossip pressure; a remote flood cannot evict them,
  and an all-local book still stays bounded.

**Where we would most value scepticism:**

1. **TOTP replay state.** The guarantee is host-local by design: a wallet
   restored on a second machine does not inherit the consumed step. We have
   documented that limitation (`docs/totp-replay.md`), but we want an opinion on
   whether the fail-closed behaviour on a corrupt or symlinked state file, and
   the concurrency handling, are sound.
2. **Torrent adoption and partial migration** (`vtorrent-torrent/src/engine_disk.rs`).
   Every piece is hash-verified before adoption, and a partial migration copies
   only verified pieces into a fresh info-hash-bound destination before an atomic
   install. We know of one documented race with a concurrent intermediate
   directory replacement and want to know whether it is exploitable in practice.
3. **The dead-man / heartbeat monitoring path**, not for its crypto but for its
   failure modes: whether silence-based alerting can be defeated by anything
   short of a compromised host.
4. **BTC SPV reorg handling and the undo journal.** Reorg rollback was narrowed
   to the fork height, then extended with a live-process undo journal. We want a
   check that outputs above the fork are dropped without losing reservations,
   swap contracts, pending transactions, or a restored reservation's prior
   state, and that an interrupted or uncovered replay fails closed.
5. **The R1 cold-staking rule and wallet encryption**, with an explicit verdict
   either way.
6. **BTC send recovery.** Check that an ambiguous broadcast cannot release
   inputs a peer may have accepted, that rebroadcast authorization is
   sufficient, and that corrupt or legacy pending records fail closed rather
   than becoming spendable.
7. **Cold-staking key custody and spending.** Verify the encrypted export
   format, passphrase handling, script/key matching, locktime enforcement, and
   that the offline CLI cannot be induced to sign for the wrong script or
   destination.
8. **Claim replacement lineage.** Confirm the structural validator covers every
   supported recovery path and that no state can price a fee bump from a stale
   raw transaction.
9. **Legacy migration tooling.** Review `btc-state-stage` parsing, permission
   checks, publication and archival ordering, and the crash states it refuses.
   Ownership and writer isolation are still operator assertions.
10. **Desktop wallet lifecycle and backup containment.** Check that the
    stopped-node guard cannot be bypassed by a concurrent startup, that a quote
    or pending send cannot cross a wallet switch, and that the fail-closed
    backup/restore rules preserve recovery data rather than silently replacing
    it. Note the deliberate consequence: BTC, swap, and cold-stake records
    cannot currently be restored automatically by the desktop app.

**Known and accepted, so it does not need re-litigating:**

- `bincode` 1.x is unmaintained (`RUSTSEC-2025-0141`, warning only, no known
  exploit). Replacing it is a wire-format compatibility change, recorded as
  accepted risk for this launch.
- The other four audit warnings (`chrono` via the Tauri/serde stack, `rand`
  0.7.3 via `rusty-leveldb` in `vtorrent-snapshot`, `glib` in the Tauri GTK
  stack, and `proc-macro-error` in the desktop build macros) are not in the
  daemon release graph. The full five-warning reachability table is in
  `docs/security-review-request-2026-10-27.md` §Dependency audit warnings.
- The 256 MiB RSS budget assumes three pinned glibc allocator variables; without
  them a BTC-enabled fleet peaked at 279.7 MiB. That is a deployment
  requirement, not a code defect.
- The seed1 dead-man heartbeat was installed and exercised on 2026-11-13: a
  stopped timer produced one alert and restoring it produced one recovery. The
  operator accepted the ping URL's exposure during installation because it only
  permits pinging this check. That is an operational risk decision, not a
  code-review item.
- The legacy BTC wallet file has no seed identity. `btc-state-stage` can stage
  and archive it, but cannot prove ownership or stop other writers. This is a
  known open risk for existing wallets, not accepted for release without the
  operator procedure and real power-loss testing.

**Deliverable.** Findings with severity, reproduction, and a concrete fix; an
explicit verdict on the R1 rule and wallet encryption; and a residual-risk
section for anything you consider acceptable.

**Please tell us:** whether you can take this, your estimated timeline, and any
access or information you need. If the scope is too broad for the time you have,
say so and we will prioritise.

Thank you.

---

## Before sending

- Confirm that this note and the scope name `b4c5c60218db5992098b9306bfa62be49c60a410` and include the image and window from `docs/release-candidate-rc14.md`.
- Decide who receives it. Sending it is the step that actually opens the gate —
  drafting it does not.
