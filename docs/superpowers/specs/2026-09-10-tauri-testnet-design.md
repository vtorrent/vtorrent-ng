# Tauri Testnet Support — Design (2026-09-10)

## Goal
Let the desktop app join a testnet (e.g. the soak fleet) instead of only
running an embedded mainnet node.

## Non-goals
- No regtest exposure in the app. No mainnet/testnet wallet sharing.
- No hot network switching (node starts at unlock; switch = lock, pick, unlock).

## Backend (`vtorrent-tauri`)
- `start_node(state, network: Option<String>, seeds: Option<Vec<String>>)`:
  `"testnet"` selects the **soak-compatible regtest chain**
  (`regtest + regtest_fast_stake`, i.e. chain ID `vtorrent-regtest`) with
  `testnet: true` PEX behavior, datadir `~/.vtorrent/testnet`, and
  `extra_seeds` from param. (`testnet: true` alone would select the mainnet
  chain and reject soak blocks — hence regtest underneath.)
  Anything else/omitted → current mainnet default.
- `DEFAULT_TESTNET_SEEDS: &[&str]` (empty until phase-B infra lands) used as
  fallback when the user provides no seeds.
- `NodeHandle` gains `network: String` (canonical chain ID); running node on
  a different network → `NodeError`, restart required.

## Frontend (`vtorrent-ui`)
- `useNetwork()` hook: `'mainnet' | 'testnet'`, localStorage `vtr-network`,
  default mainnet.
- Welcome screen: segmented Mainnet/Testnet picker above the action cards;
  when testnet, a seed-peer input (comma-separated `host:port`, optional,
  placeholder `127.0.0.1:22526`).
- `unlock`/`createWallet` pass `{ network, seeds }` into `start_node`.
  Browser mock accepts and ignores the new args.
- Layout: amber `TESTNET` badge next to sync status whenever the connected
  network is not mainnet.

## Testing
- Rust: start_node twice with different networks → second errors; same
  network → returns current info. (Unit-testable only with constructed
  AppState — follow existing Tauri test patterns or test the network-mapping
  helper if extracted.)
- UI: vitest for seed-list parsing helper; lint/tsc; screenshot welcome
  picker + badge.
