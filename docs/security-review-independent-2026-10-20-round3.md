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

### M-8 — Medium: the persistent BTC wallet does not reset state on a reorg

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

**Fix:** in `BtcWallet::sync`, compare the pre-sync best hash with the post-sync
chain. If the old tip is no longer in the chain, find the common ancestor,
clear the filter store, reset the UTXO set, set `last_scanned_height` to the
fork point, and rescan. If the fork point is below the retained floor, reset to
the floor and surface a clear “rescan with higher retention” error.

### M-9 — Medium: cold-stake funding is bound to an in-memory session

**Where:** `vtorrent-rpc/src/state.rs` (`cold_stake_scripts`);
`vtorrent-tauri/src/state.rs` (`cold_stake_scripts`);
`vtorrent-rpc/src/handlers/wallet.rs` (`create_cold_stake`/`fund_cold_stake`).

`create_cold_stake` records the generated script in a process-local set;
`fund_cold_stake` requires it. A restart loses the set, so an RPC client that
generates a key pair, restarts the node, and then funds the script is rejected.
The UI generates and funds in one session, so the desktop path works.

**Impact:** availability/UX, not fund loss. It also means the binding is not a
durable authorization record.

**Fix:** persist only the script hash (not the spending key) in the encrypted
wallet or a dedicated 0600 file, and require `fund_cold_stake` to match it.
Alternatively, document the one-session requirement in the RPC reference.

### M-10 — Medium/Low: the scan fallback can fail closed near the tip

**Where:** `vtorrent-btc/src/sync.rs` (`authenticate_filter_headers`).

When the scan start is near the tip, there is no checkpoint in range, and no
stored previous filter header, the code authenticates from the last checkpoint
below the start. If that checkpoint is below the retained floor,
`hashes_from` returns a short range and the scan fails closed.

**Impact:** a fresh scan with a birthday near the tip and a small retention can
fail; the user must raise retention or rescan from an earlier height.

**Fix:** document the requirement, or retain the checkpoint filter header and
the block headers from the floor so the fallback can authenticate the retained
range.

### L-5 — Low: WebSocket token map growth

**Where:** `vtorrent-rpc/src/ws.rs` (`create_ws_token`).

Tokens are pruned on each issue and expire after 60 seconds, but the map has no
hard cap. An API-key-authenticated caller can create many tokens in a minute.

**Fix:** cap the map (for example 1,024 entries) and reject or evict oldest
entries.

### L-6 — Low: the cold spending WIF is still in the JSON response body

**Where:** `vtorrent-rpc/src/handlers/wallet.rs` (`ColdStakeResp`).

`Cache-Control: no-store` is set, but the response body still contains the
spending WIF. A tracing layer that logs bodies, or a browser cache outside the
header’s control, would expose it.

**Fix:** avoid body logging on this route, zeroize the response buffer after
serialization, and keep the “shown once” warning.

### L-7 — Low: the desktop cold-stake path is not runtime-tested

**Where:** `vtorrent-ui/src/hooks/useNode.tsx` (`createColdStake`/
`fundColdStake`); `vtorrent-tauri/src/commands/wallet.rs`.

The Tauri `create_cold_stake` command takes an optional `label`; the UI omits
it. The command and the fund flow compile, but there is no Tauri runtime test
for the argument mapping or the wallet-unlocked path.

**Fix:** add a Tauri command test or a manual desktop check that generates,
funds, and confirms the raw P2CS output.

## Residual gaps

- The `fund_cold_stake` session binding does not defend against a fully
  compromised frontend that can call both endpoints.
- BTC SPV is disabled on the soak fleet; the swaps and the BTC reorg path are
  not exercised by the current run.
- Mainnet-scale BTC SPV retention/scan soak.
- Desktop restart, peer-status, BTC discovery, and two-node propagation runtime
  test.
- Independent external review of `137d3f8`.

## Recommended order

1. **M-8** — reorg-safe BTC wallet state (clear/rewind filter store, UTXOs, and
   scan cursor).
2. **M-9/M-10** — durable cold-stake script binding and the scan fallback
   documentation/retention.
3. **L-5/L-6/L-7** — token map cap, WIF response hygiene, desktop runtime test.
4. Commission the external review and run the mainnet-scale BTC SPV soak.
