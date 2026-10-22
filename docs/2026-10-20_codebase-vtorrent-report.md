# Whole-codebase review of vTorrent-NG

## Executive assessment

The current source still has launch-relevant correctness and availability
defects after the earlier remediation batch. The most serious verified issue
is acceptance of an immature bootstrap stake at height 3 despite the intended
height-2-only exemption. BTC header pruning also breaks scans at the retained
boundary, and empty torrent blocks bypass the new assembler cap. Legacy
recovery rejects valid uncompressed encrypted keys. Several operational
protections were reported as complete before their actual behavior was checked.
Fix and independently retest these paths before treating the candidate as
ready for launch. This is a broad internal review, not external audit sign-off.

## Scope and method

- Authorized scope: the user's request to review the whole repository and
  identify improvements.
- Source: `e604cf041fba5eaa289c04ae911f11669d54e3ad`; application implementation
  baseline `ff14e5881794fc831d3e37fb8eb7c46abc7ca2b7`.
- Inventory: Cargo metadata reports 18 workspace packages; Git tracks 213
  Rust files. The React frontend, CI workflows, and operational scripts were
  included. Earlier descriptions of a 19-crate workspace were inaccurate.
- Method: inspect entry points and selected high-risk paths across every
  package, trace trust boundaries, and run focused synthetic defect probes.
  This was not a line-by-line audit of every function.
- Tools: Rust 1.90.0, Python 3.12.3, Cargo, Git, existing frontend tooling.
  Semgrep, promtool, actionlint, and graft tools were unavailable. No fresh
  dependency-vulnerability or formal model-checking assessment was performed.
- The running fleet was left unchanged. Probe source changes were removed
  from Cargo test discovery afterward and retained under `docs/review-probes`.
  No application fixes are part of this review deliverable.

### Coverage map

| Area | Packages or surface | Paths inspected |
| --- | --- | --- |
| Consensus and governance | `vtorrent-core`, `vtorrent-node`, `vtorrent-script`, `vtorrent-spv` | Block and transaction validation, bootstrap maturity, kernel eligibility, signed votes, locks, undo, script limits, proof binding |
| Custody and recovery | `vtorrent-wallet`, `vtorrent-wallet-service`, `vtorrent-migrate`, `vtorrent-tauri` | Encryption, file writes, key provenance, encrypted legacy records, send admission/broadcast, desktop wallet lifecycle |
| Networking and privacy | `vtorrent-p2p`, `vtorrent-overlay`, `vtorrent-onion` | Framing, serialization, bans, quota keys, authenticated overlay sessions, replay counters, strict-onion routing |
| Bitcoin | `vtorrent-btc` | Header validation and retention, scan ranges, compact filters, swap verification, HTLC arithmetic, spend selection |
| Storage and history | `vtorrent-store`, `vtorrent-snapshot`, `vtorrent-daemon` | Atomic store transactions, replay/version checks, snapshot parsing, event and sync loops |
| Content and clients | `vtorrent-torrent`, `vtorrent-cli`, React frontend | Piece framing/assembly, operational commands, IPC result shapes, browser fallback, status displays |
| Delivery and operations | GitHub Actions, metrics, monitoring, soak scripts | Release dependencies, benchmark event inputs, dependency locks, alert inputs, missing-evidence behavior |

## Evidence ledger

Exact probe sources and commands are in [review-probes/README.md](review-probes/README.md).
These checks deliberately fail when a required invariant is violated.

| Evidence | Observation | Verification level |
| --- | --- | --- |
| E01 | Validly signed split bootstrap claim followed by PoS blocks at heights 2 and 3. The second immature stake is accepted. | Executed chain acceptance |
| E02 | Six valid regtest headers, retention forced to two. `hashes_from(4)` returns zero instead of heights 4, 5, 6. | Executed public API |
| E03 | Valid decoded empty BitTorrent piece frame submitted 5,000 times at offset zero. Every call succeeds. | Executed wire decoder and assembler |
| E04 | AES-CBC encrypted scalar with matching public key and correct master key. Compressed control passes; uncompressed public key fails. | Executed cryptographic helper |
| E05 | Actual `/metrics` router response lacks `vtorrent_best_peer_height`. Source also lacks the RSS and BTC sync exporters used by new alert rules. | Executed HTTP handler and source trace |
| E06 | Real observer with healthy fake node data and empty or invalid Prometheus data returns exit 0 in both cases. | Executed isolated script fixtures |
| E07 | Transaction passes stateless validation and is below the block size bound; inner transaction JSON fits the response's 4 MiB budget, but outer response is 4,787,037 bytes. | Executed serialization boundary |
| E08 | Desktop constructs a fresh node and separate RPC state; its event task only updates staking rewards. No desktop BTC header/UTXO sync loop is wired. | Source trace, no desktop runtime test |
| E09 | Rust IPC structs serialize snake_case; several Tauri hook branches return them without normalization while consumers read camelCase. | Source contract comparison |
| E10 | Public WebSocket route forwards peer and staking-address events without the API-key middleware. | Source trace |
| E11 | Benchmark workflow interpolates PR base or push `before`; scheduled events provide neither. Calling the script with an empty compare-ref prints its argument error and exits 2. | Executed argument check and workflow trace, no hosted run |

Frontend checks completed successfully: lint, five test files with 16 tests,
and production build. The build emitted a non-fatal mixed static/dynamic
import warning. Passing those checks did not exercise the desktop IPC
contracts identified below. The full Rust workspace suite was not rerun for
this review; earlier passing-suite claims are not used to dismiss these probes.

## Findings

### F01. Bootstrap maturity exemption extends past height 2

- Severity: high. Status: validated. Evidence: E01.
- Location: `vtorrent-node/src/chain/chain_reorg.rs:627-634` and
  `vtorrent-node/src/node/staking_loop.rs:60-77`.
- Path: a legacy holder signs a bootstrap claim with two stakeable outputs;
  one is consumed at height 2; a second, still immature output is staked at
  height 3. The validator checks its origin height and the height-1 block's
  type, but not the candidate height. The normal producer stops granting the
  exemption once the tip is no longer the bootstrap block.
- Observed failure: `validator accepted an immature bootstrap output at height 3`.
- Impact: a block producer can bypass the specified minimum maturity rule
  beyond the bootstrap transition. This is a validation-policy defect, not a
  demonstrated arbitrary mint or signature bypass.
- Remediation: require candidate height 2 for the exemption and test split
  bootstrap outputs at heights 2, 3, and after normal maturity. Check producer
  and validator rules together.

### F02. BTC retention breaks scan ranges and initial mainnet wallet discovery

- Severity: high for swaps at launch. Status: validated boundary; startup
  consequence established by source trace. Evidence: E02.
- Location: `vtorrent-btc/src/headers.rs:338-355`,
  `vtorrent-btc/src/sync.rs:527-531,601-604`, and
  `vtorrent-daemon/src/main.rs:978-1001`.
- Path: pruning removes the parent immediately below the retained floor.
  `hashes_from` collects the requested floor header, follows its absent
  parent, and discards the entire result. Requests starting before the
  retained range also return empty. The scan treats empty as `Ok(0)` and the
  daemon breaks the scanning loop.
- Observed failure: retained heights 4, 5, 6 produce `left: 0`, `right: 3`.
- Impact: count-bounded retention can coexist with failed wallet discovery.
  A fresh wallet starting its scan near genesis after a bounded mainnet header
  sync cannot obtain that old range. Compact-filter authentication may also
  require an earlier checkpoint range than remains in memory.
- Remediation: fix the inclusive floor boundary, distinguish unavailable
  history from an empty complete scan, and define authenticated historical
  header loading or explicit wallet-birthday/checkpoint behavior. Do not
  silently clamp old scans and declare them complete. Extend scale tests to
  scan and discover funds after pruning, not merely check header counts.

### F03. Empty torrent blocks bypass the assembler cap

- Severity: high availability. Status: validated. Evidence: E03.
- Location: `vtorrent-torrent/src/peer_wire.rs:187-197` and
  `vtorrent-torrent/src/engine.rs:56-71,744-765`.
- Path: the wire decoder accepts a piece frame with an empty data payload.
  A zero-length interval never overlaps an existing zero-length interval.
  Each accepted call appends a range, but inserting the same offset replaces
  one map entry. The 4,096-block guard checks map length, not range length.
- Observed failure: all 5,000 empty frames admitted instead of zero.
- Impact: range storage grows without the intended cap, and each subsequent
  overlap check scans a larger vector. The recent overlap fix did not close
  this empty-block path.
- Remediation: reject empty payloads before mutation, preserve duplicate-offset
  rejection, use one authoritative bounded representation, and test wire-level
  empty blocks, duplicate offsets, tiny blocks, and endpoint arithmetic.

### F04. Valid encrypted uncompressed legacy keys are rejected

- Severity: high recovery correctness. Status: validated. Evidence: E04.
- Location: `vtorrent-migrate/src/crypter.rs:308-313`.
- Path: decryption succeeds, but verification always compares the compressed
  33-byte derived public key with the stored public-key bytes. A valid 65-byte
  uncompressed record cannot match.
- Observed failure: `valid uncompressed ckey rejected despite correct master key and ciphertext`.
  The compressed control passes with the same fixture construction.
- Impact: an all-uncompressed encrypted wallet can be reported as having a
  wrong passphrase. In a mixed wallet, compressed keys may import while other
  valid keys are skipped.
- Remediation: compare parsed curve points or serialize according to the
  record's validated compression form. Test both forms, mixed records, and
  rotated master keys through the complete parser/import path. The recent
  parsed-record test covers only compressed keys and is not a BerkeleyDB-file
  or desktop-import end-to-end test.

### F05. Three new monitoring rules have missing metric inputs

- Severity: medium operational assurance. Status: validated. Evidence: E05.
- Location: `vtorrent-rpc/src/metrics.rs:51-173` and
  `deploy/seeds-monitoring/vtorrent-alerts.yml:48-73`.
- Missing inputs: `vtorrent_best_peer_height`, `vtorrent_process_rss_bytes`,
  and `vtorrent_btc_synced`. The previously reported BTC best-height exporter
  is also absent.
- Impact: SyncLag, ProcessRssHigh, and BtcSpvStalled do not evaluate as intended
  because their input series do not exist. Valid YAML and passing old metrics
  tests did not prove that the exporters were implemented.
- Remediation: implement the exporters, assert actual exposition keys and
  values, add promtool rule tests for enabled/disabled/unavailable BTC states,
  and verify the deployed scrape and alert route. Correct the earlier claim
  that these metrics had already been added.

### F06. The soak gate can report success without required evidence

- Severity: medium operational assurance. Status: validated. Evidence: E06.
- Location: `scripts/soak-observe.sh:118-148,159-187,195-201`.
- Path: empty Prometheus results produce no series checks; invalid responses
  become `Prometheus unavailable`; both are reduced to zero down/gap counts.
  Missing cadence is also printed without failure. Docker log queries use
  rounded `window_hours`, omitting the fractional beginning of the window;
  `--since 0h` means approximately now, not the start of the soak.
- Observed result: observer exits 0 for both an empty result and invalid JSON.
- Impact: a green observation does not establish continuous coverage or
  absence of stalls. Cadence only compares existing blocks and cannot detect
  that the newest block is stale. Docker memory usage is also labelled RSS
  despite not being a direct process RSS reading.
- Remediation: consume structured evidence, require all three expected series,
  reject missing/partial data, check last-block freshness, and query logs from
  the exact start timestamp. Make startup grace explicit instead of dropping
  the first sample unconditionally. Test unavailable Docker/Prometheus, empty
  history, missing series, stale height, and partial intervals.

### F07. The compact-block response budget does not bound wire bytes

- Severity: medium availability/protocol correctness. Status: validated
  serialization boundary. Evidence: E07.
- Location: `vtorrent-node/src/node/handler.rs:620-670`,
  `vtorrent-p2p/src/message.rs:18`, and `vtorrent-p2p/src/codec.rs:69,119-133`.
- Path: the helper caps the sum of inner transaction JSON byte lengths at
  4 MiB. The outer JSON encodes those bytes again as arrays of decimal numbers.
  That expansion is outside the cap; the encoder has no corresponding size
  rejection while the receiver does.
- Observed failure: a response compliant with the inner budget becomes
  4,787,037 bytes, above the 4,194,304-byte inbound maximum. This probe did not
  submit a signed block over a live network connection.
- Remediation: bound the final encoded message, use the negotiated protocol
  consistently, and reject oversized outbound frames. Test large legitimate
  compact-block recovery and full-block fallback rather than truncating a
  response that the requester expects to be complete.

### F08. Desktop node integration is materially behind daemon integration

- Severity: high feature-readiness concern. Status: source-verified, not
  reproduced in a running desktop. Evidence: E08.
- Location: `vtorrent-tauri/src/commands/node.rs:93-98,141-148,189-221` and
  `vtorrent-tauri/src/state.rs:17-24`.
- The desktop constructs a fresh `Node` and shares only chain/mempool Arcs;
  it does not use the daemon's BlockStore replay/persistence bridge.
- Its event task updates staking rewards but not peer count or sync status,
  despite `get_node_info` reading the RPC state's independent health fields.
- It creates a BTC wallet but never wires a header/UTXO discovery loop. A
  fresh wallet cannot gain spendable BTC from automatic discovery through
  this startup path. Lazy swap verification is not general wallet discovery.
- Remediation: share a headless node-service lifecycle with daemon and desktop,
  including storage, health, BTC sync, cancellation, and broadcast handles.
  Validate desktop restart, real peer status, BTC funding discovery, and
  actual two-node transaction propagation. The missing Tauri commands for
  governance, earnings, payment requests, and swap deadlines, the desktop
  health-field updates, the shared BTC sync loop, and desktop chain
  store replay/persistence were added. The adversarial review then added the
  desktop data-directory/network/lag fixes. Desktop restart, real peer status,
  BTC funding discovery, and two-node propagation still need a runtime test.

### F09. Several frontend IPC contracts do not match serialized Rust results

- Severity: medium user-visible correctness. Status: source-verified, no GUI
  execution. Evidence: E09.
- Location: `vtorrent-ui/src/hooks/useNode.tsx:96-128`,
  `vtorrent-tauri/src/commands/staking.rs:8-17`, and
  `vtorrent-ui/src/pages/StakingPage.tsx:43-49`.
- Rust returns `eligible_utxos`, `blocks_staked`, and other snake_case fields.
  The Tauri staking, transaction, torrent, and DEX fetch branches do not apply
  the normalization used by several HTTP branches. Type arguments and casts
  do not transform runtime JSON. Node info also returns `best_hash`, which
  normalization does not rename to the expected `bestBlockHash`.
- Impact: staking details can display zero or missing values despite an active
  backend. Related tables consume undefined fields.
- Remediation: generate or share wire DTOs, parse/normalize at every transport
  boundary, and run contract tests using actual Rust serialization. Browser
  mock wallet operations should also be explicitly development-only rather
  than selected solely by absence of Tauri.

### F10. Protected peer reads still have a public WebSocket side channel

- Severity: medium privacy. Status: source-verified. Evidence: E10.
- Location: `vtorrent-rpc/src/server.rs:220-224` and
  `vtorrent-rpc/src/ws.rs:73-100`.
- Peer reads require an API key, but public WebSocket events include peer
  addresses and staking reward addresses. The API-key route middleware does
  not cover the upgrade. A comment recommending a reverse proxy does not
  enforce the same privacy boundary.
- Remediation: authenticate subscriptions/upgrades or separate public block
  events from private wallet/peer events. Test a remote unauthenticated client
  against both REST and WebSocket surfaces, including Origin handling.

### F11. Benchmark baseline selection is not defined for every CI event

- Severity: medium delivery reliability. Status: source-verified event logic;
  no hosted workflow execution. Evidence: E11.
- Location: `.github/workflows/build.yml:10-12,121-122,154` and
  `scripts/bench-gate.sh:43-46,58-59`.
- Scheduled runs have neither a PR base nor a push `before`, yielding an empty
  compare-ref. A newly created tag can provide an all-zero previous ref, which
  is not a Git commit. Desktop release jobs depend on the benchmark gate.
- Remediation: resolve and validate baselines per event type and define the
  release baseline explicitly. Add workflow tests for PRs, main pushes,
  schedules, tags, and first-run behavior. A zero-comparison benchmark run
  should not silently count as a successful regression comparison.

## Improvement plan

1. Fix F01 with a genuinely height-bounded bootstrap rule and keep its positive
   height-2 control. Fix F03 before treating the torrent memory bound as closed.
2. Resolve F02 before swaps-at-mainnet-launch sign-off. Integrate retention,
   filter checkpoints, wallet discovery, and recovery in the same tests.
3. Fix F04 and add real serialized wallet-file/import coverage, including
   compressed/uncompressed keys, independent random master keys, and rotation.
4. Close F05/F06 before accepting soak evidence. Use exporter/rule/observer
   contract tests so missing telemetry cannot look healthy.
5. Unify daemon/desktop orchestration and transport DTOs for F08/F09. Keep
   business validation in shared services rather than mirrored adapters.
6. Bound final wire messages for F07 and close the WS privacy boundary in F10.
7. Harden CI event handling for F11. Resolve the gitignored lockfile once per
   release run, reuse it across test/audit/build jobs with `--locked`, and
   archive it; different jobs currently resolve dependencies independently.

Further maintainability opportunities include removing the public no-op
network shims in `vtorrent-node/src/node/p2p.rs:18-39`, replacing broad shared
`AppState` access with typed service handles, using reliable transaction
submission/backpressure rather than reporting success after an ignored
`try_send` failure, and separating financial identity from the UI's browser
mock mode. Consolidate repeated fee, network, DTO, and snapshot-format rules
only after tests establish their intended behavior.

## Remediation status after review

The bounded high-severity findings were fixed after this review snapshot, each
with a permanent regression:

| Finding | Status | Regression |
| --- | --- | --- |
| F01 bootstrap maturity | Fixed: exemption requires candidate height 2 | `test_bootstrap_age_exemption_does_not_extend_to_height3` |
| F02 BTC retention boundary | Boundary fixed: retained floor remains scannable, partial ranges fail closed, and wallet scans clamp to the retained floor with a warning. `--btc-birthday` sets an explicit creation height for older funds. | `test_hashes_from_retained_floor_returns_retained_range` |
| F03 empty torrent blocks | Fixed: empty payloads are rejected before mutation | `test_piece_assembler_rejects_empty_blocks` |
| F04 uncompressed legacy keys | Fixed: pubkey comparison uses the record's compression form | `test_decrypt_private_key_accepts_compressed_and_uncompressed` |
| F05 missing alert metrics | Fixed: best-peer height, BTC height/sync, and process RSS are exported | `metrics_export_alert_dependencies` |
| F06 soak false green | Fixed: requires three Prometheus series, fails on unavailable or missing evidence, checks for no blocks after the first hour, uses the exact window start for log queries, and applies an explicit 120-second startup grace | `docs/review-probes/soak_missing_evidence.py` now exits 0 |
| F07 compact-block wire budget | Fixed: the final encoded payload is bounded and an oversized response falls back to `notfound`/full block | `blocktxn_payload_rejects_oversized_final_message` |
| F09 frontend IPC contracts | Fixed for the dashboard's used fetch/action branches: Tauri results are normalized and node info exposes `bestBlockHash`. Missing Tauri commands for governance, earnings, payment requests, and swap deadlines, plus the unused place-order shape, remain part of F08. | Frontend lint, 16 tests, and production build |
| F10 WebSocket side channel | Fixed: the upgrade requires the configured API key as a query parameter; the browser hooks append it | `websocket_requires_api_key_when_configured` |
| F11 benchmark baseline selection | Fixed: scheduled/tag events fall back to `HEAD^`, and empty or all-zero refs are rejected | `bench-gate.sh --compare-ref ""` and zero-ref both exit 2 |

F08 remains open: desktop node-service parity, including the missing Tauri
commands above. F02's older-than-retention wallet discovery still needs a
birthday/checkpoint design.

Post-fix verification: node 228, torrent 98, migrate 18, BTC 75 (1 ignored),
RPC 148; frontend lint, 20 tests, production build, and the Playwright smoke
test; 4 Tauri library tests; workspace formatting and strict all-feature Clippy
pass; the live soak observer exits 0 with the hardened checks. The fleet was
rebuilt to `vtorrent/node:batch-e7691fb` (code `e7691fb`) and a fresh soak
window started at 2026-10-21T02:47:54Z. The review candidate `6402d87`
(`bab2456` plus the Playwright E2E setup and Vite dev-origin CORS allowance)
adds the BTC wallet birthday flag,
the desktop governance/earnings/payment/deadline commands, desktop health-field
updates, the shared BTC sync loop, desktop chain persistence, and the
cold-stake/BTC/compact-block/desktop hardening. The adversarial reviews are in
`docs/security-review-independent-2026-10-20.md` and
`docs/security-review-independent-2026-10-20-round2.md`; their probes are under
`docs/review-probes/security-2026-10-20/`. The round-2 fixes include the
burn-address removal, session-bound cold-stake funding, desktop cold-stake
commands, network-scoped BTC state, scan warnings, output-size fee estimation,
single-use WebSocket tokens, and the BTC scan checkpoint-retention fix.

## Review limits and self-assessment

No new finding claims arbitrary fund theft, remote code execution, or a
successful attack on the live fleet. Severity reflects the demonstrated
invariant violation and traced caller impact. Desktop and hosted CI findings
still need their respective runtime environments for end-to-end confirmation.
No fresh dependency audit, exhaustive fuzzing, scale soak, or external reviewer
sign-off is implied.

Earlier statements that all P0s were closed, all new alert metrics existed,
and green suites established broad readiness were too strong. This review
replaces those assertions with reproduced defects and explicit coverage limits.
Existing fixes such as auth layer ordering, normal stakeable-script checks,
signed votes, and proof budget/race tests remain useful, but do not establish
the missing boundary cases above.

The release candidate needs another remediation and verification pass. A
normal regtest staking soak cannot close the bootstrap, mainnet BTC pruning,
legacy recovery, or desktop contract gaps by elapsed time alone.
