# Security review — cover note

A ready-to-send message for the reviewer. The technical scope lives in
`docs/security-review-request-2026-10-27.md`; this note is the ask, the context,
and the specific questions.

---

## Message

Subject: Independent security review — vTorrent-NG mainnet candidate `9f3d4e9`

Hello,

I'm asking for an independent human security review of vTorrent-NG before we
open the network with atomic swaps enabled. An internal model review has already
been done and does **not** count for this gate — we need someone who did not
write or previously review this code to look at it with fresh eyes.

**Repository:** https://github.com/vtorrent/vtorrent-ng
**Review head:** `9f3d4e9eb9e9f6c591a84e3f47a9c61b005f1358`
**Scope:** `docs/security-review-request-2026-10-27.md` in that tree

The code under review is `9f3d4e9`. It is not covered by any prior soak: the
`6a49727` fleet predates every change below and was interrupted by a host
restart on 2026-11-16. The eventual shipping image needs its own full window
after this review and its fixes settle.

Read this note and the scope document from the tip of `main`. Confirm that no
code changed after the candidate with
`git diff --name-only 9f3d4e9..HEAD -- '*.rs' '*.toml' '*.ts' '*.tsx'`, which
must return nothing. Check out the candidate itself when running the
verification commands.

That document lists the exact hashes, the verification commands, and what we
consider in scope. Nothing is required beyond a clone and a Rust toolchain; no
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

Three internal AI-assisted supplementary reviews are recorded in
`docs/2026-11-17_ai-supplementary-review.md`,
`docs/2026-11-17_ai-supplementary-review-pass2.md`, and
`docs/2026-11-17_ai-supplementary-review-pass3.md`. They found and fixed the
desktop authorization defects above, the torrent credit replay, the multisig
DoS cap, release/seed hardening, the recovery fixes in `20e3fe6`, the block
sigop limit in `089e7c1`, and the remaining wallet, snapshot, staging, RPC, and
CSP fixes in `9f3d4e9`. They are not independent review and do not count for
this gate.

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

- Confirm the review head is still `9f3d4e9eb9e9f6c591a84e3f47a9c61b005f1358`
  or update it; the scope document and this note must name the same commit. Do
  not substitute a later local commit without updating both documents and the
  soak provenance.
- Do not describe the interrupted `6a49727` fleet as covering this head. The
  shipping image needs its own 168-hour window after the review and its fixes
  settle.
- Decide who receives it. Sending it is the step that actually opens the gate —
  drafting it does not.
