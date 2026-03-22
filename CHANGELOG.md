# Changelog

All notable changes to vTorrent will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — vTorrent 2.0.0

### Added

**Core Infrastructure**
- New Rust-based monorepo (`vtorrent-ng`) replacing the legacy C++/Qt codebase
- Cargo workspace with 8 crates: `vtorrent-core`, `vtorrent-wallet`, `vtorrent-migrate`, `vtorrent-snapshot`, `vtorrent-node`, `vtorrent-p2p`, `vtorrent-torrent`, `vtorrent-rpc`, `vtorrent-tauri`
- Full test suite: 97 tests, 0 failures

**Cryptography & Security**
- Modern wallet encryption: Argon2id key derivation + ChaCha20-Poly1305 AEAD (replacing old AES-256-CBC + scrypt)
- TOTP 2FA (RFC 6238) preserved and enhanced from the legacy client — compatible with Google Authenticator and Authy
- FIDO2/WebAuthn support planned for hardware security keys (YubiKey, Ledger)
- All private key operations confined to the Rust backend; JavaScript never touches key material

**Legacy Migration**
- `vtorrent-migrate`: Pure-Rust BerkeleyDB page parser — reads legacy `wallet.dat` files without requiring a BDB installation
- Supports encrypted wallets (AES-256-CBC + scrypt, matching the legacy `crypter.cpp`)
- Supports stealth address key extraction (`sxAddr`, `sxKeyMeta` record types)
- TOTP secret migration: detects `keyOTP` records in legacy wallets

**Blockchain Snapshot**
- Complete UTXO snapshot extracted from the legacy blockchain (block height 1,680,456, 2018-01-10)
- **59,375 legacy addresses** with their final VTR balances preserved
- **11,589,746.63 VTR** total supply captured
- Snapshot embedded in the new genesis block — old holders can claim directly in the new client
- Binary snapshot (2.4 MB) embedded in the Tauri binary for O(log n) offline balance lookup

**New Chain (vTorrent 2.0)**
- Proof-of-Stake consensus (PPCoin-style, modernized)
- 10-second target block time
- 5% annual staking reward
- Minimum stake: 100 VTR, minimum age: 30 days
- Maximum supply: 20,000,000 VTR
- Deterministic genesis block with embedded legacy UTXO snapshot

**P2P Networking**
- Async TCP peer manager (Tokio-based)
- Bitcoin-compatible message framing (command + payload)
- Version handshake, `inv`/`getblocks`/`getdata` sync protocol
- DNS seed bootstrap: `seed1.vtorrent.io`, `seed2.vtorrent.io`, `seed3.vtorrent.io`

**BitTorrent Integration**
- Native BitTorrent client built into the wallet (`vtorrent-torrent` crate)
- `.torrent` file parsing (BEP-3 metainfo format)
- Magnet link support
- HTTP tracker announce/scrape (BEP-15)
- Peer wire protocol (BEP-3)
- **VTR incentive system**: earn VTR for seeding, pay VTR for priority download slots
- Configurable seeding rate: VTR per GB uploaded
- Configurable download priority: VTR per GB downloaded

**Decentralized Trading (Atomic Swaps)**
- HTLC (Hash Time-Locked Contract) atomic swap implementation
- `OP_IF/OP_ELSE` script builder for cross-chain swaps
- Funding, claim, and refund transaction builders
- P2P order book with `SwapOrder` and `OrderStatus` types
- No exchange required — trade VTR directly peer-to-peer

**HTTP RPC API**
- Axum-based JSON-RPC server (port 22525)
- Endpoints: wallet info, balance, addresses, transactions, staking status, mempool, peers, torrent management, swap orders
- CORS configured for local UI access
- Authentication via API key header

**Desktop UI (Tauri + React)**
- React 18 + TypeScript + Tailwind CSS frontend
- Tauri 2.0 desktop packaging (macOS, Windows, Linux)
- 6 screens: Welcome, Create Wallet, Import Wizard, Dashboard, Torrents, P2P Trade, Security Center
- Import wizard: drag-and-drop `wallet.dat`, passphrase entry, live snapshot balance lookup
- Security center: TOTP 2FA setup with QR code, encryption details
- Real Tauri `invoke()` IPC calls with browser dev-mode mock fallback

**CI/CD**
- GitHub Actions workflow: Rust tests on every push, desktop builds on version tags
- Multi-platform builds: macOS (Intel + Apple Silicon), Windows (x64), Linux (deb + AppImage)
- Auto-release to GitHub Releases on version tags

### Changed

- **Address format**: Legacy `V...` addresses (Base58Check prefix 75) are preserved in the snapshot; new chain uses the same prefix for continuity
- **WIF format**: Legacy `7...` WIF keys (prefix 203) are imported and re-encoded for the new chain
- **Network magic**: Preserved from legacy (`0x22 0x05 0x35 0x70`) for potential bootstrap compatibility
- **P2P port**: Preserved at `22524`

### Removed

- Legacy C++/Qt codebase (archived in `vtorrent/vTorrent-Client`)
- Dependency on BerkeleyDB (replaced by pure-Rust BDB page parser)
- Dependency on OpenSSL (replaced by `ring` and `aes` crates)
- Dependency on Boost (replaced by Rust standard library and Tokio)
- Centralized exchange dependency (replaced by built-in atomic swap DEX)

### Security

- **CVE mitigations**: The legacy codebase was based on Bitcoin 0.8.x with ~10 years of unpatched vulnerabilities. The new codebase starts from a clean Rust foundation with no inherited CVEs.
- **Passphrase handling**: Passphrases are never stored in memory longer than needed; Argon2id derives the encryption key and the passphrase is zeroed immediately after.
- **Key isolation**: Private keys are handled exclusively in the Rust backend. The JavaScript frontend never receives key material.

---

## [1.x] — Legacy vTorrent (2014–2019)

The legacy vTorrent client was built on Bitcoin 0.8.x / PPCoin with a Qt5 GUI and custom HTML/JS interface. It was delisted from Bittrex in 2016 and subsequently lost exchange support. The blockchain data has been preserved and is included in the new chain's genesis snapshot.

[Unreleased]: https://github.com/vtorrent/vtorrent-ng/compare/HEAD...HEAD
