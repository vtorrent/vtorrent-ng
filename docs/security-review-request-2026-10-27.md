# Independent security review request

Updated 2026-11-14. This package requests an independent human security review.
The prior internal model review in `docs/2026-10-26_security-vtorrent-report.md`
does not satisfy the independence gate.

`docs/security-review-cover-note.md` holds the message to send: the ask, what
changed since the last round, the questions we most want challenged, and what is
already accepted so it is not re-litigated.

## Review target

Review the candidate `f8e016246434c1f8764a88c82438c4b199f973ea`. This is the
current review head. It is **not** the image in the active 168-hour soak: that
fleet runs `6a49727`, which predates the fund-safety and recovery changes below.
Under the soak provenance policy, the shipping image needs its own full window
after this review and its fixes settle.

The tables below record the earlier deltas; the last table covers the range
from `b729e36` to the review head. Check out the latest `main`: commits after
`f8e0162` are documentation only. Confirm that with
`git diff --name-only f8e0162..HEAD -- '*.rs' '*.toml' '*.ts' '*.tsx'`
returning nothing.

The deltas added after `dd98176` are the reason this request was refreshed; the
original 2026-10-27 scope predates them.

| Commit | Full hash | Purpose |
|---|---|---|
| Candidate | `70ea7b9dfb3c65dcfb30cb03ec8c168c5d3cbffd` | Reviewed feature baseline |
| `1901bb9` | `1901bb9c458c5211306c7f79721e111298bd416a` | Two-peer filter-header agreement |
| `513efde` | `513efdecd072707c0060f09ac71cf37ed205583a` | Network-group diversity and failover |
| `c465d1b` | `c465d1b747d5ff7994d4a78fa795ae6bee6ef118` | Five-peer headroom and in-cycle retry |
| `6f617ce` | `6f617ce0dff5ee108b610d5c0b2f534ef3c5d9ef` | BTC SPV health metrics |
| `7ec3b2f` | `7ec3b2fa7c790a23e98277f54de7920f27028495` | Non-blocking metrics during scans |
| `2c381fa` | `2c381fa1f1bff1dd365056412456da708c3785eb` | Persistent compact-filter peer cache |
| `dbd2034` | `dbd20340860815dfb71a7caf6cff3124cf39ca26` | Stale-scan alert fix |
| `aeb103a` | `aeb103a782058960b5a103a455d0335f9f3f1962` | Four fresh-review BTC findings |
| `8639a7f` | `8639a7fc4a62c72420a34c4cd0d34e866c67a42f` | Restart lineage rescan |
| `9b9a4f7` | `9b9a4f73a00c2f816669c1ecb18c270c7df9b9d0` | External taker claim recognition |
| `c3b3847` | `c3b3847356b46befb3cab96a5ce280831c727ce6` | Hide restored outputs until rescan |
| `287a505` | `287a5054f7884b1c5671cd6c01f4119330f1e441` | Stuck-scan alert |
| `c3a79fa` | `c3a79fa34d8dc1b25a1f7f3f1cad8bced40816c2` | Keep outputs locked through partial scans and restarts |
| `5f4c788` | `5f4c788d06995b5c77202b531c52055d7cf79c6c` | Return canonical Bitcoin transaction IDs |
| `8ba6331` | `8ba6331fe7465acfc7d586b50e4160bc58dc58a8` | Whole-project review remediation |
| `9a22bcd` | `9a22bcd8ccd9bbdb4caf9bec794640fc3c0111aa` | Track release dependency lockfile |
| `c292fc4` | `c292fc461f231e732b27de2c9a057de3a9996fed` | Taker-side external VTR claim RPC |
| `dd98176` | `dd98176fcb6eb724e0753df8ee47dba40df9dc22` | Four-peer mainnet target and 256 MiB steady RSS policy |
| `ff148fd` | `ff148fdc6f39e4fac8beb873297c60e8928874f6` | Codebase-wide review remediation (consensus, overlay, P2P, RPC, torrent, UI) |
| `35ea52e` | `35ea52e4fb847a35f0aa096bbef34baf49ff324e` | TOTP replay state, torrent adoption/partial migration, authenticated dead-man |
| `800fe0a` | `800fe0a85dc743aab8b11ef3f2f9875305bc98ee` | Docs: allocator requirement for BTC SPV fleets |
| `115178b` | `115178b2e176a3b9526a989c626da1676026f08b` | Ops: soak status warns on missing allocator settings |
| `86344c5` | `86344c59e7d7231bcc14c768350416274bd99ec5` | External dead-man heartbeat for seed1, with hermetic tests |
| `f9f1b03` | `f9f1b038f973343695d726c8171d81cc5404b11b` | BTC SPV reorg resumes the rescan at the fork height; lag gauge omitted while the scan cursor is 0 |
| `bae6291` | `bae6291823be8f067158af6e4e3edfe6771b4a83` | Ops: soak sign-off collector |

Deltas after the earlier review head:

| Commit | Purpose |
|---|---|
| `aec1a76` | Per-height BTC wallet undo journal |
| `d6b2767` | Replay the undo journal on a covered reorg |
| `d2e65a8` | Atomic, fallible send and reservation persistence |
| `a66e4bc` | Alert when BTC SPV never completes a first sync |
| `b729e36` | Seed-derived UTXO filename; quiet the bootstrap scan alert |

Deltas added after `b729e36`:

| Commit | Purpose |
|---|---|
| `4ca704f` | Close confirmed swap and wallet fund-loss paths |
| `4413c9e` | Desktop claim and wallet persistence gaps |
| `ff164bf` | Authenticate desktop BTC spends; fail closed on legacy state |
| `9aa3604` | Restore 2FA backups; stop staking on lock |
| `440e44a` | Bind DEX orders and matches to verified terms |
| `a46b029` | Require a passphrase and TOTP for VTR sends |
| `39273e0` | Authenticate swap refunds |
| `53e75c2` | Bound untrusted DEX order gossip |
| `f406d79` | Clear swap risk only on confirmed settlement |
| `f096b30` | Enforce a minimum new-wallet passphrase length |
| `990d66e` | Verify legacy wallet migration against the seed |
| `89d2338` | Fail closed on legacy BTC wallet state |
| `b6b1b71` | Alert when the BTC scan stops making progress |
| `69b6f8b` | Zeroize transient wallet key and passphrase buffers |
| `b2a5fb5` | Encrypted cold-staking spending-key export |
| `7684033` | Prevent export overwrite; retain pending BTC inputs |
| `71be577` | Retain and rebroadcast uncertain BTC sends |
| `747fbae` | Offline cold-stake spending builder and CLI |
| `3ae1b87` | Enforce private seed files in legacy BTC staging |
| `41641f8` | Validate BTC claim replacement lineage |
| `72960bb` | Keep local DEX orders through gossip pressure |
| `ab57acf` | Test-only isolation for the cold-stake desktop fixtures |
| `f8e0162` | Zeroize the remaining WIF encoding, export, and send buffers |

Other commits in the range are tests, formatting, and documentation. The full
sequence is `git log b729e36..f8e0162`.

Docs-only commits in the delta are evidence, not implementation changes. The
`ff148fd` review findings are documented in
`docs/2026-10-29_codebase-wide-review.md`.

## Verification

```bash
git clone https://github.com/vtorrent/vtorrent-ng
cd vtorrent-ng
git checkout f8e016246434c1f8764a88c82438c4b199f973ea
git rev-parse HEAD
bash docs/security-review-2026-10-26/verify-fixes.sh
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/proof/Cargo.toml --target-dir target
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/rpc-proof/Cargo.toml --target-dir target
cargo test --workspace --all-features
```

The `f8e0162` workspace run passed 52 groups, 1,282 tests, zero failures. CI
repeats the `--all-features` suite on every push. Set `VTORRENT_TOTP_STATE_DIR`
to a fresh directory before running the suite so the host-scoped TOTP replay
state does not collide with an existing installation.

The legacy-wallet guard and progress-based scan alert are in this head. The
`btc-state-stage` migration tool is experimental and not approved for production
use. Review its parsing, permissions, and publication logic, but its ownership
and writer-isolation confirmations remain operator assertions. Real
power-loss testing and the operator procedure are still open; do not treat this
review as approving migration of production wallets.

## Scope emphasis

- BTC SPV peer agreement, failover, peer cache, and filter-header trust.
- Wallet restart lineage and reorg reconciliation.
- Mainnet swap claim verification and settlement observation.
- RPC authorization on sensitive routes.
- The R1 cold-staking rule and wallet encryption, with explicit verdicts.
- BTC send recovery when a broadcast result is uncertain, including retained raw
  transactions, rebroadcast authorization, and input-release rules.
- Cold-staking key export and the offline P2CS spend path, including key
  custody, script/key matching, locktime enforcement, and the encrypted file
  format.
- BTC claim replacement lineage and fee-bump recovery.

Added with `ff148fd` and `35ea52e`:

- **TOTP replay state.** The accepted step is stored in the encrypted wallet and
  in a locked, authenticator-scoped host file (`vtorrent-wallet/src/otp.rs`),
  consumed on unlock, enrollment confirmation, verification, and disable
  (`vtorrent-rpc/src/handlers/mod.rs`, `vtorrent-tauri/src/commands/wallet.rs`).
  Review the fail-closed behaviour on a corrupt or symlinked state file, the
  concurrency guarantee, and the documented residual risk: the guarantee is
  host-local, so a wallet restored on a second machine does not inherit the
  consumed step. See `docs/totp-replay.md`.
- **Torrent adoption and partial migration.** `vtorrent-torrent/src/engine_disk.rs`
  verifies every piece before adopting a legacy complete torrent and copies only
  hash-verified pieces into an info-hash-bound destination before an atomic
  `renameat2(RENAME_NOREPLACE)` install. Review the ownership-marker bypass
  paths, symlink handling, the destination-not-overwritten rule, and the
  documented concurrent intermediate-directory replacement race. See
  `docs/torrent-adoption.md`.
- **Dead-man monitoring.** `deploy/seeds-monitoring/deadman.sh` runs a pinned,
  dedicated SSH identity against a restricted forced command and deduplicates
  failure/recovery notifications. Review argument handling, command injection
  surface, state-file races, and secret handling.
- **Consensus and networking changes in `ff148fd`.** `vtorrent-node/src/block.rs`,
  `consensus.rs`, `vtorrent-overlay/src/holepunch.rs`, `overlay.rs`,
  `vtorrent-p2p/src/peer.rs`, and the torrent DHT/UDP parsers. The
  `docs/2026-10-29_codebase-wide-review.md` report lists each finding with its
  fix; verify the fixes rather than re-deriving them.

## Deliverables

- Findings with severity, reproduction, and a concrete fix.
- Explicit verdict on the R1 cold-staking rule and wallet encryption.
- A residual-risk section for anything accepted.

## Evidence

- `docs/security-review-brief.md` and `docs/security-review-handoff.md`.
- `docs/2026-10-26_security-vtorrent-report.md` and its local proofs.
- `docs/btc-spv-soak-plan.md` and `docs/soak-log.md` for probe and soak
  evidence, including the 60-cycle active swap soak.
- `docs/btc-spv-multinode-reorg-test.md` for real-Bitcoin Core tests,
  including the separate-node taker claim.
- `docs/2026-10-29_codebase-wide-review.md` for the whole-tree findings and
  their fixes.
- `docs/totp-replay.md` and `docs/torrent-adoption.md` for the new paths'
  intended guarantees and known limits.

## Status

- The `9a22bcd` BTC probe completed its 24 h window with zero scan errors. The
  final source `dd98176` adds the four-peer target; the release fleet settled
  at 237/233 MB after scan, below the 256 MiB steady-state alert.
- `35ea52e` passed an isolated three-container cross-chain cycle against real
  Bitcoin Core regtest: BTC funding discovery, six VTR funding confirmations,
  swap preserved across a maker restart, BTC claim with six confirmations, and
  a separate taker node broadcasting the VTR claim.
- CI runs the workspace suite with `--all-features` on every push. The last
  counted local run on the review head was 52 groups, 1,282 tests, zero
  failures. The whole-tree review remediation is documented in
  `docs/2026-10-29_codebase-wide-review.md`.
- The four BTC SPV release-mode tests run against real Bitcoin Core regtest now
  run nightly in CI rather than only by hand; they were silently stale for three
  days before this was fixed.
- `f9f1b03` narrows a BTC SPV reorg rescan to the fork height. Before it, a
  three-block mainnet reorg at height 974399 triggered a 100,001-block rescan per
  node (13, 13 and 40 minutes) and fired `BtcSpvScanStuck`. Unit tests and the
  regtest suite cover the new path; the fleet evidence for it is still
  accumulating.
- **Memory is a deployment requirement, not a code fix.** A BTC-SPV fleet
  launched without the pinned allocator settings peaked at 279.7 MiB and broke
  the 256 MiB budget; a matched comparison measured 162–213 MiB default versus
  116–128 MiB tuned. See `docs/soak-log.md` 2026-11-01.
- The active release-candidate fleet runs `6a49727` toward 2026-11-20T19:20Z.
  It does not cover the deltas after that commit. Production dead-man
  provisioning is complete; an independent human review and a full window for
  the eventual shipping image remain open.
- Please confirm scope, timeline, and access needs before starting.
