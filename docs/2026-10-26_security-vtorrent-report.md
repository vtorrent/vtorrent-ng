# vTorrent security review, 2026-10-26

## Remediation status

All four findings have been repaired on top of `35fd9ccd57ca54fde36f6b0fc33d996dedf8b502`. The original findings and evidence below describe the pinned pre-fix source. Regression verification was completed before deployment, without restarting either running probe. Subsequent deployment and soak records belong in [the soak log](soak-log.md).

| Finding | Repair | Regression evidence |
|---|---|---|
| F01 | Header sync stages a private header snapshot and commits only on success; contract scans use private snapshots; restored chain caches are invalidated when headers are reconstructed without persisted lineage | Failed-prefix rollback, successful-reorg invalidation, contract-scan isolation, and `restored_chain_cache_is_rebuilt_after_first_header_sync` regressions |
| F02 | Absolute connection, handshake, write, and request deadlines; a 30-minute complete-pass limit; no-progress header batches are rejected; peer-cache promotion requires validated filter data | `silent_and_chatty_header_peers_reach_healthy_fallback`; `peer_chatter_cannot_extend_filter_or_block_deadlines` covers checkpoint, filter-header, compact-filter, and block requests |
| F03 | One shared resolver supplies configured, cached, and DNS peers to wallet sync, claim verification, and settlement scans; mainnet quorum remains two independent groups | `mainnet_configured_peer_is_supplemented_by_cache_and_seeds`, DNS-failure and non-mainnet isolation tests |
| F04 | Filter records follow retained headers, including a predecessor anchor; checkpoints retain only the relevant range; votes retain one tip, at most 16 candidate fingerprints, and at most eight groups per candidate | A 10,000-header retention test, hostile candidate/voter churn test, and checkpoint-anchor retention test |

Verification on the repaired working tree:

- The original repair's affected-crate run passed 288 tests with zero failures and one ignored scale test. The startup-lineage follow-up adds one regression.
- `cargo clippy -p vtorrent-btc -p vtorrent-rpc -p vtorrent-wallet-service -p vtorrent-daemon --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace --all-features`, including the startup-lineage follow-up: 48 groups, 1,118 passed, zero failed, seven ignored.
- Formatting and diff checks passed.

To rerun the repaired contracts from the repository root:

```bash
bash docs/security-review-2026-10-26/verify-fixes.sh
```

The staging snapshots add temporary memory overhead. Release requires a fresh soak of the repaired commit; earlier probes do not validate these changes. Mainnet approval and independent third-party signoff remain open.

### Restart-lineage follow-up

Release-mode validation against real Bitcoin Core found another F01 lifecycle case after the initial `aeb103a` repair: UTXOs and a scan cursor survive restart, but the header branch does not. Reconstructing headers from genesis therefore cannot establish that the restored outputs still belong to the active branch. A reorg while the daemon is stopped left an obsolete balance selectable in the cache.

The startup path now clears the unverified chain cache and resets scanning after its first successful header reconstruction. Address indices, reservations, signed transactions, and swap recovery metadata remain intact. The new local regression failed before the repair and passes afterward. The previously failing real-Bitcoin `btc_spv_drops_a_reorged_out_funding_output` test now passes and reports zero balance after a conflicting spend replaces the funding output.

An ordinary unchanged-chain restart must also rescan under this policy because header lineage is not persisted. On mainnet, funds older than retained history still require sufficient header retention; the repair does not make unavailable history recoverable.

## Original review verdict

**Request changes before mainnet.** This review validated two high-severity issues and two medium-severity issues. The most important defect leaves BTC wallet state attached to an abandoned branch after a peer partially updates the header chain and then fails. A separate availability issue lets a single silent peer prevent header failover indefinitely.

This is a fresh internal model review of revision `35fd9ccd57ca54fde36f6b0fc33d996dedf8b502`. The reviewer also helped implement recent changes, so the report does not satisfy the independent third-party review gate. No repair, deployment, restart, or launch approval was performed during this review.

The immutable review target includes the peer-cache implementation in `2c381fa`. See [scope and authorization](security-review-2026-10-26/scope.md). The review prioritizes the post-`70ea7b9` BTC changes and samples wallet encryption, cold staking, and RPC authorization. It is not an exhaustive audit of every crate.

| ID | Severity | Finding | Evidence | Status |
|---|---|---|---|---|
| F01 | High | Interrupted header reorg bypasses wallet reconciliation | E01 | Validated |
| F02 | High | Silent peers prevent header failover and hold the wallet lock | E02 | Validated |
| F03 | Medium | A single-IP mainnet configured peer makes swap verification fail | E03 | Validated |
| F04 | Medium | Filter records and candidate votes have no retention bound | E04 | Validated structurally |

## Reproduce the findings

This is the historical pre-fix reproduction. Run it against the application source at `35fd9ccd57ca54fde36f6b0fc33d996dedf8b502`, with the review artifact directory available and dependencies cached. It asserts the old vulnerable behavior and is not the verification command for the repaired working tree.

From that repository root:

```bash
cargo run --offline \
  --manifest-path docs/security-review-2026-10-26/proof/Cargo.toml \
  --target-dir target
```

The proof uses localhost sockets, synthetic regtest headers, synthetic wallet state, and an isolated copy of the actual filter-store implementation. It does not connect to public Bitcoin peers or touch either soak directory. Without a populated Cargo dependency cache, dependency installation is required before the offline command can run.

The assertions deliberately demonstrate vulnerable behavior. These are review artifacts, not regression tests that should be added unchanged to CI. After repairs, invert the relevant assertions to require rejection, reconciliation, bounded completion, or eviction.

Observed output is retained in [proof-output.txt](security-review-2026-10-26/proof-output.txt).

## Findings

### F01. Interrupted header reorg bypasses wallet reconciliation

**Severity:** high. **Confidence:** high. **Evidence:** E01.

Locations at the pinned revision:

- `vtorrent-btc/src/sync.rs:381-424`, `BtcSync::sync_once`.
- `vtorrent-btc/src/wallet.rs:466-512`, `BtcWallet::sync`.
- `vtorrent-btc/src/wallet.rs:515-528`, `BtcWallet::sync_with_peers`.

`sync_once` applies received headers to the live `HeaderChain` one by one. A later invalid header or connection failure returns an error without rolling back earlier accepted headers. `BtcWallet::sync` uses `?` on that result before reaching its reorg detection, UTXO invalidation, filter reset, and cursor reset.

The new fallback path then calls `sync` using another peer. That second call captures the already-reorganized tip as its starting point, so it no longer detects the change from the original branch. The wallet can remain marked synced while retaining outputs and scan progress from an abandoned branch.

**Local proof:** seed a wallet with a height-one branch and a 100,000-satoshi UTXO associated with that branch. The first peer sends a valid higher-work two-header fork followed by an orphan header. The second peer sends an empty header batch. The combined operation returns `Ok(0)` while the header height is two, the abandoned output remains in the wallet, and the scan cursor remains two rather than being reset.

The proof establishes stale wallet state, not a successful unauthorized transfer or permanent loss of the seed. A public-network reproduction would require an actual valid fork, but a malicious relay can reuse real headers; it need not forge signatures or break proof of work.

**Recommended repair:** either stage header updates transactionally or perform tip-change reconciliation even when the sync pass returns an error. Enforce the same invariant for other writers of the shared header chain, including fresh swap scans, rather than relying on one successful `wallet.sync` call to notice every change.

**Acceptance test:** valid fork headers followed by a malformed suffix must either leave all wallet/header state unchanged or invalidate the old branch's wallet state before failover reports success. Repeat with a disconnect after a full batch and with shared-header swap scans.

### F02. Silent peers prevent header failover and hold the wallet lock

**Severity:** high for BTC availability. **Confidence:** high. **Evidence:** E02.

Locations:

- `vtorrent-btc/src/p2p.rs:115-123,171-197`, unbounded stream sends/receives outside handshake.
- `vtorrent-btc/src/sync.rs:381-424`, header response loop.
- `vtorrent-btc/src/sync.rs:500-509,583-592`, filter response loops with per-message timeouts.
- `vtorrent-rpc/src/btc_sync.rs:229-302`, peer caching and wallet write-lock lifetime.

`sync_with_peers` advances to another peer only after an error. `sync_once` can wait indefinitely for its first header response because it calls `peer.recv().await` without a request deadline. A peer can complete the compact-filter handshake and then remain silent. Neither the extra connected peers nor the in-cycle scan retry can help because execution never reaches them.

The sync loop holds the wallet write lock during this wait. BTC wallet status, send, and swap handlers that need the wallet can consequently wait behind the stalled operation. The new metrics avoid that lock, but do not release it. Caching a peer after a service-advertising handshake can also put the unresponsive peer near the front of later cycles or restarts.

**Local proof:** connect one silent peer and one healthy peer. After 12 seconds, header sync is still pending and the healthy peer has received zero header requests. The proof's outer timeout cancels the operation. Source inspection confirms there is no internal header-response deadline, so the twelve-second result is not being presented as a measurement of a configured timeout.

The compact-filter loops have a related weakness: restarting a ten-second timeout for every message allows irrelevant messages or pings to extend a request indefinitely.

**Recommended repair:** use absolute deadlines for each protocol request and bounded deadlines/progress budgets for complete sync passes. Bound writes as well as reads. On deadline expiry, discard the connection and try another peer. Promote peers in the persistent cache after useful validated responses, and demote repeated failures.

**Acceptance test:** a silent first peer and a peer sending periodic pings must both reach healthy fallback within the configured operation deadline. Verify the wallet lock is released and that a cached peer cannot repeat the stall indefinitely.

### F03. A single-IP mainnet configured peer makes swap verification fail

**Severity:** medium. **Confidence:** high. **Evidence:** E03.

Locations:

- `vtorrent-rpc/src/handlers/swap.rs:509-529`, mainnet BTC claim verification.
- `vtorrent-rpc/src/btc_reconciliation.rs:403-438`, configured-peer resolution for settlement scans.
- `vtorrent-btc/src/sync.rs:177-224`, swap scan peer requirements.

The wallet sync loop combines the configured peer with cached and DNS peers. The BTC claim and settlement callers still take the configured-peer branch exclusively. If `--btc-peer` resolves to a single address, they supply only one network group to the verifier.

Mainnet verification requires **two** independent compact-filter network groups. It attempts to collect up to three peers, which is distinct from the minimum requirement. A configured hostname with addresses in multiple groups might satisfy it; a normal single-IP configuration cannot.

**Local proof:** pass a valid mainnet HTLC and one configured address to the wallet verifier. It returns `BTC contract verification requires 2 distinct compact-filter network groups` before attempting a connection. Caller inspection establishes why the normal single-IP configuration supplies exactly that list even after the wallet sync loop has learned additional peers.

This fails closed and does not disclose the preimage. It nevertheless prevents the intended mainnet claim/observation workflow and can delay time-sensitive swap actions. Regtest tests do not catch it because regtest requires only one peer.

**Recommended repair:** use a common network-aware resolver for wallet sync, claim verification, and settlement scans. Preserve the configured peer's priority while adding validated cache entries and DNS candidates on mainnet. Never reduce the verifier's independence requirement to accommodate the single-peer caller.

**Acceptance test:** a mainnet single-IP configured peer plus a healthy cache produces at least two distinct groups; the same contract verifies against consistent data. Also retain rejection tests for one group and for two addresses in the same group.

### F04. Filter records and candidate votes have no retention bound

**Severity:** medium. **Confidence:** high for the data structure; mainnet exhaustion timing was not measured. **Evidence:** E04.

Locations:

- `vtorrent-btc/src/filters.rs:20-25`, retained maps.
- `vtorrent-btc/src/filters.rs:165-166`, candidate vote insertion.
- `vtorrent-btc/src/filters.rs:196-202`, agreed record accumulation.
- `vtorrent-btc/src/headers.rs:223-249`, separate header pruning.

The BTC header chain has a configurable retention window, but the shared filter store is not pruned when that window advances. Agreed records are continually extended, and the candidate map retains entries for old tips and fingerprints. Both are cleared on explicit state reset, not on normal forward progress.

Thus the bounded-header argument does not establish a bound on the complete BTC SPV wallet. Ordinary chain growth adds records and candidate entries. Rejected, unique candidate fingerprints can also accumulate. A short stable-RSS soak is insufficient to prove a long-running bound.

**Structural proof:** the actual `filters.rs` implementation, included unchanged in an isolated module, retains 10,000 records and 10,000 candidate entries after 10,000 distinct synthetic tips. This does not emulate a real mainnet chain or measure a mainnet memory-exhaustion attack. It demonstrates the lack of eviction in the affected store.

**Recommended repair:** prune records with the canonical retained header range while preserving any explicitly needed predecessor/checkpoint anchor. Bound candidate tips, fingerprints per tip, and voters, and expire obsolete votes. Apply the policy during normal forward progress, not only on reorg.

**Acceptance test:** advance far beyond a small configured retention window and assert bounds for header records, filter records, and candidate maps while current scans and a permitted reorg still verify correctly.

## Evidence and call paths

| Evidence | Observation | Reproduction/source |
|---|---|---|
| E01 | Partial reorg is followed by successful fallback without clearing the seeded old-branch balance | Proof `interrupted_reorg`; pinned `wallet.rs` and `sync.rs` |
| E02 | Silent first peer prevents any healthy-peer request until proof cancellation | Proof `silent_peer`; pinned `sync_once` |
| E03 | Single-peer mainnet verification rejects before network I/O | Proof `configured_mainnet_peer`; claim and settlement resolver source |
| E04 | Filter records and candidate entries accumulate without eviction | Proof `audited_filters::growth_proof`; actual included filter-store source |
| E05 | Eight P2CS tests and ten encryption tests pass | Commands below |
| E06 | Sensitive-route auth and fail-closed claim verification tests pass | Commands below |
| E07 | Local dependency audit returns zero vulnerability entries and informational warnings | `cargo audit --json --no-fetch` |

**P01, F01 call path:** peer supplies valid fork prefix and invalid suffix → `sync_once` changes the live tip → `wallet.sync` exits before reconciliation → `sync_with_peers` starts from the changed tip → fallback succeeds with stale UTXOs/cursor. Supported by E01.

**P02, F02 call path:** peer advertises compact filters → sync loop caches/connects it → wallet write lock is acquired → first `getheaders` remains unanswered → no error reaches fallback → wallet-dependent operations wait. Supported by E02 and source inspection.

**P03, F03 call path:** operator sets a single-IP mainnet `--btc-peer` → claim/settlement resolver skips cache and DNS expansion → verifier sees one group → verification rejects without broadcasting. Supported by E03 and E06.

**P04, F04 call path:** forward scans or new fingerprints → map insertions → header retention advances independently → old filter records/candidate entries remain. Supported by E04. This is a data-retention path, not a demonstrated live exhaustion attack.

Proof artifact SHA-256 values after formatting:

| Artifact | SHA-256 |
|---|---|
| `security-review-2026-10-26/proof/Cargo.toml` | `7ec39cb383d15c3a01aa97dce59a60ee88bfd0ccd50e564b3db1d79fcafc1812` |
| `security-review-2026-10-26/proof/src/main.rs` | `9d5663f8dcedfaafe25aa221eb6bed81432834c6656895dfd992cc266851d3f0` |

## Supporting checks and explicit verdicts

Commands executed against the pinned application source:

```bash
cargo test -p vtorrent-node p2cs
cargo test -p vtorrent-wallet encryption::tests
cargo test -p vtorrent-rpc sensitive_read_routes_require_api_key
cargo test -p vtorrent-rpc btc_claim_verification_failure_never_broadcasts_or_updates_state
cargo audit --json --no-fetch
```

- **Cold-staking R1:** no additional bypass established in the inspected paths. Full-node coinstake validation checks both redirected value and preservation of principal. The inspected SPV P2CS verifier checks the same two conditions. Eight selected node tests passed, covering hot-key spending, P2SH-wrapped hot-key spending, principal burning, and cold-key authorization. This is not a complete consensus proof or an exhaustive malformed-script audit.
- **Wallet encryption:** no new cryptographic defect established in the inspected implementation. Argon2id parameters are fixed at 64 MiB, three iterations, four lanes; encryption creates fresh salts and nonces; decryption checks version and lengths and uses authenticated ChaCha20-Poly1305. Ten selected tests passed. The accepted threat model remains passphrase-only encryption at rest; TOTP is an app-unlock control, not an independent encryption factor.
- **RPC authorization:** the sampled sensitive-route test passed. The failed BTC claim verification test confirms no broadcast or state update on verifier rejection. This supports the fail-closed impact statement for F03, not a claim that every RPC/IPC route was audited.
- **Dependencies:** the local ignored lockfile contained 643 packages. `cargo audit --json --no-fetch` reported zero vulnerability entries, seven unmaintained-package warnings, and two unsoundness warnings, including `glib 0.18.5` and `rand 0.7.3`. Reachability was not established. The local advisory database returned no last-updated timestamp, so this is not a fresh release dependency signoff. Semgrep and promtool were unavailable.

## Release recommendation and limitations

Fix F01 and F02 before mainnet. Fix F03 before declaring the single-IP configured-peer mainnet swap workflow supported. Bound F04 before asserting a long-running BTC memory budget. Reproduce each repair with regression tests, then run the final-source soak and the separate maker/taker swap test on the repaired commit.

No direct theft or private-key extraction was demonstrated. The two high-severity findings concern wallet state integrity and availability; they should not be relabeled as successful fund theft. All four proofs used local synthetic inputs. No full workspace test run was performed in this review, and a passing prior suite does not negate these reproductions.

The existing peer-diversity checks reduce correlated-peer risk but do not prove that two network groups have different operators. The current peer cache records advertised capability at handshake, not continued honesty or validated service. Those are residual assumptions for the external review.

Governance, the full legacy wallet parser, torrent economics, and a complete network-resource audit remain outside this pass. The independent third-party review gate and the active swap soak remain open. This report does not change the recorded status of either live soak.
