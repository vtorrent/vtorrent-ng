# External Security Review — Scope & Brief

Status: BRIEF (to be sent to a review firm / independent auditors).
Purpose: commission the external security review that gates mainnet. This is
the one launch item that needs outside resources.

## 1. What vTorrent is

A ground-up Rust rewrite of the vTorrent (VTR) client: a Proof-of-Stake
blockchain that integrates BitTorrent seeding incentives, a built-in
atomic-swap DEX (VTR↔BTC), and legacy wallet recovery. Desktop UI is React +
Tauri. 19 Rust crates + a frontend; 900+ workspace tests.

## 2. Why now

The three-node regtest fleet completed a **full 7-day soak** (2026-10-06) and
`v2.0.0-beta.3` is released. Independent security review gates governance and
mainnet activation. The historical soak predates the later governance and
proof-service repairs and does not validate the current implementation.
Coordinated upgrades and a fresh soak remain part of launch readiness.
Internal tests caught prior custody and consensus flaws. Reviewers should
challenge the repairs and their remaining assumptions independently.

## 3. Scope (in priority order)

### 3.1 Wallet encryption & key handling (highest — funds at rest)
- `vtorrent-wallet/src/encryption.rs` — Argon2id (m=65536, t=3, p=4) +
  ChaCha20-Poly1305; format `[version][salt(32)][nonce(12)][ct+tag]`.
  - **Nonce reuse** across re-encrypt / passphrase change (must be fresh).
  - Salt/nonce randomness; KDF parameter adequacy; AEAD usage.
  - Key zeroization (`zeroize`) on drop and across the FFI boundary.
- `vtorrent-wallet/src/wallet.rs` — key storage, WIF handling, TOTP (`otp.rs`),
  atomic save (0600, tmp+rename), HD (BIP39) derivation.
- `vtorrent-migrate` — legacy `wallet.dat` parsing (BerkeleyDB, AES-256-CBC,
  OTP chain). Prior finding: OTP secret is passphrase-recoverable offline.

### 3.2 Consensus batch (new, fresh soak in progress — forks the chain)
- **Cold staking (P2CS)** — `vtorrent-script/src/standard.rs` (`build_p2cs`,
  classifier), `vtorrent-node/src/chain/chain_reorg.rs` (the **R1 coinstake
  re-lock rule**), `vtorrent-node/src/staking.rs` (coinstake signing),
  `vtorrent-spv/src/spv_chain.rs` (stake-proof verification).
  - **Can the hot staking key ever redirect funds?** (the R1 rule is the whole
    security claim — try to break it).
  - Script-engine correctness for the P2CS branches (IF/ELSE, CLTV).
  - SPV proof: can a forged P2CS proof pass?
- **OP_RETURN UTXO exclusion** — `vtorrent-node/src/block.rs`
  (`is_utxo_eligible`), `chain_reorg.rs`, `staking.rs`. Verify the chain and the
  producer agree (a mismatch forks).
- **BIP-9 versionbits** — `vtorrent-node/src/deployments.rs`. Activation
  determinism; can a deployment be forced/blocked?

### 3.2b On-chain governance (new, inactive — forks the chain when activated)
- `vtorrent-node/src/governance.rs` — `SignedVote` (recoverable ECDSA over a
  domain-separated digest bound to the genesis hash), vote uniqueness per
  outpoint, vote locks, deterministic activation, and compact undo records.
  - **Can a vote be forged or replayed?** Can voting power be counted twice,
    including across a stake transfer?
  - **Can a locked stake be spent or redirected** before its unlock height?
  - **Is activation deterministic** across nodes (no map-iteration-order or
    timestamp divergence), and can a parameter change remove all eligible stake
    or otherwise halt the chain?
- `vtorrent-node/src/chain/chain_reorg.rs` — governance processing during block
  application, lock enforcement, the stake-lock transfer rule, and journal
  restore on rejection and reorg.
  - Do the undo journals restore the exact pre-state across a rejected block and
    a failed or successful reorg?
- `vtorrent-node/src/chain.rs` (`select_block_transactions`,
  `next_block_params`) and `vtorrent-node/src/node/staking_loop.rs` — the
  block-assembly filter that defers votes for unrecorded proposals.
  - Does the filter ever exclude a valid transaction or include one the block
    would reject?
  - Are rejected candidates atomic? Do selection and validation agree about
    activations, the coinstake, preceding spends, and block size?
- `vtorrent-node/src/deployments.rs` — the governance deployment gate.
- Deposit handling: the proposal deposit is an OP_RETURN carrier, burned on
  pass and failure. Verify it cannot be reclaimed and that supply accounting is
  correct.

### 3.3 Atomic-swap DEX (cross-chain funds at risk)
- `vtorrent-node/src/atomic_swap.rs`, `vtorrent-btc/src/htlc.rs` — HTLC
  construction, timing invariants (the preimage-reveal vs refund ordering),
  preimage handling, refund/claim paths, reorg handling.
- Known accepted risk: BTC refunds are deliberately non-RBF (documented
  interlock) — review the reasoning, not just the code.
- Evidence: the full HTLC swap now settles against real BTC SPV
  (`docs/btc-spv-multinode-reorg-test.md`). The test uses one VTR daemon;
  separate maker and taker VTR nodes remain open.
- `vtorrent-btc/src/headers.rs` — header retention. Pruning is amortized with a
  1,024-header slack. The mainnet-scale test inserts 870,000 headers and keeps
  the retained store within 100,000 plus the slack. Review the retention bound
  and the difficulty and scan contiguity assumptions. Wallet scans clamp to the
  retained floor; `--btc-birthday` sets an explicit creation height for older
  funds.

### 3.4 Network & RPC boundary
- `vtorrent-rpc/src/server.rs` — API-key auth (constant-time compare),
  rate limiting, the auth-before-limiter ordering.
- `vtorrent-p2p` — message codec, ban manager, PEX, compact-block relay,
  the `strict_onion` clearnet-refusal path.
- `vtorrent-torrent` — tracker SSRF guards, MSE/PE (if implemented),
  incentive accounting (self-reported vs signed receipts).

### 3.4b Canonical stake-proof service and resource limits
- `vtorrent-node/src/chain/stake_proof.rs`: canonical target selection,
  retained undo history, snapshot capture, parent commitments, and exact
  coinstake, outpoint, and Merkle binding.
  - Can a valid membership path for the wrong coin or transaction pass?
  - Does reconstruction restore the exact pre-state without changing the
    live chain? What happens outside contiguous retained history?
- `vtorrent-node/src/node/proof_service.rs`: reconstruction on a blocking
  worker, cache-hit behavior, and the 60-second sliding admission windows:
  100 cold attempts per source IP and 200 process-wide.
  - Can IP aliases, reconnects, rotating addresses, or cache churn bypass the
    limits or grow bookkeeping without bound?
  - Are ineligible targets rejected before expensive work? The limits count
    attempts, not CPU milliseconds. The full UTXO copy still holds the chain
    mutex and must remain in the availability review scope.
- `vtorrent-node/src/node/mod.rs` and `vtorrent-node/src/node/handler.rs`: final live-chain
  canonicality and proof checks, cache insertion ordering, bounded proof
  decoding, and full-block/reorg proof requests.
  - Can a reorg between capture and insertion cache or serve an abandoned
    proof? Can an invalid relay evict a valid canonical cached proof?
- `vtorrent-node/src/node/proof_retry.rs`: at most 100 pending recent
  targets, four total attempts, a five-minute lifetime, 65-second spacing,
  and at most eight retries per maintenance batch.
  - Can unsolicited or late unavailable responses create work or extend its
    lifetime? Can peer loss, version changes, reorgs, or cache fulfillment
    leave stale requests or amplify traffic?
- `vtorrent-rpc/src/handlers/mod.rs`: full-node and SPV header lookup and
  cache-only proof responses. Check the intended RPC behavior for fork hashes
  as well as the P2P service's canonical-only policy.
- Full-node proof transport is distinct from SPV PoS header acceptance,
  which remains fail-closed. Governance remains inactive and unscheduled.

### 3.5 Torrent incentive economics
- `vtorrent-torrent/src/incentive.rs` + `vtorrent-core/src/receipt.rs` — can a
  peer inflate earnings or replay a receipt? (Signed receipts are the fix; test
  them.)

## 4. Out of scope
- The React/Tauri frontend (unless it handles key material — it must not).
- Performance/DoS beyond the listed boundaries.
- The legacy 1.x C++ codebase.

## 5. Deliverables requested
- Findings with severity, reproduction, and a concrete fix.
- Explicit verdict on the **R1 cold-staking rule** and the **wallet encryption**.
- A short "residual risk" section for anything accepted.

## 6. What we provide
- Immutable review delivery:
  **`70ea7b9dfb3c65dcfb30cb03ec8c168c5d3cbffd`**
  (`fix(ui): label remaining icon-only controls`). This
  is `bab2456` plus the Playwright browser E2E setup, the live cold-stake
  browser test, the Vite dev-origin CORS allowance, the Tauri path capability
  fix, the durable cold-stake registry persistence fix, the desktop
  restart/persistence parity fixes, the wallet-path confinement, import
  overwrite guard, version bump, IPC error surfacing, and staking ownership
  warnings, and the live desktop journey, restart, and BTC discovery evidence.
  **Backup/2FA threat model:** 2FA gates app unlock. It is not an encryption
  factor for the wallet file or backup. Both are encrypted with the wallet
  passphrase; an attacker with the passphrase can decrypt them without the OTP.
  Use a strong passphrase and keep backups offline.

  The frontier-model review's ten findings and three follow-up concerns are addressed across `61f4be4`, `cb765ce`, `f272e64`, `3975693`, `6f82b63`, `07a6881`, `326ce27`, `39da204`, `afe5974`, and `2287026`; verified 2FA enrollment, the torrent control availability, the reviewed send quote, verified encrypted backup and restore, the guided swap workspace, the staking readiness assistant, the activity and recovery center, the cold-stake delegation wizard, node settings and diagnostics, torrent controls and reward accounting, and navigation grouping and accessibility improvements, the external review remediation, the activity journal, and the icon-label audit are in `70ea7b9`. The external review should verify them.
  This commit includes the governance template-selection repairs,
  snapshot-based proof reconstruction, admission budget, bounded retries, the
  real-time quota-recovery test, the real-bitcoind SPV reorg tests, the full
  HTLC swap settlement test, the reorged-out funding test, the independent
  genesis verifier, the amortized BTC header retention fix, the stakeable
  coinstake and governance snapshot fixes, the desktop broadcast and BTC
  overflow fixes, the DEX and torrent availability fixes, the sensitive-route
  auth and P2P key-normalization fixes, the encrypted-wallet restart and
  legacy-extraction tests, the whole-codebase review remediation (bootstrap
  maturity, BTC retention and scan floor, torrent assembler, legacy key forms,
  alert metrics, soak evidence, compact-block wire budget, frontend IPC
  normalization, WebSocket auth, CI benchmark baselines, the BTC wallet
  birthday flag, the desktop governance/earnings/payment/deadline commands,
  desktop health-field updates, the shared BTC sync loop, and desktop chain
  persistence), and the evidence documents listed below.
  Use this full commit hash rather than floating `main`. If the engagement
  moves to a later implementation, record its full hash explicitly.
  **Post-review mainnet fixes:** `1901bb9` fixes two-peer compact-filter header
  agreement for a fresh mainnet BTC SPV chain and separates authentication from
  the filter download. `513efde` requires distinct network groups, tries the
  configured peer first, and fails over on header or filter-download errors.
  `c465d1b` keeps five mainnet peers in distinct groups and retries failed
  scans in-cycle. `7ec3b2f` adds BTC SPV scan metrics and a stale-scan alert.
  Include all four deltas in the delivery; see `docs/btc-spv-soak-plan.md`.
- The review delivery is a source snapshot, not a release tag. The core soak
  fleet runs `vtorrent/node:batch-e7691fb` (code `e7691fb`, BTC SPV disabled)
  from 2026-10-21T02:47:54Z to 2026-10-28T02:47:54Z. The review candidate
  `70ea7b9` adds the BTC wallet birthday flag, the desktop
  governance/earnings/payment/deadline commands, health-field updates, the
  shared BTC sync loop, desktop chain persistence, and the cold-stake, BTC
  scan, compact-block, and desktop hardening from the adversarial review,
  raw-script cold-stake funding, and the round-2 burn-address removal,
  session-bound funding, desktop cold-stake commands, network-scoped BTC
  state, output-size fee fix, single-use WebSocket tokens, the BTC scan
  checkpoint-retention fix, reorg-safe BTC wallet state, the durable cold-stake
  script registry, the WebSocket token cap, the Playwright browser E2E setup,
  the live cold-stake browser test, the Vite dev-origin CORS allowance, the
  Tauri path capability fix, the durable cold-stake registry persistence fix,
  the desktop restart/persistence parity fixes, the wallet-path confinement,
  import overwrite guard, version bump, IPC error surfacing, and staking
  ownership warnings, and the live desktop journey, restart, and BTC discovery
  evidence; rebuild the release image from it. The soak is supporting evidence, not a substitute for this review.
- `docs/` — designs + adversarial reviews for every subsystem
  (`cold-staking-p2cs-design.md`, `op-return-utxo-exclusion-design.md`,
  `network-upgrade-design.md`, `atomic-swap-protocol.md`,
  `memory-observability-design.md`, the code-review fix-status docs).
- **Internal security review** (`docs/security-review-internal-2026-10-09.md`)
  with three findings fixed: the P2CS R1 rule allowed stake theft, SPV did not
  enforce R1, and six endpoints shipped unrouted. Verify the fixes, do not
  assume them.
- **Governance self-review and repair**
  (`docs/governance-step3-self-review-2026-10-11.md`,
  `docs/governance-preactivation-validation-2026-10-12.md`,
  `docs/governance-wholenode-load-test.md`): the original implementation had
  unauthenticated votes, duplicate counting, nondeterministic activation, and
  broken reorg restore. The repair is tested; review it adversarially.
- A regtest fleet + a `docker/testnet/docker-compose.yml` for reproduction.
- `docs/oncall-runbook.md` for operational context.

## 7. Suggested engagement
- **Phase 1 (2–3 weeks):** wallet encryption + consensus batch + governance
  (the catastrophic surfaces).
- **Phase 2:** swap DEX + network/RPC + incentive economics.
- Findings triaged into `docs/mainnet-readiness.md`; fixes with regression tests
  before the mainnet tag.

## 8. References
- `docs/mainnet-readiness.md` (launch checklist), `docs/soak-log.md` (soak).
- `docs/cold-staking-p2cs-design.md`, `docs/op-return-utxo-exclusion-design.md`,
  `docs/network-upgrade-design.md`, `docs/atomic-swap-protocol.md`,
  `docs/governance-step3-design.md` (approved policy).
- `docs/security-review-internal-2026-10-09.md` (internal review, findings
  fixed).
- `docs/governance-step3-self-review-2026-10-11.md` and
  `docs/governance-preactivation-validation-2026-10-12.md` (governance
  self-review and repair).
- `docs/governance-reorg-load-test.md`,
  `docs/governance-pos-fork-proof-test.md`, and
  `docs/governance-interrupted-reorg-test.md` (live forks and crash recovery).
- `docs/governance-proof-request-cost.md` and
  `docs/governance-proof-snapshot-reconstruction.md` (request cost and lock
  ownership measurements).
- `docs/governance-proof-reconstruction-budget.md`,
  `docs/governance-proof-retries.md`, and
  `docs/governance-proof-quota-recovery.md` (resource policy, retry bounds,
  and real-time end-to-end evidence).
- `docs/btc-spv-multinode-reorg-test.md` (two independent daemons on a real
  regtest bitcoind: header agreement, UTXO discovery, restart persistence,
  reorg re-inclusion, a full HTLC swap that settles against real BTC SPV, and
  a conflicting-spend test that drops a reorged-out funding output). Separate
  maker and taker VTR nodes remain open.
- `scripts/verify-genesis.py` (independent snapshot and genesis hash verifier;
  CI runs it after the workspace tests).
- `docs/code-review-2026-09-20-fix-status.md` (internal reviews, all closed).
