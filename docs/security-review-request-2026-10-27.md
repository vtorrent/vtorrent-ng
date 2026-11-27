# Independent security review request

Updated 2026-11-26. This package requests an independent human security review.
The prior internal model review in `docs/2026-10-26_security-vtorrent-report.md`
does not satisfy the independence gate.

`docs/security-review-cover-note.md` holds the message to send: the ask, what
changed since the last round, the questions we most want challenged, and what is
already accepted so it is not re-litigated.

## Review target

Review `a335a385c6be1ed10f11f171ae563b8ce4e95825` (`v2.0.0-rc6`), not `main`. See `docs/release-candidate-rc6.md` for the exact image, binary, soak and desktop evidence. Check `git diff --name-only be78829..v2.0.0-rc6`: only `.github/workflows/build.yml` and `vtorrent-tauri/tauri.conf.json` changed. Earlier pass-15 evidence applies only where explicitly identified. The deferred BTC repair on `main` is outside the candidate.

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

Deltas after `f8e0162`:

| Commit | Purpose |
|---|---|
| `cc6b708` | Attestation encoding, secp256k1 signatures, funding binding and policy |
| `4b932e6` | Build attestations from chain and mempool state, with refusal reasons |
| `1f41c27` | Attestor key storage and the two HTTP endpoints |
| `0ce87ec` | Offline signer verification and CLI wiring |
| `cad1410` | Endpoint and workflow documentation |
| `8e9d401` | Review fixes: pruned-body lookup, strict field encoding, version and clock-skew checks, optional funding transaction, startup pin, creation race |
| `58f86b5` | BTC send holds across reorgs, restart, legacy files, and write failures |
| `a18edf9` | BTC claim replacement validation against the HTLC terms |
| `8e45a7e` | UI reports BTC submission as unconfirmed |
| `0a71f71` | RPC transaction lookups resolve through the body source |
| `6c5417d` | Real-node test for an uncovered BTC reorg across restart |
| `be34747` | Coverage limits closed: pruned-body lookup test, high-S signature, spent outpoint |
| `1077a02` | Fail closed on missing, pre-v3, and future-version BTC state; explicit new-seed attestation |
| `83d157a` | Legacy promotion, desktop authorization, unique exclusive save temporaries, scan-save failures |
| `a4724d5` | Review fixes: atomic attested create, durable consumed marker, archive-digest-bound promotion |
| `ba46fd6` | Single-writer state lock, immediate dual-network desktop state, mainnet promotion disabled |

Deltas after `ba46fd6`:

| Commit | Purpose |
|---|---|
| `91dd05b` | BTC initialization record, symlink/hard-link rejection, separate-process lock test |
| `30f7224` | Backup takes BTC locks; automatic restore of BTC, swap, or cold-stake records refused |
| `1008166` | Backup locks before checking state existence; cross-process BTC test isolated |
| `6c833cf` | Desktop node startup and wallet lifecycle serialized under the node guard |
| `060bedf` | Cold-stake funding requires the wallet passphrase and a fresh TOTP code |
| `611d56b` | Send quotes bound to the signing wallet; wallet switch clears pending quotes |
| `a202519` | Desktop DEX/swap/BTC actions authorize against the desktop wallet; maker address bound |
| `713fcc5` | Empty TOTP replay state recovery (superseded: an existing empty file now fails closed to prevent replay after truncation) |
| `aace0e5` | Dust BTC outputs rejected or absorbed; desktop 2FA enrollment requires the passphrase |
| `71b025f` | Torrent credit replay blocked; tracker peers pass the SSRF target filter |
| `ee62976` | Wallet passphrase file must be owner-only |
| `d76669f` | Desktop refund-bump auth fixed; auth before wallet sync; regtest-only raw claim key; 2FA errors visible |
| `b9b05e3` | Test-only: explicit tracker address policy (no production behavior change) |
| `8f7fe14` | Release actions SHA-pinned, read-only default token, `--locked` release build, honest signing text; seed digest verification and RPC key; alert topic via env |
| `49d56d2` | Multisig 20-key cap; wallet save temp cleanup; cold-stake Debug redaction; refund-bump UI credentials |
| `aacf371` | Legacy BDB page offsets and dump parsing; 32 MiB import cap |
| `20e3fe6` | BDB overflow reconstruction and fixtures; atomic TOTP replay state; torrent write claim; seed key ownership and container test; multisig boundary tests |
| `089e7c1` | Legacy 20,000 block sigop limit at validation, application, mempool, and template selection; raw oversized-script counting; same-block P2SH regression test |
| `9f3d4e9` | Cold-stake exports in backups; wallet version/unknown-field rejection; snapshot integrity checks; BTC staging confirmations and provenance; mainnet RPC key requirement; release CORS narrowing; CSP and font cleanup; real-wallet check harness |
| `f0013bb` | Clear AEAD error; worst-case fee estimate; single-key fallback removed; 2FA migration warning; TOTP seed clearing and clipboard timeout; HTLC classification; minimal `push_int`; backup cron and benchmark gate hardening; dead-man path; mkey cap, dump re-read, varint overflow |
| `b02dd33` | Incentive payouts disabled pending receipts; `ut_vtr` validation; uncompressed-key signing; P2SH push-only; template framing; cold-stake export round-trip; staking-intent atomic write; RPC bind failure exit; query rate limits; 2038 HTLC guard; torrent piece caps |
| `3d32e8c` | Store append monotonicity and rebuild metadata; backup verify-only TOTP; best-effort wallet TOTP re-save; unsolicited torrent piece rejection |
| `6c2193b` | SPV genesis pin and best-chain inclusion; bounded peer dials; torrent receive deadline and bitfield cap; `/ws` origin required; non-public seed probe; VTR funding selection lock and mempool filter |
| `f897f5c` | Per-peer torrent lease ownership; partial-scan BTC reservation keeps contract; per-IP overlay session cap |
| `db0902c` | Testnet RPC/monitoring loopback binding; Grafana password required and anonymous off; bitcoind RPC scoped to Docker network; release image requires explicit runtime base |
| `a514d5b` | Maximum future block drift reduced from 2 hours to 5 seconds, bounding stake-timestamp grinding and CLTV drift |
| `99b02fc` | Kernel denominator uses eligible stake inside the age window, excluding young and aged-out coins |
| `8fed10c` | Failed funding admission reverts the order and removes its recovery record; taker VTR claim is submitted automatically after the observed BTC claim |
| `eddd565` | SPV pins the VTR genesis and fixed consensus difficulty; inclusion requires best-chain ancestry |
| `4d32ab2` | Signed bandwidth receipts over BEP-10; settlement pays only a verified receipt within 5% agreement and pays nothing on missing/disputed/replayed receipts; payouts enabled by default |
| `5011acf` | Peer dials run in parallel batches with per-attempt timeouts |
| `fce6495` | Untrusted PEX dials run in detached tasks and are registered from the event loop |
| `733e476` | Release text states unsigned; published releases get a generated, verified SHA256SUMS asset |
| `c874dac` | Fifth-pass fixes: bounded block spacing, unconditional SPV pin, detached bootstrap dials, receipt-gated retryable settlement, swap deadlock/refund fixes, BTC reservation release, cold-stake key handling, and CI/ops hardening |
| `5cdd580` | Follow-up closures: cumulative receipt epochs, account-wide TOTP replay check, append-only cold-stake registry, source-built container image, hardened testnet overlay, frontend audit, BTC fee override, and documented external-taker claim path |
| `2a25e3f` | Taker match import with local swap persistence; `--checkpoint HEIGHT:HASH` enforcement and launch procedure; minimal P2CS/HTLC CLTV encoding with legacy parsing |
| `1e42381` | Optional minisign-signed `SHA256SUMS`, release verification script and key-custody guide; import and checkpoint guard tightening |
| `29892b9` | Canonical/two-phase match import, funded taker address echo, separate receipt epochs with cumulative engine totals, persisted checkpoints, strict release verification |
| `514fe9f` | Post-persist import recheck, receipt domain v3 with explicit peer epochs, genesis-bound checkpoint files, release checksum self-hash fix, unlisted-asset rejection, key/checkpoint tooling, per-node image overrides |
| `9533a2c` | Non-blocking BTC status/address reads during SPV sync; release-fleet BTC attestation and code-commit evidence |
| `dd7676f` | Equal-work fork tie-break (lower tip hash) and SPV header rebuild from the stored chain on restart |
| `04b3c4a` | Equal-work SPV proofs, canonical SPV rebuilds, equal-height tip discovery, checkpoint policy, desktop swap services, claim retries, receipt/payment durability, incentive caps |
| `7a216ee` | Second-pass fixes: fork-relay removal and ban bounds, recoverable checkpoint reorgs, SPV install race, header-index serving, fresh/final swap-claim evidence, preimage/witness recovery, incentive debt protection and final-receipt pacing |
| `c27daf2` | Automated VTR claim fee replacement, chunked SPV snapshot with generation checks, and side-header pruning |
| `d790c16` | Persistent incentive-debt snapshot and ledger across graceful restarts, with a persistent process budget |
| `f8ccbfe` | Fsynced receipt-backed incentive-account journal, crash recovery, and receipt-epoch debt keying |
| `1f5fa81` | Journal replay preserves ledger payment progress, compaction durability, receipt-epoch reconciliation, stale-write versions, and budget-before-emit ordering |
| `2b65c3a` | Receipt-backed removal guard, poisoned-journal fail-closed behavior, and journal directory fsync |
| `66bc705` | Reservation rollback on failed durable writes, five-second result draining, and settled-ledger pruning |
| `9594df4` | Signed payment intents persisted before broadcast, startup confirm/rebroadcast/release reconciliation, and durable intent cleanup |
| `10c4671` | Stop re-emitting a recovered payment while its txid is pending; live recovery drill regression |
| `2fdd9aa` | Pass 6: torrent framing/storage/final-receipt fixes from the two-process E2E, journal compaction and migration fail-closed, mempool-aware UTXO selection, faucet authentication |
| `3e8b2cb` | Inbound torrent serving: opt-in seed listener, verified piece serving, signed receipts, RPC/desktop registration |
| `3dd99a0` | Private torrent tracker/peer targets under `--testnet`; fleet option to open the seed listener |
| `04115cb` | Keep a peer bitfield that arrives before the extension handshake; bitfield-first incentive E2E |
| `4b34b9e` | Desktop torrent settings request/response type split (UI only) |
| `d5e80e1` | Staking skips outpoints already spent by a mempool transaction; unit test |
| `17db693` | Pass 7: single-owner piece assembly, peer-aware leases, receipt bounds, session-cancelled serving, magnet registration, staking/payment protection, RPC/desktop guards |
| `ef1d1a1` | Pass 8: lost-block/stall/completion fixes, BEP-10 id direction, per-peer receipt throttle, per-IP cap, non-stake payment fallback, dust and lifecycle follow-ons |
| `22056d4` | Pass 9: empty-address fallback, 10 s receive slices and progress watchdog, per-connection+per-address receipt throttle, Cancel-safe tombstones, bounded verified-piece semaphore, current-window/idle eviction, no periodic dust write-off, registry pruning, magnet Stopped, largest-stake reserve, owner-scoped clear_piece, upload dedupe/counter, tracker-init Stopped, IPv6 /64 cap, early address/ID edges |
| `d376506` | Pass 10: ledger-aware torrent-removal guard, desktop receipt-removal guard, dust-preserving ledger policy, live DELETE refusal/release; follow-ups for the node1 empty-address error and a live ledger-backstop case |
| `e091936` | Pass 11: release-lock audit fixes for RUSTSEC-2026-0594 (`cookie 0.18.3`) and RUSTSEC-2026-0592 (`url 2.5.9`); `cargo audit` clean; exact-image live DELETE refusal/release; follow-ups for node1 empty-address, live ledger backstop, fee-floor/stake-age, and desktop settle/write-off |
| `0f02ef5` | Pass 13: canonical torrent block geometry |
| `83f1b08` | Pass 13: pending incentive debt is a removal liability and eviction input |
| `f772993` | Pass 13: removal guard consults the live incentive journal |
| `f14bcf0` | Pass 13: torrent payout addresses are bound to the wallet; Tauri removal checks the guard before cancelling |
| `3891ff5` | Pass 13: reserve structurally stakeable coins regardless of age in incentive payments |
| `123673d` | Pass 13: apply the stake reserve to manual sends, cold-stake funding, and swap HTLC funding |
| `4402f7a` | Pass 13: legacy import creates BTC state and binds the default address to the imported key |
| `0af5546` | Pass 13: seeder IPv4-mapped keys, prune race, partial-piece watchdog, resume-bit distrust, sub-minimum staking status |
| `001c05e` | Pass 13: deterministic two-peer split regression |
| `b96091c` | Pass 13: require an unlocked hot wallet that owns the torrent payout address |
| `0960fa7` | Pass 13: run the removal guard outside the session lock and re-check debt |
| `80f9eef` | Pass 13: keep the cold-stake funding path as an explicit restake exemption from the hot-wallet reserve |
| `a0724dd` | Pass 14: reserve intersection and quote revalidation, unlocked-WIF torrent payout, desktop staking key binding, multi-master legacy extraction, bounded un-receipted debt, same-wallet lock/unlock, surfaced node-start errors |
| `be78829` | Pass 15: attested BTC-state repair, force-remove surface, legacy network-only BTC-state guard |

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
git checkout --detach a335a385c6be1ed10f11f171ae563b8ce4e95825
git rev-parse HEAD
bash docs/security-review-2026-10-26/verify-fixes.sh
cp release/Cargo.lock Cargo.lock
cargo audit --file release/Cargo.lock
cargo test --locked --workspace --all-features
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/proof/Cargo.toml --target-dir target
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/rpc-proof/Cargo.toml --target-dir target
```

`release/Cargo.lock` is the tracked lockfile; the workspace `Cargo.lock` is
gitignored. Copy it before testing so the audited dependency graph is the graph
under test. Linux desktop builds need the Tauri system prerequisites:
`libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`,
`librsvg2-dev`, `libssl-dev`, and `patchelf` (or the distribution equivalents).
A daemon-focused review can run
`cargo test --locked -p vtorrent-daemon -p vtorrent-rpc -p vtorrent-wallet ...`
without them; the full workspace suite requires the desktop libraries.

The `e091936` workspace run passed 56 test groups and 1,528 tests with 0
failures and 13 ignored. The pass-15 candidate `be78829` passed **57 groups and 1,555 tests, 0 failed,
13 ignored** on the tracked release lock. Canonical CI
run **45357625083** on the docs-only child `cd0e975` passed every job,
including the no-cache Docker image build. CI repeats the `--all-features`
suite on every push.
Set `VTORRENT_TOTP_STATE_DIR` to a fresh directory before running the suite so
the host-scoped TOTP replay state does not collide with an existing
installation. The ignored Bitcoin Core reorg tests are documented in
`vtorrent-daemon/tests/btc_spv/mod.rs` and run against a disposable regtest
container.

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
- **Desktop wallet lifecycle and backup containment.** Node startup and wallet
  switching serialize under one guard; cold-stake funding requires the wallet
  passphrase and a fresh TOTP code; send quotes are bound to the signing wallet.
  Desktop restore deliberately refuses BTC, swap, and cold-stake recovery data
  because a backup cannot prove it is current. Review whether those guards can
  be bypassed and whether fail-closed recovery is the right trade-off.

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
- `docs/desktop-recovery-runbook.md` for the manual procedure that replaces
  automatic restore of BTC, swap, and cold-stake recovery records.
- `docs/2026-11-17_ai-supplementary-review.md` for an internal AI-assisted
  review pass. It found and fixed desktop authorization defects in `a202519`,
  TOTP replay recovery in `713fcc5`, and dust/2FA enrollment issues in
  `aace0e5`. It is **not** independent human review.
- `docs/2026-11-17_ai-supplementary-review-pass2.md` for a second AI-assisted
  pass covering consensus, P2P/overlay/SPV, RPC/storage, torrents, and a
  regression review of the first pass. It fixed torrent credit replay and
  tracker SSRF in `71b025f`, passphrase-file permissions in `ee62976`, and the
  remaining desktop auth gaps in `d76669f`. It is **not** independent review.
- `docs/2026-11-17_ai-supplementary-review-pass3.md` for a third AI-assisted
  pass covering legacy/snapshot parsers, wallet internals, core/script, the
  frontend boundary, and release/ops. It fixed the multisig key cap, wallet-save
  temp handling, cold-stake debug redaction, and release/seed hardening in
  `8f7fe14` and `49d56d2`. The initial BDB parsing fix is in `aacf371`; the
  follow-up recovery, TOTP, torrent, and seed fixes are in `20e3fe6`; the block
  sigop limit is in `089e7c1`; and the wallet, snapshot, staging, RPC, and CSP
  fixes are in `9f3d4e9`. It is **not** independent review.
- `docs/2026-11-17_ai-supplementary-review-pass4.md` for a fourth AI-assisted
  pass covering consensus/staking, wallet signing, torrents/incentives,
  P2P/SPV, RPC/daemon/store, and BTC swaps. It fixed the incentive payout
  default, uncompressed-key signing, P2SH push-only, template framing, and
  related findings in `b02dd33`, with store/backup/torrent hardening in
  `3d32e8c`, SPV/P2P/WS/funding hardening in `6c2193b`, torrent/BTC/overlay
  fixes in `f897f5c`, container hardening in `db0902c`, the timestamp-drift
  bound in `a514d5b`, the eligible-stake denominator in `99b02fc`, and the swap
  rollback/auto-claim in `8fed10c`, the SPV genesis pin in `eddd565`, and the
  receipt-gated incentives in `4d32ab2`, and the parallel peer dials in
  `5011acf`, the detached untrusted dials in `fce6495`, the release checksums
  in `733e476`, the fifth-pass fixes in `c874dac`, the follow-up closures in
  `5cdd580`, the taker-import/checkpoint/CLTV work in `2a25e3f`, the
  release-signing wiring in `1e42381`, the import/receipt/checkpoint fixes in
  `29892b9`, the edge-case hardening in `514fe9f`, the BTC liveness/soak
  tooling in `9533a2c`, the fork/SPV fixes in `dd7676f`, and the review
  remediation in `10c4671`. It is **not**
  independent review.
- `docs/2026-11-23_ai-supplementary-review-pass11.md` for the release-lock
  audit fixes (`cookie 0.18.3`, `url 2.5.9`), the exact-image transfer/DELETE
  evidence, and the corrected 1,000-sat fee record. It is **not** independent
  review.
- `docs/2026-11-24_ai-supplementary-review-pass12.md` for the read-only
  four-reviewer pass that produced the remediation list, and
  `docs/2026-11-24_ai-supplementary-review-pass13.md` for the fixes, the exact
  image/binary, the new window, and the remaining release gates. Neither is
  independent human review.

## Dependency audit warnings

`cargo audit --file release/Cargo.lock` exits 0 with **zero vulnerabilities**
and **five allowed warnings**. Only `bincode` is reachable from the daemon
release graph; the other four are desktop, migration-tool, or build-time only.

| Advisory | Crate | Kind | Reachability from `vtorrent-daemon` |
|---|---|---|---|
| `RUSTSEC-2025-0141` | `bincode` 1.3.3 | unmaintained | **Yes** — direct dependency of `vtorrent-node`, `vtorrent-p2p`, `vtorrent-rpc`, `vtorrent-spv`, `vtorrent-torrent`, `vtorrent-cli`, `vtorrent-daemon`. Accepted as a wire/persistence-format compatibility risk; see `docs/mainnet-readiness.md`. |
| `RUSTSEC-2026-0543` | `chrono` 0.4.45 | unmaintained | No — `tauri-utils` → `serde_with` → `chrono`; desktop shell only. |
| `RUSTSEC-2026-0097` | `rand` 0.7.3 | unsound with a custom logger | No — `vtorrent-snapshot` → `rusty-leveldb`; migration tool only. |
| `RUSTSEC-2024-0429` | `glib` 0.18.5 | unsound iterator impls | No — Tauri GTK/WebKit stack; desktop only. |
| `RUSTSEC-2024-0370` | `proc-macro-error` 1.0.4 | unmaintained | No — build-time only via `glib-macros` / `gtk3-macros`; not linked into the daemon. |

`cargo audit` does not fail the build on these warnings. CI runs the audit on
every push against the tracked `release/Cargo.lock` and fails only on
vulnerabilities.

## Status

- The `9a22bcd` BTC probe completed its 24 h window with zero scan errors. The
  final source `dd98176` adds the four-peer target; the release fleet settled
  at 237/233 MB after scan, below the 256 MiB steady-state alert.
- `35ea52e` passed an isolated three-container cross-chain cycle against real
  Bitcoin Core regtest: BTC funding discovery, six VTR funding confirmations,
  swap preserved across a maker restart, BTC claim with six confirmations, and
  a separate taker node broadcasting the VTR claim.
- CI runs the workspace suite with `--all-features` on every push. The last
  counted local run on the review head was 54 groups and 1,436 tests, zero
  failures, with 11 ignored: seven need a Bitcoin Core container, two are manual
  release-mode governance experiments, one is a mainnet-scale header-retention
  test, and one is a doctest. The whole-tree review remediation is documented in
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
- The `6a49727` release-candidate fleet was interrupted by a host restart on
  2026-11-16 and cannot be counted as a continuous window. It predates every
  delta after that commit. Production dead-man provisioning is complete; an
  independent human review and a fresh full window for the eventual shipping
  image remain open.
- Please confirm scope, timeline, and access needs before starting.
