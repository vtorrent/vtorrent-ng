# AI supplementary security review — pass 5

**Date:** 2026-11-18
**Scope:** HEAD `f9a91a2` (six parallel read-only reviewers)
**Status:** AI-only. This is not the independent human review and does not
satisfy the mainnet security gate.

Six reviewers covered consensus/SPV, the signed bandwidth-receipt protocol,
swap automation and recovery, detached peer dialing, wallet/BTC recovery, and
release/operations. Every finding below was traced against the source before a
fix was written. Fixes are on `main` after `f9a91a2`.

## Fixed

| # | Severity | Area | Defect | Fix |
|---|---|---|---|---|
| 1 | Critical | Swap RPC | `match_dex_order` held the `swaps` write guard across the funding submission and re-took the same non-reentrant lock on the failure path, deadlocking every swap reader/writer. | The guard is dropped before submission; the durable record is removed before the order is reverted. A removal failure now keeps the order matched instead of leaving disk and memory inconsistent. |
| 2 | Critical | Incentives | Settlement zeroed counters and the receipt, then emitted a fire-and-forget payment. A locked wallet or build failure silently destroyed the claim while `total_paid` reported success; peers without a receipt were also cleared. | Settlement requires a verified receipt and a non-empty payout address. The agreed amount moves to `unpaid_satoshis`; `total_paid` advances only after the payment task reports a successful broadcast. Failed payments remain queued and are retried. |
| 3 | High | Consensus | Only the future half of the timestamp rule was bounded. A staker could pick timestamps inside the future window and produce multiple blocks per 60-second target. | Production blocks must be at least 55 seconds after the parent (`MIN_BLOCK_SPACING`, 60 − 5s drift). Test/regtest fixtures explicitly opt out. |
| 4 | High | Consensus replay | The future check used the wall clock directly. A backwards clock step made an accepted tip fail replay, and the store treated the failure as corruption and truncated the tail. | The reference time is `max(wall clock, parent timestamp)`, so replay can never reject an already-accepted parent and the live future bound remains 5 seconds. |
| 5 | High | SPV | The genesis pin was installed only when `--btc-seed` was set, and a trusted first header could be any height with no parent. | The pin and the genesis header are installed unconditionally at daemon startup. A pinned chain rejects a first non-genesis header and requires linkage for every later header. |
| 6 | High | SPV reorg | Reorg events persisted fork blocks but never fed them to SPV, so every later `NewBlock` was rejected as an unknown parent and inclusion proofs stayed on the abandoned tip. | Applied fork headers are added to SPV during the reorg handler. |
| 7 | High | BTC recovery | `release_swap_input_keep_contract` released the outpoint and then `add` re-reserved it because the retained contract still named the input. The reconciler logged success while the funds stayed locked and rescanned forever. | `SwapContract` records `input_released`; the release path clears the binding, drops the retained funding transaction, returns the input to the spendable set, and only reports success when it is actually spendable. |
| 8 | High | Cold stake | `generate_cold_stake_keys` recorded the pair before the spending-key export. A failed export left a fundable P2CS script whose key was never persisted, and `spending_wif` is intentionally never serialized. | Generation no longer records anything. The wallet records the script only after export and round-trip verification, and never retains the cold key. |
| 9 | High | Peer dialing | PEX dials were detached, but DHT, GitHub, DNS-seed, cache, and extra-seed paths still awaited unbounded connects inside the node loop. | All bootstrap dials now use the detached path with 3-second resolution/connect timeouts. In-flight dials count toward the peer target, so failed queues no longer suppress seed/DHT fallback. |
| 10 | High | Peer bans | A failed PEX dial recorded a connection failure against the dialed IP, letting a poisoned address ban honest peers. | Only trusted sources (configured seeds, DNS seeds, DHT, GitHub bootstrap) may escalate a failure into an IP ban. PEX failures only affect address-book scoring. |
| 11 | High | Receipts | Every receipt built a fresh secp256k1 context and was verified before the cheap replay check, with no per-peer rate limit. | Shared signing/verification contexts, a 30-second per-peer receipt limit, a read-only replay pre-check before ECDSA, and bounded window/id values. |
| 12 | High | Receipts | Uncompressed legacy keys could never produce a verifying receipt: the verifier hardcoded compressed pubkeys and the sender always serialized compressed. | Compression is inferred from the 33/65-byte pubkey, and the sender uses the key's compression flag. |
| 13 | High | Wallet sends | Desktop quote/send and cold-stake funding selected only the default address and signed with only its WIF. Imported legacy funds were visible but unspendable. | Coin selection covers every wallet address and signing tries every wallet WIF. New `build_payment_with_keys` / `build_payment_to_script_with_keys` paths keep the shared builder. |
| 14 | Medium | Swap recovery | An auto VTR claim trusted any stored preimage, and a BTC refund ignored the observed preimage, so a reorged-out claim could let the taker keep both legs. | Auto-claim requires a current `ClaimConfirming`/`Claimed` observation. BTC refunds are refused after any observed claim or stored preimage. |
| 15 | Medium | Receipts | An attacker-chosen `u64::MAX` settlement id permanently rejected later receipts; negative/reversed windows were accepted. | Receipt ids and window ends are bounded to `now + 600`, and `window_start <= window_end` is enforced. |
| 16 | Medium | Incentives | The account map could grow past its cap forever because eviction skipped every account holding bytes, and unpayable hex peer ids counted as accounts. | A hard cap evicts unpayable, settled accounts; when every account is payable the new id is not tracked. The Tauri summary now clones once instead of six times. |
| 17 | Medium | P2P | A duplicate inbound key used `or_insert`, orphaning the new task and letting its `Disconnected` event delete the live peer. | Duplicate inbound registrations are rejected and the new channel is dropped. Registration re-checks `MAX_PEERS`. |
| 18 | Medium | Release CI | `github.ref_name` was interpolated into a `run:` script; git refnames may contain `$()`, backticks, and semicolons. | The ref is passed through `env:` and quoted as `$REF_NAME`. |
| 19 | Medium | Testnet compose | The post-soak seed overlay published RPC on `0.0.0.0:22625` with a committed API key. | RPC is bound to `127.0.0.1:22625`; desktop discovery only needs the P2P port. |
| 20 | Medium | Backups | `cron.daily` stopped every seed at the same minute, and a truncated tar kept its final name. | A per-host random sleep staggers the window; the archive is written as `.partial`, verified, hashed, then atomically renamed. |
| 21 | Medium | Nightly CI | Mutable action refs and a regtest bitcoind with `rpcallowip=0.0.0.0/0`. | Actions are pinned to the same SHAs as the main workflow; RPC is allowed only from Docker's private range and published on loopback. |
| 22 | Medium | CI | Tests resolved a fresh `Cargo.lock` while release builds used `release/Cargo.lock`. | The test job copies the release lock and runs with `--locked`. |
| 23 | Medium | Runbook | Documented `peers` and `staking/status` calls return 401 without the API key. | Commands include `X-API-Key` from the service key file. |
| 24 | Medium | Provisioning | `SEED_PEER` was interpolated unquoted into a systemd unit, allowing directive injection. | Only an IPv4 literal is accepted. |
| 25 | Medium | BTC HTLC | Funding and refund used a fixed 1,000-sat fee (about 7 sat/vB) and are not safely replaceable. | The fixed floor is 5,000 sats (about 35 sat/vB). Dynamic fee estimation remains open. |
| 26 | Low | Wallet fees | The P2PKH input estimate assumed a 33-byte pubkey; uncompressed inputs underpaid by 32 bytes per input. | The estimate uses the uncompressed maximum (188 bytes). |
| 27 | Low | TOTP | Verify-only backup preview skipped the wallet-carried replay floor. | A code at or below `last_totp_step` is rejected in verify-only mode too. |
| 28 | Low | Alerts | A transient ntfy failure returned 500 immediately, and critical alerts repeated only every four hours. | The relay retries with backoff; the repeat interval is one hour. |
| 29 | Low | Docs | Consensus docs claimed a 2016-block difficulty adjustment and omitted the real timestamp rules. | The table now states fixed difficulty, 5-second future drift, and 55-second minimum spacing. |
| 30 | High | Fork choice | The exact-image soak produced two equal-height blocks and the fleet split permanently: fork choice only reorganized on strictly greater cumulative work. | A same-height fork with the lexicographically lower tip hash now wins, so all nodes deterministically converge. The first candidate window was discarded and restarted. |
| 31 | High | SPV restart | The SPV chain was seeded with genesis only; a daemon resuming from a stored VTR chain rejected every later header as an unknown parent, including reorg fork headers. | The daemon rebuilds the in-memory SPV chain from the loaded VTR headers before the event bridge starts. The governance reorg boundary test now passes. |
| 32 | High | SPV fork choice | SPV switched its best tip only on strictly greater work, so after an equal-work reorg it kept the abandoned tip and could accept or reject inclusion proofs on the wrong branch. | SPV applies the same lower-hash tie-break; a regression test submits both tips and checks the selected branch. |
| 33 | High | SPV recovery | A dropped event or failed trusted-header insertion left SPV stale; lag reconciliation repaired only the block store, and later headers failed on unknown parents. | A shared `spv_sync` rebuilds the view from the canonical chain at startup, every 30 seconds, after lag reconciliation, and after a failed insertion. |
| 34 | High | Desktop swaps | The desktop started BTC wallet sync but not the BTC swap reconciler, so a taker's automatic VTR claim depended on a manual RPC call. | Both front-ends start the two reconcilers through one shared `spawn_swap_services` function; a test asserts both are started. |
| 35 | High | Swap claims | The automatic claim skipped a recorded claim after a persistence or submission failure, and refused to try during the final hour. A failed BTC scan also blocked the claim attempt. | Recorded claims are re-submitted, the claim is attempted until expiry, and a scan failure no longer suppresses the claim. Regression tests cover all three cases. |
| 36 | High | Fork convergence | Equal-height peers never requested each other's tips, so the tie-break could not run after a partition. | Equal-height handshakes and periodic tip reconciliation request headers. Fork blocks are deliberately not relayed: they are only statelessly validated, and relaying them is a ban/eclipse amplifier. |
| 37 | High | Checkpoints | A future checkpoint blocked reorgs before the node reached it and caused a peer ban for a local policy decision; fork blocks were not checked at the pinned height. | Below a not-yet-reached checkpoint, a reorg is allowed only when the fork point is close enough to remain recoverable within the journal; fork blocks are checked at the pinned height, validation runs first, and local policy rejections do not ban the sender. |
| 38 | High | Receipt accounting | The first receipt and every peer epoch change discarded unsettled downloaded bytes; carrying the remainder then permanently disputed future payments when the peer's reset counter was lower. | The first receipt pays what it acknowledges, an epoch change settles the old receipt first, and only the unacknowledged remainder is dropped because the reset counter cannot attest it. |
| 39 | Medium | Incentive payments | Mempool admission cleared the debt, so an evicted, lost, or reorged transaction left the peer unpaid with no retry. | A broadcast payment stays pending until six confirmations; a shallow reorg keeps it pending, and a missing transaction remains pending rather than being re-paid, because the original can still be mined. |
| 40 | Medium | Incentive spend cap | Default-on payouts had no process budget or per-payment limit. | Added `--torrent-incentive-budget-sats` (10 VTR default) and `--torrent-incentive-payment-cap-sats` (1 VTR default). The cap bounds principal; network fees are additional and are not included. |
| 41 | Medium | Short downloads | A fast download could exit before the periodic receipt exchange, leaving no payable agreement. | A final cumulative receipt is sent before the peer task closes. |
| 42 | Medium | SPV clock | Trusted historical headers were re-checked against the wall clock, so a large backward clock step could stop startup during SPV seeding. | Trusted headers skip the future-time check; untrusted headers still enforce it. Both paths have tests. |
| 43 | High | Fork relay | Relaying every stored fork block let one peer turn cheap, statelessly valid forks into an inv flood that got the relaying node banned by its peers. | Fork relay is removed; competing tips are discovered through equal-height header requests. |
| 44 | High | Checkpoint trap | Allowing any reorg below a future checkpoint could strand a node on a branch more than the journal depth away, with no recovery reorg and repeated bans of honest peers. | A pre-checkpoint reorg is allowed only when the fork point is within `max_reorg_depth` of the checkpoint, so a later return to the pinned branch remains possible. |
| 45 | High | Swap claim conflict | The recorded VTR claim was never re-broadcast when local mempool admission failed, so a maker refund in the taker's mempool could make the claim permanently unrelayable. | The claim is broadcast to peers even when local admission rejects it; the chain check still rejects an already-spent claim. |
| 46 | Medium | Stale claim evidence | A failed BTC scan could authorize a VTR claim from a restored `ClaimConfirming`/`Claimed` observation that a reorg had removed. | Auto-claim requires `Claimed`; a stale observation must reach 100 confirmations, and the wallet-unlock state is re-checked after the scan. |
| 47 | Medium | Preimage recovery | The preimage was persisted only at 6 confirmations, and a non-standard but valid claim witness was classified `SpentElsewhere`. | The preimage is persisted at `ClaimConfirming`, and any true-branch witness element hashing to the lock is accepted. |
| 48 | Medium | Debt persistence | Session eviction and `remove_torrent` could discard unpaid or in-flight incentive debt; debt state is still process-local across restart. | Accounts with unpaid or pending payments are never evicted, and `remove_torrent` refuses while debt is outstanding. Restart persistence remains an operator constraint and is documented. |
| 49 | Medium | SPV install race | A reorg between the header snapshot and the SPV install could briefly re-publish the abandoned branch. | The replacement view is built outside the locks and installed only after re-checking the chain and SPV tips; a mismatch defers to the next tick. |
| 50 | Medium | SPV serving | `getheaders` resolved and emitted from pruned bodies, so equal-height reconciliation could no-op or walk from genesis; pruned bodies also caused an orphan re-fetch loop. | Header lookup and emission use the retained header index; known-header checks treat a pruned body as known. |
| 51 | Medium | Incentive completion | The final receipt could be dropped by the receiver's 30-second per-connection rate limit. | The peer task waits out the remaining window before sending the final receipt. |
| 52 | Medium | Swap fee bump | The VTR claim now signals replaceability and is broadcast when local admission conflicts with a refund, but no automated higher-fee replacement is built. | Residual: a much higher-fee maker refund can still win; the operator must use manual claim tooling if the recorded claim stays stuck. |
| 53 | Medium | Incentive persistence | Debt, pending-payment, and budget state are process-local; a restart loses them. | Sessions with outstanding debt are neither evicted nor removed, and the restart limitation is documented; persistence remains an operator constraint. |

## Follow-up closures

The residual items that could be closed without an operator key, a launch-time
artifact, or a multi-node architecture change were addressed after the first
pass-5 commit:

- **Receipt accounting is cumulative.** Accounts track total bytes uploaded and
  downloaded plus the baselines already settled. A receipt now signs the
  cumulative totals and its account epoch; settlement compares the unpaid delta
  and advances the baselines only by the bytes actually paid for. A missed,
  late, or clock-skewed window no longer permanently disputes an honest claim,
  a lagging peer receipt cannot cause a double payment, and an epoch change
  after a restart realigns both baselines without paying the same bytes twice.
  The receipt domain is now v3; both peers must run this build.
- **Verify-only TOTP checks the account sidecar.** Backup previews now consult
  the read-only account-scoped replay store (and the legacy per-wallet file),
  so a code consumed to unlock or restore any wallet on the host is rejected.
- **Cold-stake registry is append-only.** Each script hash is written as its
  own `0600` file under `wallet.cold_stake.d/`, which makes concurrent GUI/CLI
  registration lossless across processes. The JSON snapshot is still written
  for older readers and backups, but reads merge both sources. Backups include
  the directory, and restore still refuses it for reviewed manual recovery.
- **The container image builds from the repo.** `docker/Dockerfile` now copies
  `release/Cargo.lock` to the name Cargo reads, and
  `docker-compose.build.yml` is an opt-in source-build overlay. The verified
  build is recorded as an image ID/digest; the base stack keeps its pinned soak
  image.
- **A hardened testnet overlay exists.** `docker-compose.hardened.yml` runs the
  nodes as uid 1000 with `cap_drop: ALL`, no-new-privileges, a read-only root
  filesystem, and tmpfs `/tmp`. It is opt-in because the active soak volumes
  are root-owned.
- **CI audits frontend dependencies.** The release build runs
  `pnpm audit --prod --audit-level=high` after the frozen-lockfile install.
- **BTC HTLC fees are operator-adjustable.** The compiled 5,000-sat floor can
  be raised at runtime with `VTORRENT_BTC_HTLC_FEE_SATOSHIS`; claims remain
  RBF-signalling for explicit bumps.
- **The separate-taker path is documented.** The supported external claim flow
  is now spelled out in `docs/atomic-swap-protocol.md`; the automatic claim
  remains a single-node convenience.
- **Taker match import is implemented.** A separate taker can paste the
  maker's match response (or call `POST /api/v1/swap/import-match`) to create
  local swap state. The handler derives the taker address from the unlocked
  wallet, reconstructs the exact VTR HTLC, requires six confirmations, and
  persists the record. The desktop has an import form, and a regression test
  imports a match and submits the taker's VTR claim.
- **Checkpoint enforcement is implemented and persisted.**
  `--checkpoint HEIGHT:HASH` pins a main-chain block, rejects reorgs below it,
  and writes `<data-dir>/checkpoint.json`; `Node::new`/`new_with_chain` reload
  it on later starts, including the desktop node. The launch runbook now
  requires publishing the height-10,000 checkpoint from two agreeing seeds; the
  code side is closed, while the mainnet artifact is necessarily post-launch.
- **CLTV operands are minimal.** New P2CS and VTR HTLC scripts use
  `OP_0`/`OP_n`/minimal pushes for the locktime; the parser still accepts the
  legacy fixed `PUSH4` shape. Timestamp locktimes were already minimal, so this
  removes the zero/small-locktime future-unspendability case. Old nodes do not
  recognize the new minimal shape, so this is part of the coordinated release
  upgrade; legacy scripts remain spendable on the new build.
- **Import is canonical and two-phase.** The order ID is canonicalized before
  lookup, map keys, and persistence, so an uppercase spelling cannot bypass the
  local-term checks. The recovery record is written before memory is updated,
  and both maps are re-checked under their write locks before commit, so a
  concurrent stage advance or a rejected write cannot leave an overwritten
  order behind. The match response also echoes `taker_address` so the desktop
  imports the address the maker actually funded.
- **Receipts separate local and peer epochs.** The epoch this side signs is
  immutable; receiving a peer's epoch updates only the peer baseline, and
  `peer_epoch` is an explicit `Option` so epoch 0 is not a sentinel. The engine
  signs cumulative totals and only enforces settlement-id monotonicity within
  the same epoch. The domain is v3, so pre-fix peers fail closed instead of
  being misread.
- **Checkpoint is persisted and chain-bound.** `--checkpoint` writes
  `<data-dir>/checkpoint.json` with the genesis hash; the file is size-capped,
  symlinks are rejected, a different-genesis file fails startup, and a lower
  CLI pin cannot silently replace a higher one. Production daemon/desktop
  configs load it on later starts; library/test constructors opt out.
- **Release verification is strict and complete.** Supplying a public key now
  requires a signature; a committed `release/minisign.pub` or
  `VTR_REQUIRE_SIGNING=1` without a CI secret fails the release workflow.
  `verify-release.sh` also rejects unlisted files, and the checksum step no
  longer hashes a partially written `SHA256SUMS` into itself.
- **Soak tooling starts a BTC-enabled fleet correctly.** The release-fleet
  script passes `--btc-attest-new-seed` for fresh container-local BTC state.
  `/btc/status` and `/btc/address` use a non-blocking wallet read and fall back
  to the published health snapshot while the sync task holds the wallet write
  lock, so monitoring does not hang during the initial mainnet header sync.
- **Release signing is wired.** If `MINISIGN_SECRET_KEY` is configured, the
  release workflow signs `SHA256SUMS`, verifies it against
  `release/minisign.pub`, and uploads `SHA256SUMS.minisig`. Without the secret,
  releases remain unsigned. `scripts/verify-release.sh` and
  `docs/release-signing.md` cover verification and key custody.

## Not fixed

| Risk | Why it remains |
|---|---|
| A new node without `--checkpoint` still accepts a long-range forged PoS chain from genesis. | Enforcement and `HEIGHT:HASH` plumbing are in place; the actual mainnet checkpoint can only be published after 10,000 blocks. New nodes must be configured with it per the launch runbook. |
| Backups containing cold-stake or swap state are deliberately not auto-restorable. | Recovery requires the reviewed manual runbook; the UI and policy documents must keep saying so. |
| Release builds are unsigned unless the operator configures signing. | The minisign workflow, verification script, and documentation are in place; generating the key, storing it in `MINISIGN_SECRET_KEY`, and obtaining Apple/Windows certificates remain operator key-custody tasks. |
| BTC HTLC fees are a configurable floor, not a live estimate. | `VTORRENT_BTC_HTLC_FEE_SATOSHIS` raises the 35 sat/vB floor without a rebuild; a live estimator and safe funding replacement are still needed. |
| EVM/BTC cross-chain swap promotion on mainnet remains disabled. | The independent review and multi-node adversarial testing are still required. |
| New minimal P2CS/HTLC scripts are not understood by pre-change nodes. | This is a mainnet-from-genesis rule and requires a full fleet upgrade; do not run a mixed fleet while creating or spending cold-stake/swap scripts. |
| The base testnet stack still defaults to a Docker Hub tag; per-node image variables and the source-build overlay are available. | A release must publish the image digest (`@sha256:`) and set `VTORRENT_NODE{1,2,3}_IMAGE` one node at a time so the soak clock is not reset fleet-wide. |
| The base testnet stack still runs as root for the active soak volumes. | A hardened non-root/cap-drop overlay exists for new deployments; migrating the live volumes is an operator change window. |

## Verification

- Focused suites: core 36, script 99, wallet 75, torrent 129, BTC 226, node
  261, SPV 51, P2P 84, RPC 211, Tauri 34, wallet-service 10. Desktop UI: 45
  tests, lint and production build clean.
- Clippy: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  completed clean.
- Full workspace on the final tree: **1,488 passed, 0 failed, 11 ignored**
  across 54 test groups.
- Container source build: `docker build -f docker/Dockerfile` succeeded;
  `vtorrent-daemon 2.0.0` runs as uid `vtorrent`; image
  `sha256:e26c878cade66adf6c54fc5a870b96454992ee17ce35a1d783bd45125715301d`.
- Release verification: `scripts/verify-release.sh` accepts an intact
  checksum directory and rejects a tampered asset.

The consensus timestamp change, the detached dial refactor, the receipt
settlement/payout path, and the SPV pin/reorg handling are AI-authored and
must be explicit items in the independent human review.

The 55-second minimum spacing is a consensus rule. Every node must run this
version before it is treated as the release image; the existing soak fleet
produces blocks ~61 seconds apart, so it is compatible, but a mixed fleet
would reject a block produced below the new floor. The earlier five-second
future window is retained and is now relative to `max(wall, parent)`.
