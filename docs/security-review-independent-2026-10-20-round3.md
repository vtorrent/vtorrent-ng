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

### L-7 — Low (FIXED in `77744bf`): the desktop cold-stake path was not fully
runtime-tested

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
at height 2. All 15 Tauri library tests and strict Tauri Clippy pass.

**Component coverage update:** four tests in
`vtorrent-ui/src/pages/StakingPage.test.tsx` render the real page in jsdom and
exercise its Generate/Fund controls. They verify browser funding with a
passphrase, desktop-mode funding without a browser passphrase, non-positive
amount rejection without a backend call, and retry after a funding failure.
The successful cases assert the displayed raw script is passed to the backend
helper with the expected satoshi amount and show the returned transaction ID.
All 25 frontend tests, lint, and the production build pass. The full workspace
suite (`cargo test --workspace --all-features`) also passes at `86aae3a`.
Backend helpers are mocked in these component tests; the live browser and Tauri
journeys are covered separately below.

**Live browser coverage update:** the Playwright E2E starts a regtest daemon
with the test-only miner, imports and unlocks a throwaway wallet, funds it
through the faucet, creates a mock browser wallet, generates a P2CS pair in the
real Staking page, funds 1 VTR through the UI, mines a block, and verifies the
confirmed transaction contains the exact P2CS script and 1 VTR output. The RPC
CORS allowlist now includes the Vite dev origins (`http://127.0.0.1:3000`,
`http://localhost:3000`); sensitive endpoints still require the API key. The
test is opt-in (`VTORRENT_E2E_LIVE=1`) and requires
`cargo build -p vtorrent-daemon --features test-util`.

**Live Tauri desktop coverage update:** the actual Tauri app was launched under
`tauri-driver` with an isolated HOME and the Vite dev server. The first run
failed to create a wallet because the capability set was empty and
`@tauri-apps/api/path` was denied. `vtorrent-tauri/capabilities/default.json`
now grants `core:path:default` (least privilege), and a unit test asserts the
permission. A full run with `core:path:default` opened the wallet on testnet
(regtest), started the node, funded 100 VTR from a peer daemon, generated a P2CS
script in the real Staking page, funded 1 VTR through the UI, relayed the
transaction to the peer, mined block 2, and confirmed the P2CS output on the
Tauri node. Funding txid
`b7a4644f57f44a938ac490d2c932b98b451fcc2f1b29bc0113e3ecca029688a0`, block
`582db1225481ca797b7e9da5fb5ec0e218fceede4dcbc4313cb2a789b212fa95`, script
`6376a914e23afa1f9082341bb21043e28555f02c52c392bc88ac670400000000b17576a9143b5abb209554608490e909cb47205c794203b7b188ac68`.
`get_transactions` showed the send at height 2; the Tauri suite is now 15 tests.

**Desktop restart and durable registry update:** a restart test found that
`create_cold_stake` recorded the script only in memory; the durable registry
file was not written, so the script would be rejected after restart. `dfeace6`
persists the script hash with `cold_stake_registry::add`; the Tauri test clears
the in-memory set and funds through the durable registry. In the live app, the
script generated before restart was accepted after reopening the wallet and
chain at height 3, relayed to the peer, mined at height 4, and confirmed.
Funding txid
`6ffbdf5a699c2ee2b071e2f81974f4b5b294bc5e4a32a96153f7fe3583dd23a6`, block
`53bf985b8ea7d812699de38a3e57cec2ab5febf1989ef0947205f3721e93c975`, script
`6376a914d7c0c7db0717b076a8211fd700064d7129830a6888ac670400000000b17576a914ab3ecafe45c36986fcf2bb33e03fc00a0ac7578a88ac68`.

**Desktop parity audit update:** the same audit found and fixed six related
desktop gaps in `5b815c2`: the node reported `vtorrent-mainnet` on testnet
(`rpc_state.network` was never set); the BTC sync loop used mainnet
(`rpc_state.btc_network` was never set); the mempool was not persisted; the
staking intent was not persisted or auto-resumed; `create_wallet` overwrote an
existing wallet; and the Tauri cold-stake WIFs were plain `String`s. The node
now sets the network/regtest/BTC fields, reads `VTORRENT_BTC_PEER` for
testnet/regtest, loads `mempool.json` and saves it every 60 s and on exit,
persists `staking.json` and resumes it after unlock, refuses to overwrite an
existing wallet, and returns `Zeroizing<String>` WIFs. Live verification:
network `vtorrent-regtest`; mempool restored after restart (`mempool_size 1`);
staking auto-resumed (`enabled=true`); BTC SPV synced to regtest height 684
with a `bcrt1...` address using `VTORRENT_BTC_PEER=127.0.0.1:18448`.

**Wallet-path and IPC-error hardening update:** `803aada` confines
frontend-supplied wallet paths to the app data directory, rejects `..`
components, and applies the check to `create_wallet`, `open_wallet`, and
`import_legacy_wallet`. Legacy import now refuses to overwrite an existing
wallet. The Tauri crate and UI version are beta.3, and the UI renders the
backend error message via `formatInvokeError` instead of a generic fallback.
Live checks: an outside path returned "Wallet path must be inside the app data
directory"; creating over the existing wallet returned "A wallet already exists
at this path"; the create page displayed that message; `get_node_info.version`
is `2.0.0-beta.3`.

**Minor API parity update:** `77744bf` returns `owned` and `warning` from the
Tauri `start_staking`/`get_staking_status` commands, matching the RPC, and the
Staking page displays the warning when the selected address is not owned by the
hot wallet. The unused `stakingAddress`/`dataDir` arguments were removed from
the frontend `start_node` call. The Tauri suite remains 6 tests; the frontend
suite is 22 tests.

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

**Real BTC SPV swap update:** the ignored release test
`multi_node_htlc_swap_settles_across_real_btc_spv` passed against the real
`vtr-btc-regtest` bitcoind container and two VTR daemons. It settled a full
HTLC swap: VTR funding `d922bab48bb528b3e5ea57977ccc3fb5c1b44babb252641914a0bd2bc850a817`,
BTC funding `b448b78e3040ac24142eff43ed693b2d51c479a94bc46aeefd709fd9b1cb5d4c`,
BTC claim `578b16a969d3a77482d57b40c6a6b23d4ae3c232c1e14c048745571e96ae3586`,
and VTR claim `0d71f48be47ec1097b20861f31d97cf0b646e302124a16a5ea599938bc1097c3`.
This is regtest BTC SPV with real bitcoind, not a mainnet-scale
header/filter retention soak.

```bash
cargo test -p vtorrent-daemon --features test-util --test process_recovery \
  --release multi_node_htlc_swap -- --ignored --nocapture
```

## Residual gaps

- The `fund_cold_stake` registry does not defend against a fully compromised
  frontend that can call both endpoints.
- BTC SPV is disabled on the soak fleet; the real BTC SPV swap test is a
  separate regtest run, not a mainnet-scale soak.
- Mainnet-scale BTC SPV retention/scan soak.
- Frontier-model review follow-ups are fixed in `39da204` (SPV principal
  preservation), `f803cf9`/`afe5974` (BTC reservation release and serialization),
  and `2287026` (passphrase-retention docs); verified 2FA enrollment, the
  torrent control availability, and the reviewed send quote are in `86aae3a`.
  The external review should verify them.
- Independent external review of `86aae3a` (`ea92f22` plus the reviewed send
  quote).

## Recommended order

1. Run the mainnet-scale BTC SPV retention/scan soak and exercise the reorg
   path.
2. Commission the external review of `86aae3a` and rebuild the fleet on it.
3. Re-run the desktop build verification on the reviewed candidate.
