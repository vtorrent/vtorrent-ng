# Adversarial Security Review — Round 3 — 2026-10-20

Status: INTERNAL ADVERSARIAL REVIEW, NOT THE EXTERNAL GATE.
Reviewed revision: `569151f` (HEAD; code `137d3f8`; fleet image
`vtorrent/node:batch-137d3f8`).
Previous reports: `docs/security-review-independent-2026-10-20.md` and
`docs/security-review-independent-2026-10-20-round2.md`.

This pass re-tested the round-2 fixes, then attacked the new BTC scan
authentication, the WebSocket token flow, the cold-stake funding path, and the
persistent BTC wallet state across reorgs.

## Method

- Source trace of the changed paths: `authenticate_filter_headers`,
  `BtcWallet::sync`/`scan_utxos_bip158`, `create_ws_token`/`ws_handler`,
  `create_cold_stake`/`fund_cold_stake`, the Tauri commands, and the UI fund
  flow.
- Seven executable probes under
  `docs/review-probes/security-2026-10-20/`; **7/7 pass**.
- `cargo test --workspace --no-fail-fast` — exit 0; strict Clippy and fmt
  clean; frontend lint, 16 tests, and build pass.
- Fleet state: 3/3 nodes on `batch-137d3f8`, height 53,594, same tip,
  `syncing=false`, staking, 0 errors/restarts.

Limitations: no Semgrep/CodeQL, no fuzzing, no live mainnet-scale BTC sync, no
GUI runtime test. Findings are source-traced; the existing BTC swap tests use a
fresh `BtcSync` and do not cover the persistent wallet store.

## Fix verification (round 2)

- **C-3** — the P2SH-hash-as-P2PKH address is gone from `ColdStakeKeys`, the
  RPC response, and the UI. The panel shows the raw script/script hash and
  funds it through `fund_cold_stake`. The probe reconstructs the old address to
  document the bug.
- **H-4** — `create_cold_stake` records the script in a session set;
  `fund_cold_stake` rejects scripts not in that set and caps the locktime.
- **H-5** — Tauri `create_cold_stake`/`fund_cold_stake` and the desktop fund
  flow are present.
- **M-5** — the desktop `Lagged` handler checks the store tip/hash and skips
  needless rebuilds.
- **M-6** — BTC UTXO state is network-scoped.
- **M-7** — the scan authenticates from a stored previous header or the next
  checkpoint; the fallback authenticates from the last checkpoint below the
  start. The skip is gone for the common cases.
- **L-2/L-3/L-4** — output-size fee estimation, `Cache-Control: no-store`, and
  single-use WebSocket tokens are present.

## Findings

### M-8 — Medium (FIXED in `7f096c0`): the persistent BTC wallet did not reset
state on a reorg

**Where:** `vtorrent-btc/src/wallet.rs:388-402` (`sync` uses the persistent
`filter_headers` and `utxos`); `vtorrent-btc/src/filters.rs`
(`observe_candidate` checkpoint/record conflict checks);
`vtorrent-btc/src/headers.rs` (`add_header` reorgs the header chain).

`BtcWallet::sync` reuses the wallet’s persistent `filter_headers` store, UTXO
set, and `last_scanned_height` across header reorgs. `HeaderChain::add_header`
can switch to a higher-work fork, but the wallet does not clear or rewind the
filter store, UTXO set, or scan cursor.

Consequences:

- If a reorg crosses a 1,000-block checkpoint, `observe_candidate` can reject
  the new chain’s checkpoint against the stored old-chain checkpoint, and the
  scan fails until the process restarts (the store is in-memory).
- UTXOs recorded from reorged-out blocks remain in the wallet until a rescan.
- `last_scanned_height` can be beyond the fork point, so the new chain’s
  replacement blocks below that height are not rescanned.

**Impact:** wrong BTC balance after a reorg, or a persistent scan failure until
restart. The swap tests use a fresh `BtcSync` per observation, so they do not
cover this path.

**Fix applied:** `BtcWallet::sync` now compares the pre-sync best hash with the
post-sync chain. If the old tip is no longer on the main chain, it finds the
common ancestor, clears the filter store, resets the UTXO set (and its persisted
file), and resets `last_scanned_height` to 0 so the rescan covers outputs below
the fork. The fork height is logged. Regression:
`reorg_fork_point_is_common_ancestor`.

### M-9 — Medium (FIXED in `2e3fe1e`): cold-stake funding was bound to an
in-memory session

**Where:** `vtorrent-rpc/src/state.rs` (`cold_stake_scripts`);
`vtorrent-tauri/src/state.rs` (`cold_stake_scripts`);
`vtorrent-rpc/src/handlers/wallet.rs` (`create_cold_stake`/`fund_cold_stake`).

`create_cold_stake` records the generated script in a process-local set;
`fund_cold_stake` requires it. A restart loses the set, so an RPC client that
generates a key pair, restarts the node, and then funds the script is rejected.
The UI generates and funds in one session, so the desktop path works.

**Impact:** availability/UX, not fund loss. It also means the binding is not a
durable authorization record.

**Fix applied:** `vtorrent-wallet::cold_stake_registry` persists only the
script hash next to the encrypted wallet (atomic, 0600 on Unix). Both the RPC
and Tauri `fund_cold_stake` accept a script registered durably or in the
current session. The spending key is still never persisted. Regression:
`registry_round_trips`.

### M-10 — Medium/Low (FIXED in `7f096c0`): the scan fallback can fail closed
near the tip

**Where:** `vtorrent-btc/src/sync.rs` (`authenticate_filter_headers`).

When the scan start is near the tip, there is no checkpoint in range, and no
stored previous filter header, the code authenticates from the last checkpoint
below the start. If that checkpoint is below the retained floor,
`hashes_from` returns a short range and the scan fails closed.

**Impact:** a fresh scan with a birthday near the tip and a small retention can
fail; the user must raise retention or rescan from an earlier height.

**Fix applied:** the requirement is documented on `scan_utxos_bip158` and in
the `--btc-header-retention` help: a start near the tip with no stored previous
header needs a checkpoint in range; if it is below the retained floor, raise
retention or rescan from an earlier height.

### L-5 — Low (FIXED in `b3028df`): WebSocket token map growth

**Where:** `vtorrent-rpc/src/ws.rs` (`create_ws_token`).

Tokens are pruned on each issue and expire after 60 seconds, but the map has no
hard cap. An API-key-authenticated caller can create many tokens in a minute.

**Fix applied:** the map is capped at 1,024 entries; the oldest expiry is
evicted before inserting. Regression: `token_map_is_capped`.

### L-6 — Low (FIXED in `2e3fe1e`): the cold spending WIF was in the JSON
response body

**Where:** `vtorrent-rpc/src/handlers/wallet.rs` (`ColdStakeResp`).

`Cache-Control: no-store` is set, but the response body still contains the
spending WIF. A tracing layer that logs bodies, or a browser cache outside the
header’s control, would expose it.

**Fix applied:** `Cache-Control: no-store` and `Pragma: no-cache` are set, and
the WIF fields use `Zeroizing<String>` in both the RPC and Tauri responses. The
body still contains the WIF by design (shown once); avoid body logging.

### L-7 — Low (IPC fixed in `6cdcfb8`; component coverage added): the desktop
cold-stake path was not fully runtime-tested

**Where:** `vtorrent-ui/src/hooks/useNode.tsx` (`createColdStake`/
`fundColdStake`); `vtorrent-tauri/src/commands/wallet.rs`.

The Tauri `create_cold_stake` command takes an optional `label`; the UI omits
it. At the reviewed revision there was no Tauri runtime test for the argument
mapping or the wallet-unlocked path.

**Verification update:** the Tauri IPC test invokes `create_cold_stake` with the
frontend's omitted optional `label` and `fund_cold_stake` with camelCase
`scriptPubkey`/`amountSatoshis`. It verifies unregistered-script rejection,
admission of the signed payment to the real mempool, a 1 VTR output to the
generated raw P2CS script, submission of the same transaction ID to the
peer-relay channel, and inclusion in a regtest block with the output confirmed
at height 2. All four Tauri library tests and strict Tauri Clippy pass.

**Component coverage update:** four tests in
`vtorrent-ui/src/pages/StakingPage.test.tsx` render the real page in jsdom and
exercise its Generate/Fund controls. They verify browser funding with a
passphrase, desktop-mode funding without a browser passphrase, non-positive
amount rejection without a backend call, and retry after a funding failure.
The successful cases assert the displayed raw script is passed to the backend
helper with the expected satoshi amount and show the returned transaction ID.
All 20 frontend tests, lint, and the production build pass. Backend helpers are
mocked in these component tests; a live desktop/browser session remains
unverified.

**Two-node coverage update (working tree):**
`cold_stake_funding_propagates_and_confirms_on_two_daemons` starts two isolated
daemon processes with separate data directories. The sender generates and
funds a raw P2CS script through RPC. The receiver obtains the transaction over
P2P without an RPC transaction submission and confirms it using the test-only
regtest miner. The sender then follows the receiver's block. After both
processes shut down, both stores replay to the same tip and contain the exact
1 VTR P2CS output at height 2. This covers real P2P transaction/block relay and
persistent confirmation, not production PoS block construction or one
end-to-end GUI-to-network session.

```bash
cargo test -p vtorrent-daemon --features test-util --test process_recovery \
  cold_stake_funding_propagates_and_confirms_on_two_daemons -- --nocapture
```

The targeted test passes; strict daemon Clippy and workspace formatting pass.

## Residual gaps

- The `fund_cold_stake` registry does not defend against a fully compromised
  frontend that can call both endpoints.
- BTC SPV is disabled on the soak fleet; the swaps and the BTC reorg path are
  not exercised by the current run.
- Mainnet-scale BTC SPV retention/scan soak.
- A live desktop/browser session remains (L-7); component interactions,
  command/IPC execution, two-daemon P2P propagation, and regtest confirmation
  are tested separately.
- Desktop restart, peer-status, and BTC discovery runtime tests.
- Independent external review of `481eb39`.

## Recommended order

1. **L-7** — live desktop/browser session check.
2. Run the mainnet-scale BTC SPV retention/scan soak and exercise the reorg
   path.
3. Commission the external review of `481eb39` and rebuild the fleet on it.
