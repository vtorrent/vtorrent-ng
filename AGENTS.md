# AGENTS.md

Guidance for AI agents and contributors working on the vTorrent-NG codebase.

## Project Overview

vTorrent-NG is a ground-up Rust rewrite of the vTorrent (VTR) cryptocurrency client — a
Proof-of-Stake blockchain that integrates BitTorrent incentives, a built-in atomic-swap DEX,
and legacy wallet recovery. Desktop UI is React + TypeScript + Tailwind, packaged with Tauri 2.

## Repository Layout

This is a Cargo workspace with 16 crates plus a frontend:

| Crate | Purpose |
|---|---|
| `vtorrent-core` | Crypto primitives, address encoding, network constants |
| `vtorrent-wallet` | Wallet management, key storage, TOTP 2FA, Argon2id encryption |
| `vtorrent-migrate` | Legacy `wallet.dat` parser (BerkeleyDB, AES-256-CBC) |
| `vtorrent-snapshot` | Legacy blockchain UTXO snapshot extractor |
| `vtorrent-node` | Consensus engine, blockchain state, mempool, PoS logic |
| `vtorrent-p2p` | P2P networking (tokio, message codec, PEX, DHT, ban manager) |
| `vtorrent-overlay` | Kademlia overlay for NAT traversal and peer relay |
| `vtorrent-spv` | SPV header chain + Bloom filter |
| `vtorrent-rpc` | Axum JSON-RPC server (wallet, node, SPV, DEX, torrent) |
| `vtorrent-store` | Persistent block store (redb) |
| `vtorrent-daemon` | Production daemon binary (P2P + RPC + staking) |
| `vtorrent-tauri` | Tauri IPC bridge (Rust backend ↔ React frontend) |
| `vtorrent-script` | Bitcoin-style script engine (opcodes, standard scripts) |
| `vtorrent-onion` | Tor / I2P privacy transport |
| `vtorrent-cli` | Command-line client |
| `vtorrent-torrent` | BitTorrent client (metainfo, tracker, peer wire, incentives) |
| `vtorrent-ui` | React + TypeScript + Tailwind frontend (not a crate) |

## Build & Test Commands

```bash
# Build the whole workspace
cargo build --workspace

# Run all tests (320 tests currently pass)
cargo test --workspace

# Formatting and linting (enforced in CI)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the full daemon
cargo run -p vtorrent-daemon -- --rpc-addr 127.0.0.1:22525 --listen 0.0.0.0:22526

# Testnet mode (LAN/localhost multi-node testing)
cargo run -p vtorrent-daemon -- --testnet --listen 127.0.0.1:22526 --seed 127.0.0.1:22527

# Frontend
cd vtorrent-ui && pnpm install && pnpm dev
cd vtorrent-ui && pnpm lint
```

## Conventions

- **Rust edition 2021**, workspace dependencies declared centrally in the root `Cargo.toml`
  under `[workspace.dependencies]`. Add new shared deps there, not in individual crates.
- **Error handling**: `thiserror` for library error enums, `anyhow` for binaries.
- **Logging**: `tracing` + `tracing-subscriber` (never `println!` in library code).
- **Async**: `tokio` (full features) for networking; `libp2p` for overlay/DHT.
- **Serialization**: `serde` + `serde_json` for RPC, `bincode` for binary wire formats.
- **Security**: private keys are handled exclusively in the Rust backend; the JS frontend
  never receives key material. Passphrases are zeroized immediately after key derivation.
- **No comments unless necessary** — follow existing code style; the codebase is lightly
  commented with `///` doc comments on public items only.

## Key Details

- **Network magic**: `0x22 0x05 0x35 0x70` (preserved from legacy).
- **Ports**: P2P `22524`, RPC `22525`; testnet P2P `32524`, RPC `32525`.
- **Consensus**: Proof-of-Stake, 10s target block time, 5% annual reward, min stake 100 VTR,
  min age 30 days, max supply 20,000,000 VTR.
- **Address format**: Base58Check prefix `75` (`V...`); WIF prefix `203` (`7...`).
- **Genesis**: deterministic, embeds a legacy UTXO snapshot (59,375 addresses,
  11,589,746.63 VTR) for old-holder claims.
- **DNS seeds**: none currently (the legacy `seed1/2/3.vtorrent.io` domains are
  retired). Bootstrap peers are added via `bootstrap/peers.txt` (GitHub-hosted)
  or `BOOTSTRAP_PEERS` once new seed nodes are deployed (see `docs/dns-seeds.md`).

## CI

GitHub Actions (`.github/workflows/build.yml`) runs on every push/PR:
1. `cargo test --workspace --all-features`
2. `cargo fmt --all -- --check` + `cargo clippy ... -D warnings`
3. `cargo audit` (weekly + on push)
4. Desktop builds (Linux/macOS/Windows) only on `v*` tags.

## Never Commit

- Wallet/key files (`*.dat`, `*.wallet`, `*.key`, `*.pem`) — gitignored.
- Snapshot binaries (`*.snapshot`, `*.snap`) — distributed separately.
- `Cargo.lock` is gitignored (workspace library pattern).
