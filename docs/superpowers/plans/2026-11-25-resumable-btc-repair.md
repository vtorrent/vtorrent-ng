# Resumable BTC repair implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resume a desktop repair after process interruption without resetting wallet history.

**Architecture:** Prepare four files in a private staging directory on the wallet filesystem. Publish a durable intent record before installing any file at its final name. Use stable file identity and a digest to distinguish an installed artifact from an unrelated file with identical contents.

**Tech stack:** Rust 2021, `std::fs`, existing wallet file locks, serde, SHA-256, Tauri command tests.

**Spec:** [Approved recovery contract](../specs/2026-11-25-resumable-btc-repair-design.md).

## Global constraints

- Develop in an isolated worktree. Do not deploy to `review-be78829` or use a real wallet.
- Keep the IPC signature, passphrase, configured TOTP, and explicit attestation on every attempt. Persist no seed or credential.
- Mainnet and regtest only. Unknown existing state requires assisted recovery. Never delete it or relax the existing hard-link and missing-state checks.

## Review focus

- Identical empty files from different operations are not interchangeable (task 1).
- A crash can follow publication of a directory entry but precede durable cleanup (tasks 1 and 3).
- Startup and wallet switching can race with repair (task 2).
- A marker without state may mean a previously used wallet lost its file (tasks 1 and 2).
- Filesystem support differs on Windows, Linux, and macOS; unsupported publication must fail without overwriting (task 3).

### Task 1: Implement durable publication in `vtorrent-btc`

**Files:** Create `vtorrent-btc/src/repair.rs`; register it in `vtorrent-btc/src/lib.rs`. Reuse state serialization, marker bytes, and locking from `vtorrent-btc/src/utxo.rs`.

**Interface:** `RepairPaths::new(mainnet: PathBuf, regtest: PathBuf) -> io::Result<RepairPaths>` validates a common wallet directory and distinct expected network paths. `RepairPaths::run(&self, wallet_id: &[u8; 32]) -> io::Result<()>` performs fresh initialization or authenticated-caller resume. `RepairPaths::ensure_idle(&self) -> io::Result<()>` refuses any pending or unreadable intent. Derive all target paths locally; never accept paths from the record.

- [ ] Write `repair::tests::preserves_unowned_files`: assert `run().is_err()` and unchanged bytes for legacy partial state, an orphan marker, malformed intent, wrong wallet ID, changed content, and an identical-content replacement with different file identity. Run `cargo test -p vtorrent-btc repair::tests` and confirm the new behavior is missing.
- [ ] Under both network writer locks (mainnet first), preflight both targets and markers before staging. Reject legacy names under the locks as well. Use a private, unique staging directory; an interruption before durable intent leaves no final files and no authority to adopt abandoned staging files.
- [ ] Serialize version, wallet fingerprint, operation ID, network roles, stable file identifiers and digests into a no-clobber intent. Sync staged files and directory entries before making the intent durable. Use single-link, same-volume, no-replace moves for installation: a crash must leave either the staged file or the matching final file. Do not use ordinary Unix `rename` over a destination or retain hard-link aliases (`utxo.rs:73–84` rejects them). Verify platform primitives on the supported targets before selecting a dependency.
- [ ] Before any resume write, validate the entire set: each artifact must exist in exactly one expected location, be a regular file, and match recorded identity and bytes. Reject conflicts without advancing the other network. Sync final entries before durably retiring intent; leftover empty staging directories must not authorize another repair.
- [ ] Run `cargo test -p vtorrent-btc repair::tests` to green, then commit the storage change.

### Task 2: Bind repair and startup to the same wallet

**Files:** `vtorrent-tauri/src/commands/wallet.rs` (`authorize_wallet`, `repair_btc_state`, wallet selection), `vtorrent-tauri/src/commands/node.rs` (`start_node`, `btc_utxo_path`), `vtorrent-tauri/src/state.rs` (wallet selection synchronization), and `vtorrent-btc/src/wallet.rs` (locked open path if needed).

**Interface:** Keep `repair_btc_state` unchanged externally. Internally derive `RepairPaths` and a domain-separated nonsecret wallet fingerprint from one authenticated wallet snapshot. Use task 1's `run` for repair and `ensure_idle` for startup.

- [ ] Add command tests: failed password, missing configured OTP, or false attestation creates no intent; a wallet switch cannot redirect an authorized repair; either network refuses startup during a pending repair. Use a barrier for the switch/start tests rather than sleeps. Run `cargo test -p vtorrent-tauri repair` and confirm the new assertions expose missing behavior.
- [ ] Trace all writers of `wallet` and `wallet_path` before choosing lock order. Serialize selection changes with the authenticated snapshot through repair completion. Reuse existing credential validation; avoid exporting a secret to identify the record.
- [ ] Check pending status **after acquiring the same network writer lock that the opened BTC wallet retains**, before loading or creating any state. An `ensure_idle()` call before acquiring that lock leaves a race. Keep the existing legacy-file and initialization-marker refusals. Return actionable errors distinguishing an active writer, a resumable repair, and a conflict requiring assistance.
- [ ] Run `cargo test -p vtorrent-tauri repair` and the existing BTC initialization/missing-state tests. Confirm both normal startup after completion and refusal during repair, then commit.

### Task 3: Prove recovery across process exits

**Files:** Add process-exit tests alongside the internal storage tests in `vtorrent-btc/src/repair.rs`; extend command regressions in `vtorrent-tauri/src/commands/wallet.rs`. Keep interruption hooks test-only.

- [ ] Add `repair::tests::resumes_after_process_exit`: run the same test executable as a child against a disposable directory. Exit after each persistent step, including staging, intent publication, each marker and state move, directory sync, and intent retirement. Resume in a fresh process. Assert that both normal wallet opens succeed and all already-published files keep their bytes. After retirement, a repeat request may refuse existing state but must not change it.
- [ ] Add a two-process writer test: while one writer holds the repair locks, the other gets an in-use error. Kill the first process and verify the OS releases its locks. Also simulate an unsupported no-replace operation and assert pre-existing files survive.
- [ ] Run `cargo test -p vtorrent-btc repair::tests` and `cargo test -p vtorrent-tauri repair`; verify the named tests actually execute. Run the storage tests on Linux, both macOS architectures, and Windows before declaring publication portable. Process exits test restart behavior, not physical power-loss durability; review platform sync semantics separately.
- [ ] Run `cargo fmt --all -- --check`, workspace Clippy with CI flags, and `TMPDIR=/mnt/temp/pass15-tests cargo test --workspace --all-features`. Commit the crash regressions and record results against the worktree SHA. Release selection remains a separate decision.
