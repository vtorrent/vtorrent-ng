# 2.0.0 feature completion and verification plan

**Goal:** every advertised feature is implemented, tested, and documented before launch.
Launch timing follows completion, not the reverse. RC14 remains a checkpoint, not a release candidate.

## Workstream A: close accepted High and Medium defects

| Item | Acceptance | Test |
|---|---|---|
| Refund upgrade path | A pre-RC14 recovery record with a refund decodes as relayed; a persist cannot drop the relayed flag; retry sets it; record version 2 round-trips | Unit: v1->v2 migration, persist guard, retry flag |
| Peer eclipse | Outbound peers counted separately; bootstrap peers dialed periodically; DNS re-resolved when outbound is low | Unit: outbound accounting; integration: inbound-only peer set still dials seeds |
| SPV scan progress | A pass that hits its budget returns at a frame boundary and persists partial progress; restart resumes instead of restarting | Unit: budget return; integration: interrupted scan resumes |
| Filter-header trust | Decision recorded: either an independent observation source for claims, or BTC swaps are documented as requiring non-colluding peers | Design record plus tests for the chosen path |
| Disk-full writes | Persistent write failure pauses the torrent with an error instead of looping | Fault-injection test |
| Slow-loris piece ownership | A stalled piece is reassigned or fails the peer within the stall budget | Timing test with a non-serving peer |
| DEX gossip | Order announcements are authenticated and bound to terms; local orders broadcast; wire format matches the parser | Unit: signature/binding; integration: two-node order propagation |
| PEX amplification | `getaddr` responses are egress-charged or capped per interval | Unit: budget accounting |
| Node stop | Tauri command stops the node and releases locks so restore and wallet switching work in-session | Tauri test plus desktop E2E |
| Staking state | Lock clears staking display and intent; unlock resumes only the same wallet | Tauri/RPC test |
| Cold-stake recovery | Spending key can be exported and imported through the UI; backup/restore instructions match | Desktop E2E |
| 2FA restore downgrade | Restoring a pre-2FA backup over a 2FA wallet requires current-wallet authorization or is refused | Tauri test |
| Backup recovery data | A documented export/import path exists for BTC state, swap records, cold-stake keys | Tauri test and UI walkthrough |

## Workstream B: governance and deferred features

Decision required: are governance activation and state rent part of 2.0.0? If yes, add their own design, implementation, and test phases; if not, keep them explicitly out of the release notes and readiness claims.

## Workstream C: verification program

1. Unit and integration: full workspace, all features, `-D warnings`.
2. Property/fuzz: decoders (bincode, bencode, P2P frames, torrent metainfo, RPC JSON), with size and time bounds.
3. Real Bitcoin: four-bitcoind suite, HTLC swap, reorg across restart, filter sync, cross-node claim.
4. Fleet: three-node soak with torrents, swaps, staking, BTC SPV, plus fault injection (restart, disk full, peer churn).
5. Desktop: Playwright flows for create/open/import/send/swap/torrent/backup/restore/2FA on Linux; installed builds smoke-tested on Windows and macOS.
6. Legacy: real `wallet.dat` differential from the user; encrypted and unencrypted, compressed and uncompressed keys, WIF prefixes.
7. Platform matrix: four desktop targets on every candidate.
8. Soak: 168 hours on the exact final daemon, with per-node coverage and continuity report.

## Workstream D: release documentation

Executable runbook with tag and push commands, checksum pre-publication gate, seed bring-up including provisioning script and RPC key, checkpoint command with key. Release notes with unsigned disclosure, critical-finding history, known limitations, and the exact review scope. Security package naming the full delta and the AI-not-independent disclosure.

## Sequencing

1. Finish Workstream A small fixes and tests.
2. Decide Workstream B scope.
3. Run Workstream C on RC15, fix findings, repeat until a candidate passes with no Critical or High.
4. Start the final 168-hour window, then execute Workstream D and publish.
