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

# Run all tests (456 tests currently pass)
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
- **Async**: `tokio` (full features) for networking; BEP-5 Kademlia DHT and a
  custom UDP overlay (not libp2p) for peer discovery and NAT traversal.
- **Serialization**: `serde` + `serde_json` for RPC, `bincode` for binary wire formats.
- **Security**: private keys are handled exclusively in the Rust backend; the JS frontend
  never receives key material. Passphrases are zeroized immediately after key derivation.
- **No comments unless necessary** — follow existing code style; the codebase is lightly
  commented with `///` doc comments on public items only.

## Key Details

- **Network magic**: `0x56 0x54 0x52 0x32` (`"VTR2"`); legacy was `0x19 0x3b 0x2f 0x5a`.
- **Ports**: P2P `22526`, RPC `22525` (testnet uses the same defaults).
- **Consensus**: Proof-of-Stake, 60s target block time, 5% annual reward, min stake 1 VTR,
  min stake age 6 hours (max stake age 6 days), max supply 20,000,000 VTR.
- **Address format**: Base58Check prefix `70` (`V...`); WIF prefix `198` (`7...`).
- **Genesis**: deterministic, embeds a legacy UTXO snapshot (59,375 addresses,
  11,589,746.63 VTR) for old-holder claims.
- **DNS seeds**: `seed1.vtorrent.org` (Falkenstein DE) and `seed2.vtorrent.org`
  (Helsinki FI), managed at IONOS. Bootstrap peers also published via
  `bootstrap/peers.txt` (GitHub-hosted) and `BOOTSTRAP_PEERS`
  (see `docs/dns-seeds.md`).

## Performance

Benchmark suite in `vtorrent-node/benches/consensus_hotpath.rs` (criterion):

| Benchmark | Result |
|---|---|
| `compute_stake_modifier` | ~67 ns |
| `stake_kernel_hash` | ~65 ns |
| `check_stake_kernel` | ~62 ns |
| `compute_pos_reward` | ~2 ns |
| `build_stake_block` (1u/0tx) | ~26 ns |
| `build_stake_block` (50u/50tx) | ~7 µs |
| `chain_add_block` (coinbase-only) | ~3 µs |
| `merkle_root` (1 tx) | ~173 ns |
| `merkle_root` (10 tx) | ~2.4 µs |
| `merkle_root` (100 tx) | ~23.5 µs |
| `sighash` (1 input P2PKH) | ~131 ns |
| `sighash` (5 inputs) | ~221 ns |
| `sighash` (20 inputs) | ~545 ns |

Key optimizations:
- **Incremental sighash**: Hashes transaction fields directly via `Sha256::update()`
  instead of cloning + serializing the full transaction. 56-91% faster depending
  on input count. Verified bit-identical to bincode reference via unit test.
- **Static Secp256k1 context**: `LazyLock<Secp256k1<All>>` avoids per-engine RNG re-seeding.
- **Merkle root in-place reduction**: Single `compute_merkle_root_from_txids` with
  caller-provided scratch buffer. 7-10% faster than per-level Vec allocation.

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
