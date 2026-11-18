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
  The receipt domain is now v2; both peers must run this build.
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

## Not fixed

| Risk | Why it remains |
|---|---|
| A genuinely separate taker still has no locally persisted `SwapState`, so the automated VTR claim cannot sign on the taker's node. | The supported path is now explicit: `/swap/vtr-claim-external` with the maker's match terms and the observed preimage, documented in `docs/atomic-swap-protocol.md`. Full automation needs taker-side match import and an adversarial two-node test. |
| A long-range forged PoS chain from genesis is still accepted by a new node. | Minimum spacing caps production rate but does not replace a checkpoint. A launch checkpoint or finalized header commitment is still needed. |
| P2CS/HTLC CLTV operands are non-minimal for small locktimes. | Changing the script shape is a consensus/script-version change; the current engine accepts them. |
| Backups containing cold-stake or swap state are deliberately not auto-restorable. | Recovery requires the reviewed manual runbook; the UI and policy documents must keep saying so. |
| Release builds are unsigned and `SHA256SUMS` is integrity-only. | Signing needs operator key custody (Apple/Windows/minisign), deferred by the operator. |
| BTC HTLC fees are a configurable floor, not a live estimate. | `VTORRENT_BTC_HTLC_FEE_SATOSHIS` raises the 35 sat/vB floor without a rebuild; a live estimator and safe funding replacement are still needed. |
| EVM/BTC cross-chain swap promotion on mainnet remains disabled. | The independent review and multi-node adversarial testing are still required. |
| The base testnet stack still pins a Docker Hub tag; the source-build overlay is opt-in. | A release must publish and pin the image digest (`@sha256:`) so operators verify the exact artifact they run. |
| The base testnet stack still runs as root for the active soak volumes. | A hardened non-root/cap-drop overlay exists for new deployments; migrating the live volumes is an operator change window. |

## Verification

- Focused suites: core 36, wallet 73, torrent 118, BTC 225, node 250, SPV 45,
  P2P 84, RPC 202, Tauri 33, wallet-service 10.
- Clippy: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  completed clean.
- Full workspace on the final tree: **1,450 passed, 0 failed, 11 ignored**
  across 54 test groups.
- Container source build: `docker build -f docker/Dockerfile` succeeded;
  `vtorrent-daemon 2.0.0` runs as uid `vtorrent`; image
  `sha256:13ce3ce3891371cf208e7ad41828f3f1088b2cd335a69f8a269021f74c847f7e`.

The consensus timestamp change, the detached dial refactor, the receipt
settlement/payout path, and the SPV pin/reorg handling are AI-authored and
must be explicit items in the independent human review.

The 55-second minimum spacing is a consensus rule. Every node must run this
version before it is treated as the release image; the existing soak fleet
produces blocks ~61 seconds apart, so it is compatible, but a mixed fleet
would reject a block produced below the new floor. The earlier five-second
future window is retained and is now relative to `max(wall, parent)`.
