# External Security Review — Scope & Brief

Status: BRIEF (to be sent to a review firm / independent auditors).
Purpose: commission the external security review that gates mainnet. This is
the one launch item that needs outside resources.

## 1. What vTorrent is

A ground-up Rust rewrite of the vTorrent (VTR) client: a Proof-of-Stake
blockchain that integrates BitTorrent seeding incentives, a built-in
atomic-swap DEX (VTR↔BTC), and legacy wallet recovery. Desktop UI is React +
Tauri. 19 Rust crates + a frontend; ~914 workspace tests.

## 2. Why now

The three-node regtest fleet completed a **full 7-day soak** (2026-10-06) and
`v2.0.0-beta.3` is released. The remaining launch gate is an **independent
security review** of the custody and consensus surfaces. A consensus bug forks
the chain; a wallet bug loses funds — both are catastrophic and neither is
caught by the project's own tests.

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

### 3.2 Consensus batch (new, unsoaked — forks the chain)
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

### 3.3 Atomic-swap DEX (cross-chain funds at risk)
- `vtorrent-node/src/atomic_swap.rs`, `vtorrent-btc/src/htlc.rs` — HTLC
  construction, timing invariants (the preimage-reveal vs refund ordering),
  preimage handling, refund/claim paths, reorg handling.
- Known accepted risk: BTC refunds are deliberately non-RBF (documented
  interlock) — review the reasoning, not just the code.

### 3.4 Network & RPC boundary
- `vtorrent-rpc/src/server.rs` — API-key auth (constant-time compare),
  rate limiting, the auth-before-limiter ordering.
- `vtorrent-p2p` — message codec, ban manager, PEX, compact-block relay,
  the `strict_onion` clearnet-refusal path.
- `vtorrent-torrent` — tracker SSRF guards, MSE/PE (if implemented),
  incentive accounting (self-reported vs signed receipts).

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
- Source at `v2.0.0-beta.3` (tag) + the consensus batch on `main`.
- `docs/` — designs + adversarial reviews for every subsystem
  (`cold-staking-p2cs-design.md`, `op-return-utxo-exclusion-design.md`,
  `network-upgrade-design.md`, `atomic-swap-protocol.md`,
  `memory-observability-design.md`, the code-review fix-status docs).
- A regtest fleet + a `docker/testnet/docker-compose.yml` for reproduction.
- `docs/oncall-runbook.md` for operational context.

## 7. Suggested engagement
- **Phase 1 (2–3 weeks):** wallet encryption + consensus batch (the two
  catastrophic surfaces).
- **Phase 2:** swap DEX + network/RPC + incentive economics.
- Findings triaged into `docs/mainnet-readiness.md`; fixes with regression tests
  before the mainnet tag.

## 8. References
- `docs/mainnet-readiness.md` (launch checklist), `docs/soak-log.md` (soak).
- `docs/cold-staking-p2cs-design.md`, `docs/op-return-utxo-exclusion-design.md`,
  `docs/network-upgrade-design.md`, `docs/atomic-swap-protocol.md`.
- `docs/code-review-2026-09-20-fix-status.md` (internal reviews, all closed).
