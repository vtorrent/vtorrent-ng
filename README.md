# vTorrent-NG

**vTorrent Next Generation** — A complete ground-up rewrite of the original vTorrent cryptocurrency client, built in Rust with a modern React/Tauri desktop interface.

> *Decentralized. Incentivized. Exchange-Free.*

---

## What is vTorrent-NG?

vTorrent-NG is the revival of the original [vTorrent (VTR)](https://github.com/vtorrent/vTorrent-Client) project — a cryptocurrency that integrated BitTorrent incentives directly into the blockchain client. The original project was delisted from Bittrex due to its association with torrent technology and later affected by exchange failures.

This rewrite preserves the original vision while delivering the features that were never fully completed:

- **In-client BitTorrent** — Upload and download torrents directly from the wallet. Earn VTR for seeding; spend VTR for faster downloads.
- **Built-in P2P Trading (DEX)** — Trade VTR directly with other users via atomic swaps (HTLCs). No exchange required, no exchange risk.
- **Legacy Wallet Recovery** — Import your old `wallet.dat` file and claim your original VTR balance on the new chain. Old holders are made whole.
- **TOTP 2FA** — The original vTorrent's unique wallet 2FA feature, modernized and enhanced with Argon2id + ChaCha20-Poly1305 encryption.

---

## Repository Structure

```
vtorrent-ng/
├── vtorrent-core/        # Cryptographic primitives, address encoding, network constants
├── vtorrent-wallet/      # Wallet management, key storage, TOTP 2FA, Argon2id encryption
├── vtorrent-migrate/     # Legacy wallet.dat parser (BerkeleyDB, AES-256-CBC decryption)
├── vtorrent-snapshot/    # Legacy blockchain UTXO snapshot extractor
├── vtorrent-node/        # Consensus engine, blockchain state, mempool, PoS logic
├── vtorrent-p2p/         # P2P networking layer (tokio async, custom message codec, PEX, DHT)
├── vtorrent-overlay/     # Kademlia overlay for NAT traversal and peer relay
├── vtorrent-spv/         # Simplified Payment Verification header chain + Bloom filter
├── vtorrent-rpc/         # Axum JSON-RPC server (wallet, node, SPV, DEX, torrent endpoints)
├── vtorrent-store/       # Persistent block store (redb key-value database)
├── vtorrent-daemon/      # Production daemon binary (P2P + RPC + staking)
├── vtorrent-tauri/       # Tauri IPC bridge (Rust backend ↔ React frontend)
└── vtorrent-ui/          # React + TypeScript + Tailwind CSS desktop UI
```

---

## Technology Stack

| Layer | Technology |
|---|---|
| **Core / Node** | Rust 1.74+ (safe, fast, no GC) |
| **Desktop Shell** | Tauri 2.x (native OS integration, ~10 MB binary) |
| **Frontend UI** | React 18 + TypeScript + Tailwind CSS |
| **Wallet Encryption** | Argon2id (KDF) + ChaCha20-Poly1305 (AEAD) |
| **2FA** | TOTP (RFC 6238) — compatible with Google Authenticator / Authy |
| **Consensus** | Proof-of-Stake (5% annual, 6h min stake age) |
| **P2P Protocol** | Custom binary framing over TCP (Bitcoin-compatible message format) |
| **Peer Discovery** | PEX (addr/getaddr) + Kademlia DHT + DNS seeds |
| **NAT Traversal** | Kademlia overlay relay network |
| **SPV** | Header chain + Bloom filter (BIP-37 compatible) |
| **DEX** | Atomic Swaps via HTLC (Hash Time-Locked Contracts) |
| **Torrent** | libtorrent-rasterbar via Rust FFI (planned) |

---

## Building from Source

### Prerequisites

- Rust 1.74+ (`rustup install stable`)
- Node.js 18+ and pnpm (`npm install -g pnpm`)
- System dependencies: `build-essential`, `pkg-config`, `libssl-dev`

### Build the Rust workspace

```bash
git clone https://github.com/pnoch/vtorrent-ng.git
cd vtorrent-ng
cargo build --workspace
cargo test --workspace
```

### Run the full daemon (recommended)

```bash
cargo run -p vtorrent-daemon -- --rpc-addr 127.0.0.1:22525 --listen 0.0.0.0:22526
```

**Testnet mode** (allows LAN/localhost multi-node testing):

```bash
cargo run -p vtorrent-daemon -- --testnet --listen 127.0.0.1:22526 --seed 127.0.0.1:22527
```

### Run the P2P node only (no RPC)

```bash
cargo run -p vtorrent-node
```

### Run the migration tool (legacy wallet.dat import)

```bash
cargo run -p vtorrent-migrate -- --wallet /path/to/wallet.dat --output keys.json
```

### Run the UI (development mode)

```bash
cd vtorrent-ui
pnpm install
pnpm dev
```

---

## RPC API

The daemon exposes a JSON-RPC HTTP server on `127.0.0.1:22525` by default.

| Method | Endpoint | Description |
|---|---|---|
| GET | `/api/v1/node/info` | Node status, height, peer count, sync state |
| GET | `/api/v1/wallet/info` | Wallet balance, addresses, lock state |
| POST | `/api/v1/wallet/send` | Send VTR to an address |
| GET | `/api/v1/mempool` | Pending transactions |
| GET | `/api/v1/peers` | Connected peer list |
| GET | `/api/v1/staking/status` | Staking engine status and rewards |
| POST | `/api/v1/staking/start` | Start staking with a given address |
| POST | `/api/v1/staking/stop` | Stop staking |
| GET | `/api/v1/spv/status` | SPV header chain height and tip hash |
| POST | `/api/v1/spv/headers` | Submit a batch of headers to the SPV chain |
| GET | `/api/v1/dex/orders` | Open DEX swap orders |
| POST | `/api/v1/dex/order` | Place a new swap order |
| DELETE | `/api/v1/dex/order/:id` | Cancel a swap order |
| POST | `/api/v1/legacy/check` | Look up a legacy address in the genesis snapshot |
| POST | `/api/v1/legacy/claim` | Sign and broadcast a legacy claim transaction |

---

## Legacy Wallet Claim

If you held VTR on the original chain, you can claim your balance on the new chain:

1. Open the vTorrent-NG client.
2. Click **"Import Legacy Wallet"** on the welcome screen.
3. Select your old `wallet.dat` file.
4. Enter your legacy passphrase (and TOTP code if 2FA was enabled).
5. The client will show your claimable balance from the genesis snapshot.
6. Sign and broadcast the claim transaction.

Your private keys never leave your machine. The claim process is fully local until the final broadcast.

---

## Roadmap

| Phase | Status | Description |
|---|---|---|
| **Core Crates** | ✅ Complete | Crypto, wallet, migration, snapshot, node, P2P |
| **UI Shell** | ✅ Complete | Welcome, dashboard, import wizard, security center, torrents, DEX, staking, legacy claim |
| **Tauri IPC Bridge** | ✅ Complete | Full command layer connecting UI to Rust backend |
| **Snapshot Tool** | ✅ Complete | Legacy LevelDB chainstate parser and UTXO extractor |
| **P2P Networking** | ✅ Complete | Peer handshake, message codec, peer manager, PEX, DHT, ban manager |
| **PoS Staking** | ✅ Complete | Coinstake creation, stake modifier, difficulty adjustment, staking UI |
| **Overlay / NAT** | ✅ Complete | Kademlia overlay, relay handler with real peer map |
| **SPV Client** | ✅ Complete | Header chain, Bloom filter, RPC endpoints |
| **Testnet Mode** | ✅ Complete | `--testnet` flag, private address PEX, end-to-end wiring |
| **Persistent Store** | ✅ Complete | redb block store, warm restart from disk |
| **BitTorrent Integration** | 📋 Planned | libtorrent-rasterbar FFI, seeding rewards, download payments |
| **Atomic Swap DEX** | 📋 Planned | HTLC on-chain execution, order matching engine |
| **Mainnet Launch** | 📋 Planned | Genesis snapshot, DNS seeds, public release |

---

## Original Project

The original vTorrent client is preserved at [vtorrent/vTorrent-Client](https://github.com/vtorrent/vTorrent-Client) for historical reference. The original chain launched in 2014 and ran until exchange delistings and failures forced it offline.

---

## License

MIT License — see [LICENSE](LICENSE) for details.

---

## Contact

- **GitHub**: [vtorrent](https://github.com/vtorrent)
- **Email**: vtorrent.crypto@gmail.com
- **BitcoinTalk**: [Original ANN Thread](https://bitcointalk.org/index.php?topic=889481.0)
