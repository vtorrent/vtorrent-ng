# Pass-13 Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (native) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the pass-12 release-critical code defects and the review-package/evidence gaps, verify the new candidate, and start a new exact-image window.

**Architecture:** Keep the existing architecture. Add one shared block-request validator used by the seeder and engine upload paths; make the removal guard consult the live incentive journal and the pending-owed predicate; make the stake selector structural rather than age-based; initialize BTC state on legacy import and bind the UI to the backend default key; then harden the seeder/network and the release evidence.

**Tech Stack:** Rust 2021 workspace, Tokio, serde/bincode, Axum RPC, Tauri 2 + React/TypeScript, Docker, GitHub Actions.

**Spec:** `docs/2026-11-24_ai-supplementary-review-pass12.md`

## Global Constraints

- Keep genesis and consensus compatibility; do not change block/transaction encodings or reward math.
- Keep atomic swaps enabled; do not disable the swap paths.
- Every desktop VTR send requires the wallet passphrase and a fresh TOTP when 2FA is enabled.
- Preserve legacy BTC state; fail closed when ownership/history is uncertain.
- No production release key or signing certificate is generated without operator authorization.
- AI review does not satisfy the independent human security-review gate.
- The exact-image 168-hour soak is a gate; the current `review-e091936` window is superseded by any code change.
- Pin bincode to `=1.3.3`; the unmaintained advisory remains accepted.
- Do not modify the production `vtr-node*` soak fleet.
- `release/Cargo.lock` is the tracked release lock; the root `Cargo.lock` is gitignored.

## Review Focus

- **1-byte/unaligned `Request` frames** must be rejected before any disk access.
- A settled-but-unpaid **live journal debt** must block removal even when the live session account has been zeroed.
- A **receipt-grace or never-receipted** downloader must not be deleted or evicted with positive `pending_owed`.
- An **immature structural stake coin** must not be spent by the automatic incentive payment.
- **Legacy import** must either create the BTC state and a controllable default address or fail clearly without reporting success.

---

### Task 1: Reject non-canonical block requests

**Files:**
- Modify: `vtorrent-torrent/src/peer_wire.rs` (add `BLOCK_SIZE`/`valid_block_request`)
- Modify: `vtorrent-torrent/src/seed.rs:865-937` (request path)
- Modify: `vtorrent-torrent/src/engine.rs:1251-1357` (upload path)
- Modify: `vtorrent-torrent/src/limits.rs` and the seed/engine counters (remove dead per-peer cap or make it real)
- Test: `vtorrent-torrent/src/peer_wire.rs` tests; `seed.rs`/`engine.rs` regression tests

**Interfaces:**
- Produces: `pub const BLOCK_SIZE: u32 = 16 * 1024;`
- Produces: `pub fn valid_block_request(metainfo: &Metainfo, index: u32, begin: u32, length: u32) -> bool`
- The function returns true only when `index < piece_count`, `begin < piece_len`, `begin % BLOCK_SIZE == 0`, and `length == min(BLOCK_SIZE, piece_len - begin)`.

- [ ] **Step 1: Write failing tests** for `(0,0,16384)=true`, `(0,1,16384)=false`, `(0,0,1)=false`, final short block, `index==piece_count=false`, and a request crossing the piece end.
- [ ] **Step 2: Run the test** and confirm it fails because `valid_block_request` does not exist.
- [ ] **Step 3: Implement** `valid_block_request` in `peer_wire.rs` using `metainfo.piece_length`, `metainfo.total_size`, and `metainfo.piece_count`.
- [ ] **Step 4: Use it** in both `serve_connection` and the engine upload handler before `FileLayout`/`read_block_from_disk`; ignore invalid frames without touching disk. Remove the dead `MAX_OUTSTANDING_PER_PEER` machinery or replace it with a real bound.
- [ ] **Step 5: Run the focused tests**, then commit.

### Task 2: Make removal/eviction ledger- and pending-aware

**Files:**
- Modify: `vtorrent-torrent/src/incentive.rs` (`PeerBandwidthAccount::blocks_removal`/pending)
- Modify: `vtorrent-torrent/src/session.rs:295-309` (`has_outstanding_incentives`; `add_session` eviction)
- Modify: `vtorrent-daemon/src/incentive_ledger.rs` (dust/retain/write-off)
- Modify: `vtorrent-daemon/src/main.rs:1264-1300` (guard installation/order; journal)
- Modify: `vtorrent-rpc/src/handlers/torrent.rs:253-281` (guard order/error)
- Modify: `vtorrent-tauri/src/commands/torrent.rs:288-314` (guard before cancel)
- Test: `session.rs`, `incentive_ledger.rs`, `handlers/torrent.rs`, Tauri tests

**Interfaces:**
- `PeerBandwidthAccount::blocks_removal(&self) -> bool` includes `unpaid >= MIN_PAYMENT_SATS`, `payment_in_flight`, `pending_payment`, `agreed_owed >= MIN_PAYMENT_SATS`, or `calculate_owed() >= MIN_PAYMENT_SATS`.
- `SessionManager::has_outstanding_incentives` uses `blocks_removal`; `add_session` eviction uses the same predicate.
- The guard checks the live `IncentiveJournal` accounts for the session's info hash as well as the durable ledger; the journal is installed before/with the guard.
- Missing/unreadable ledger is generic/fail-closed; path is logged only server-side.
- Tauri checks the guard before removing/cancelling the token.

- [ ] **Step 1: Write failing tests** for a positive sub-minimum receipt (must remain removable), an un-receipted `calculate_owed` debt (must block), a live-journal debt with a zero session account (must block), a dust ledger entry (must not block/retain forever), and Tauri refused removal leaving the cancel token.
- [ ] **Step 2: Run the tests** and confirm the intended failures.
- [ ] **Step 3: Implement** the account predicate, journal-backed guard, dust/retain policy, error handling, and Tauri ordering.
- [ ] **Step 4: Run the focused tests**; add a daemon-level journal/guard test; commit.

### Task 3: Structural stake reserve

**Files:**
- Modify: `vtorrent-wallet-service/src/lib.rs:152-254` (`prepare_incentive_payment`; selector)
- Modify: `vtorrent-wallet-service/src/lib.rs` manual `build_payment`/`build_payment_to_script` callers as needed
- Modify: `vtorrent-rpc/src/handlers/{wallet,dex,swap}.rs`, `vtorrent-tauri/src/commands/{wallet,swap,staking}.rs` to use the reserve-aware builder for manual send/DEX/swap/cold-stake
- Test: `vtorrent-wallet-service` tests; RPC/Tauri payment tests

**Interfaces:**
- `is_stakeable_with` (structural) drives the reserve regardless of age.
- The selector prefers coins that cannot stake (below minimum/non-P2PKH); it reserves the largest structurally stakeable outpoint; it fails closed with `StakeReserved` if only the reserve remains.
- Manual paths share the same selector or document an explicit override.

- [ ] **Step 1: Write failing tests** for immature 200 VTR + sufficient 0.5 VTR (must spend 0.5), single UTXO (must fail closed), multiple eligible (must reserve the largest), and manual send/cold-stake.
- [ ] **Step 2: Run the tests** and confirm failure.
- [ ] **Step 3: Implement** the structural selector and call-site changes.
- [ ] **Step 4: Run focused tests**; commit.

### Task 4: Legacy import BTC state and default address

**Files:**
- Modify: `vtorrent-tauri/src/commands/wallet.rs:425-440` (import)
- Modify: `vtorrent-tauri/src/commands/node.rs:269-298` (start/state)
- Modify: `vtorrent-tauri/src/commands/wallet.rs:77-80` (`authorize_wallet`/default)
- Modify: `vtorrent-ui/src/hooks/useWallet.tsx:379-398` (default address/error)
- Modify: `vtorrent-wallet/src/wallet.rs:210-230,513-515` if the default-address contract changes
- Test: Tauri wallet/import tests; desktop E2E

**Interfaces:**
- `import_legacy_wallet` creates the attested BTC states for the generated default key, or returns a blocking error that the UI surfaces; it does not report success if `start_node` cannot run.
- `ImportResult` returns the backend default address; the UI uses it; DEX/swap/claim use the matching signing key.

- [ ] **Step 1: Write failing tests** for import→start and multi-key imported DEX/swap/claim.
- [ ] **Step 2: Run the tests** and confirm failure.
- [ ] **Step 3: Implement** state initialization/default-address binding.
- [ ] **Step 4: Run focused tests**; add the desktop import E2E; commit.

### Task 5: Seeder/network hardening

**Files:**
- Modify: `vtorrent-torrent/src/seed.rs` (`connection_ip_key`; `retain_sessions`; trusted resume verification)
- Modify: `vtorrent-torrent/src/engine.rs` (watchdog progress)
- Modify: `vtorrent-node/src/staking.rs:91-126` (below-minimum status)
- Test: `seed.rs`, `engine.rs`, `staking.rs` tests

- [ ] **Step 1: Write failing tests** for IPv4-mapped addresses, concurrent register/prune, slow-block watchdog, trusted-bit corrupt file, and sub-minimum staking status.
- [ ] **Step 2: Run the tests** and confirm failure.
- [ ] **Step 3: Implement** the fixes; update the trusted-resume docs or verify in a bounded pass.
- [ ] **Step 4: Run focused tests**; commit.

### Task 6: Review package, evidence, and release gates

**Files:**
- Modify: `docs/security-review-request-2026-10-27.md`, `docs/security-review-cover-note.md`
- Modify: `docs/2026-11-23_ai-supplementary-review-pass11.md`, `docs/soak-log.md`, `docs/mainnet-readiness.md`, `docs/mainnet-launch-runbook.md`
- Modify: `scripts/soak-observe-candidate.sh`, `.github/workflows/{build,release-checksums}.yml`, `docker/Dockerfile`, `.cargo/audit.toml` if added
- Evidence: `soak-e091936/ci/` canonical run/audit/build logs

- [ ] **Step 1: Fix the code-freeze check** to `git diff --name-only <candidate>..HEAD -- . ':(exclude)docs/**'` and include lock/UI/config/workflow paths; fix the recipe to `cp release/Cargo.lock Cargo.lock && cargo test --locked ...` and list GTK/WebKit prerequisites.
- [ ] **Step 2: Correct the evidence** — the 1,000-sat fee, the ledger-guard test status, the exact image/CI archive, the 5-warning reachability table, the 168 h coverage/alert, reproducibility, signing trust boundary, rollback drill, launch runbook, and `syn` note.
- [ ] **Step 3: Add the canonical CI/audit/build output** to `soak-e091936/ci/`.
- [ ] **Step 4: Commit the docs/evidence changes.**

### Task 7: Deterministic two-peer test

**Files:**
- Add: `vtorrent-torrent/tests/two_peer_split.rs` (or equivalent)
- Use: a partial-bitfield mock + two peer tasks; assert both peers serve at least one piece and the assembled SHA-1 matches.

- [ ] **Step 1: Write the failing test** with one peer advertising only the first half and one only the second; assert both contribute and the hash matches.
- [ ] **Step 2: Run it**; confirm failure if the scheduler cannot split.
- [ ] **Step 3: Fix any scheduler issue found.**
- [ ] **Step 4: Run it**; commit.

### Task 8: Verify and replace the window

- [ ] **Step 1:** `cargo fmt`, Clippy `-D warnings`, full `cargo test --workspace --all-features`.
- [ ] **Step 2:** Build the image from the clean commit; extract the binary; run both E2Es with `VTORRENT_E2E_DAEMON_BIN`/`SHA256`.
- [ ] **Step 3:** Commit the code and docs; tag the image with the commit; write the pass-13 report/artifact/patch.
- [ ] **Step 4:** Retire `review-e091936`; start `review-<newcommit>`; run the live transfer/payment/removal proof; start the new 168 h window and cron; push upstream; confirm canonical CI.
