# Security review — cover note

A ready-to-send message for the reviewer. The technical scope lives in
`docs/security-review-request-2026-10-27.md`; this note is the ask, the context,
and the specific questions.

---

## Message

Subject: Independent security review — vTorrent-NG mainnet candidate `ab57acf`

Hello,

I'm asking for an independent human security review of vTorrent-NG before we
open the network with atomic swaps enabled. An internal model review has already
been done and does **not** count for this gate — we need someone who did not
write or previously review this code to look at it with fresh eyes.

**Repository:** https://github.com/vtorrent/vtorrent-ng
**Review head:** `ab57acf7c868c97dff7cce145a45e9fe0200a197`
**Scope:** `docs/security-review-request-2026-10-27.md` in that tree

The code under review is `ab57acf`. It is not the image in the active 168-hour
soak: that fleet runs `6a49727` and predates the fund-safety and recovery
changes below. The eventual shipping image needs its own full window after this
review and its fixes settle.

Check out the latest `main`; commits after `ab57acf` are documentation only.
`git diff --name-only ab57acf..HEAD -- '*.rs' '*.toml' '*.ts' '*.tsx'` must
return nothing.

That document lists the exact hashes, the verification commands, and what we
consider in scope. Nothing is required beyond a clone and a Rust toolchain; no
credentials or live infrastructure access are needed.

**What changed since the last review round.** The earlier review covered up to
`dd98176`, and an earlier package covered `b729e36`. Since `b729e36`:

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

- Confirm the review head is still `ab57acf` or update it; the scope document and
  this note must name the same commit. Do not substitute a later local commit
  without also updating both documents and the soak provenance.
- Do not describe the active `6a49727` fleet as covering this head. The shipping
  image needs its own 168-hour window after the review and its fixes settle.
- Decide who receives it. Sending it is the step that actually opens the gate —
  drafting it does not.
