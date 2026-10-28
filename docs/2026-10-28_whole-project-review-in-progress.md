# Whole-project review, in progress

## Scope and status

- Reviewed revision: `6bc25782770bdc8488f62c87caf40cc17148984e`; pre-fix application code was `5f4c788d06995b5c77202b531c52055d7cf79c6c`.
- Remediated revision: `9a22bcd8ccd9bbdb4caf9bec794640fc3c0111aa`.
- Authorized scope: all workspace crates, the frontend, CI/build, deployment, and operations. This report is **not complete** and does not certify unaudited areas.
- Reviewer: an internal coding model that helped implement prior changes. This is not independent human security signoff.
- No production wallets or services were modified during this review. The torrent proof uses a temporary directory and Unix symlink.

## Remediation status

All six findings from this pass are remediated and pushed in `8ba6331` and `9a22bcd`. The original finding text below remains as the pre-fix record.

| Finding | Fix | Verification |
|---|---|---|
| F01 | Removed secret-bearing CLI flags; added 0600 `--rpc-api-key-file` and `--btc-seed-file`; environment variables remain supported | CLI rejection and file-mode tests; smoke/e2e/compose updated |
| F02 | Added symlink-aware directory creation, canonical containment checks, and `O_NOFOLLOW` on Unix | Proof now confirms the symlinked path is rejected and no file is overwritten |
| F03 | Added tracked `release/Cargo.lock`; Docker builds with `--locked`; CI audits and archives that lockfile | Locked daemon build and exact-source Docker image `sha256:196a7c82832ecbfba2560f935ea3c431b8e12b77d3f5d7d2b2a5e9046051517c` passed |
| F04 | Tracker response is consumed incrementally and rejected at the cap | 64 MiB local stream rejected; client did not read the full body |
| F05 | BTC send now requires passphrase and optional TOTP reauthorization | No-passphrase request rejected; authenticated send succeeded |
| F06 | Added justified `cargo-machete` metadata for the proof crate | `cargo machete` exits 0 |

Residual: F02's checks close the demonstrated pre-existing-symlink path and protect the final component with `O_NOFOLLOW`, but a local attacker able to race intermediate directory replacement is outside this proof. F01 still accepts secrets through environment variables, which are less exposed than argv but visible to the process owner and container metadata; production should use the 0600 file options.

**Release remains blocked** on the incomplete coverage below, the independent human review, and a soak of `9a22bcd`. The previous "do not launch" verdict applied to the pre-fix revision; it is not a finding against the remediated build.

## Original findings and evidence

## Confirmed findings so far

### F01: BTC seed and RPC API key can be exposed through process arguments

**Severity: high in multi-user deployments; conditional on local process-inspection access.** `vtorrent-daemon/src/config.rs:114–128` accepts the Bitcoin BIP39 seed and RPC API key as CLI arguments. `vtorrent-daemon/src/main.rs:406–438` initializes the Bitcoin wallet from the supplied seed. On the review host, a running test probe's `/proc/<pid>/cmdline` had mode `0444`, `/proc` was mounted without a `hidepid` option, and both `--btc-seed` and `--rpc-api-key` appeared in its argument vector. The values were not included in the report.

**Impact:** another local account able to inspect the process can recover the seed when the operator uses the CLI option. The seed gives access to BTC derived from it. The API key can also expose protected RPC routes. The exposure depends on the operator using argument-based secrets and the host's process-visibility policy; no remote exploit was demonstrated.

**Fix:** support and document protected secret-file or inherited-file-descriptor input for the BTC seed and API key; avoid secrets in argv and logs. Review analogous CLI passphrase arguments in `vtorrent-cli/src/main.rs:92–105`. Test with an unprivileged second account and without printing secret values.

### F02: Torrent piece writes can follow a symlink outside the download directory

**Severity: medium, conditional on a pre-existing or concurrently created symlink.** `vtorrent-torrent/src/engine_disk.rs:60–96,140–157` rejects lexical traversal but uses `create_dir_all` and `OpenOptions::open` on paths that can contain symlinks. `vtorrent-torrent/src/engine.rs:797–816` calls this path for a piece that matches the torrent's SHA-1 digest.

**Proof:** before the fix, a temporary download tree contained `payload/escape` as a symlink to an outside directory, and `write_piece_to_disk` overwrote `outside/victim`. After the fix, the same proof is rejected and `outside/victim` remains unchanged. Reproduce on Unix:

```bash
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/proof/Cargo.toml --target-dir target
```

**Impact:** a malicious torrent author who can induce a user to download into a tree containing a suitable symlink can overwrite a file outside the intended download directory, subject to the daemon user's permissions. The proof does not show that a remote peer can create the prerequisite symlink by itself. The same lexical-only path handling is used by `read_piece_from_disk`.

**Fix:** use descriptor-relative, beneath-root file operations that reject symlink traversal for every component and final file. A pre-open canonicalization check alone is vulnerable to replacement races. Test existing symlinks, final-component symlinks, concurrent replacement, and Windows reparse points.

### F03: Release dependency resolution is not pinned

**Severity: medium for release provenance.** The root `Cargo.lock` is ignored. `.github/workflows/build.yml:92–95` generates a new lockfile, and `docker/Dockerfile:15,37` accepts an absent lockfile and builds without `--locked`. Pinned container base digests therefore do not pin Rust dependencies or prove two builds from the same source resolve identically.

**Fix:** retain and verify a release-specific dependency lockfile, build with `--locked`, and archive the lockfile, toolchain versions, image digest, and SBOM. Rebuild from the same inputs and compare resulting artifacts, documenting platform-specific nondeterminism.

### F04: A tracker can exceed the response-size limit before rejection

**Severity: high for availability.** `vtorrent-torrent/src/tracker.rs:333–350` checks `Content-Length` when present, but then calls `response.bytes().await` before checking the resulting length. A tracker that omits `Content-Length` can make the client buffer an arbitrarily large response until the request timeout or resource exhaustion. The 4 MiB check occurs only after buffering has finished.

**Local proof:** the bundled proof runs a localhost HTTP tracker that streams 64 MiB without `Content-Length`. After the fix, the client rejects the response at the 4 MiB cap and stops consuming; the local socket accepted about 6.5 MiB of buffered writes before the peer observed closure. The proof does not attempt to exhaust memory.

**Fix:** enforce the response limit while streaming, stopping as soon as the accumulated bytes exceed the cap. Cover absent `Content-Length`, chunked responses, dishonest lengths, multiple concurrent sessions, and normal valid tracker responses.

### F05: Default loopback RPC permits unauthenticated BTC sends while unlocked

**Severity: high when untrusted local processes can reach RPC.** The daemon permits an unset RPC API key when bound to loopback (`vtorrent-daemon/src/config.rs:228–240`). The RPC middleware passes requests through when no key is configured (`vtorrent-rpc/src/server.rs:36–55`). `POST /api/v1/btc/send` is in that protected router, but its handler only checks whether the VTR wallet is currently unlocked and does not reverify a passphrase or TOTP (`vtorrent-rpc/src/handlers/btc.rs:53–63,91–109`).

**Local proof:** before the fix, the in-process router was configured without an API key and with a synthetically unlocked wallet and fake regtest UTXO; a request with no passphrase returned HTTP 200 and delivered a signed BTC transaction to a local mock Bitcoin peer. After the fix, the no-passphrase request is rejected and an authenticated request succeeds. No real Bitcoin broadcast or theft was performed:

```bash
cargo run --offline --manifest-path docs/whole-project-review-2026-10-28/rpc-proof/Cargo.toml --target-dir target
```

**Impact:** while the wallet is unlocked, another local process able to reach the daemon's loopback port can request a BTC send under the default no-key configuration. Wallet unlock remains a prerequisite. The proof sets the unlocked state directly to model a legitimate unlock; it does not bypass unlock or demonstrate a browser-origin attack.

**Fix:** require an authenticated local transport or API key by default, and reauthorize BTC sends with the same passphrase/TOTP policy used for VTR sends. Add tests for a second local principal, no-key startup, already-unlocked wallets, and failed/broadcast paths.

### F06: The CI unused-dependency gate fails on a review proof

**Severity: medium for release engineering.** Running the CI command `cargo machete` with local version 0.9.2 at the reviewed revision exits with code 1 and reports `sha2` as unused in `docs/security-review-2026-10-26/proof/Cargo.toml`. The proof incorporates the filter implementation with `include!` (`proof/src/main.rs:22`), and that included file uses `sha2`. This is a static-analysis false positive that can fail the `machete` CI job in `.github/workflows/build.yml:101–109`, which is a prerequisite for desktop builds. The installed hosted-CI version and current hosted check result were not verified.

**Fix:** configure the proof package's cargo-machete metadata to ignore this justified dependency, or restructure the proof so the scanner can see the use. Verify both `cargo machete` and the proof still pass from a clean checkout.

## Checks performed

- Frontend: `pnpm test` passed 42 tests; `pnpm lint` and `pnpm build` passed. The build reported an existing mixed dynamic/static import warning.
- `cargo test --workspace --all-features` passed 48 groups with 1,121 tests passed, zero failed, and eight ignored. Ignored tests and external-service workflows are not evidence of success.
- Selected all-feature library suites passed: 406 tests across node, wallet, script, migrate, and snapshot; 319 across core, P2P, overlay, SPV, store, onion, and torrent; and 280 across BTC, RPC, Tauri, and wallet-service, with one ignored test.
- `cargo fmt --all -- --check` passed. `python3 scripts/verify-genesis.py` independently reproduced the expected snapshot totals and genesis hash.
- `cargo audit --json --no-fetch` on the local ignored lockfile reported zero vulnerability entries, seven unmaintained warnings, and two unsoundness warnings (`glib 0.18.5`, `rand 0.7.3`). Advisory database freshness and reachability were not established.
- Semgrep was unavailable. No remote scanner was installed or run.
- `promtool test rules deploy/seeds-monitoring/vtorrent-alerts.test.yml` passed using the existing local Prometheus image with networking disabled. Production notification delivery was not tested.
- `cargo machete` exits 0 after the proof-crate metadata fix.
- Both local proofs pass: the tracker stops after roughly 6.5 MiB of a 64 MiB stream, the symlink write is rejected, and the no-passphrase BTC send is rejected while the authenticated send succeeds.
- The exact-source Docker image was built from `9a22bcd` with `--locked`; image ID `sha256:196a7c82832ecbfba2560f935ea3c431b8e12b77d3f5d7d2b2a5e9046051517c`.

## Coverage and remaining work

Initial source sampling covered consensus validation/reorg entry points, wallet encryption, BTC wallet recovery, RPC auth boundaries, P2P framing, torrent disk/peer paths, Tauri capability configuration, migration parsing, persistent block writes, the Dockerfile, and CI. These are samples, not complete subsystem audits. This pass has not completed the full consensus fork/PoS proof, full swap and deadline analysis, all RPC/Tauri entry points, overlay and onion cryptography, every torrent protocol path, migration recovery, complete frontend behavior, production deployments, backups, rollback drills, or independent dependency reachability analysis. A fresh independent human review was not run as part of this pass.

Do not interpret the absence of another finding as a clean verdict for any unchecked subsystem. Findings F01–F06 remain unremediated in this review.
