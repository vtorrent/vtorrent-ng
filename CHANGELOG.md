# Changelog

All notable changes to vTorrent will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — vTorrent 2.0.0

### Since v2.0.0-beta.2 (44 commits)

**Edge-Case Audit Fixes (session of 2026-08-30)**
- Legacy snapshot claims were rejected by the mempool relay fee floor (claims carry no fee by design) — every claim would have failed; now fee-exempt
- Faucet-minted blocks were never persisted to the block store (announced to peers but no `NewBlock` event) — restart hit a height gap and truncated the chain to genesis; now persisted via the event bridge
- `unlock_wallet` did not re-derive the change address after a daemon restart — send/balance failed until re-import
- `Transaction::serialized_size` omitted claim address/signature fields (~79 bytes), undercounting fee-rate and block-size accounting for claims
- `Chain::block_height` O(n) linear scan → O(1) map lookup (hot path for every tx lookup and getblock-by-hash RPC)
- `inv` capped at 1000 items, `getdata` at 500 (unbounded lists were a bandwidth-amplification DoS vector)
- `getblocktxn` O(height) scan → O(1) hash lookup
- `gettxout` RPC returned hardcoded `coinbase: false`; now derived from the transaction
- New: mempool TTL evicts transactions older than 48 h; `GET /api/v1/blockchain/utxo/:txid/:vout`; faucet per-address 10 s cooldown; P2P escalating bans for repeated connection failures (5 m → 15 m → 1 h)

**Mainnet Infrastructure**
- Three geographically distributed seed nodes deployed and verified: `vtr-seed1` (Falkenstein DE), `vtr-seed2` (Helsinki FI), `vtr-seed3` (Ashburn US); DNS seeds `seed1/2/3.vtorrent.org` live at IONOS
- Bootstrap surfaces active: hardcoded `BOOTSTRAP_PEERS`, GitHub-hosted `bootstrap/peers.txt` with CDN mirrors, DNS seeds
- Seed-fleet monitoring: Prometheus + Grafana + Alertmanager on vtr-seed1, IP-pinned metrics proxies, ntfy push alerts (daemon-down, peer-drop, height-stall, disk/RAM)
- Parameterized seed provisioning script (`deploy/provision-seed.sh`); on-call runbook (`docs/oncall-runbook.md`)
- `--isolated` bootstrap flag for local testnets; `--public-addr` for correct PEX self-filtering on 0.0.0.0 listeners

**Critical Node Fixes**
- Mempool eviction on block acceptance: confirmed + conflicted transactions are now removed — previously every staked block after the first spend was invalid network-wide
- Reorg persistence: abandoned blocks are rolled back on disk (`rollback_tip`) and fork blocks recorded; the store self-heals corrupt tails at startup instead of refusing to boot; genesis backfilled at store open
- Event-bridge lag reconciliation: dropped `NewBlock` events trigger a full store rebuild from the in-memory chain
- Release profile enables `overflow-checks` — consensus arithmetic now panics (rejecting the block) instead of silently wrapping

**Protocol & Networking Fixes**
- BIP-152 short-id collision handling on both sides (sender nonce retry, receiver misbehaviour scoring); pending compact blocks bounded
- `getblocktxn` requests carry the full-header block hash (the 6-field digest never matched, so missing-tx recovery always failed)
- `getblocks`/`getheaders` locator caps + O(height) lookup — closes an amplification DoS
- P2P payload cap 32 MB → 4 MB; node event queue bounded (flood OOM hardening)
- Seed re-dial every maintenance cycle; hostname resolution before peer dedup/ban checks
- CScriptNum semantics enforced for all arithmetic opcodes (4-byte operands, overflow rejection); CLTV/CSV accept 5 bytes per BIP-65/112

**Wallet & RPC Fixes**
- Imported wallets persist to `<data-dir>/wallet.json` (Argon2id + ChaCha20-Poly1305, 0600, atomic) and restore locked on startup
- Swap lifecycle guards: no double-funding, no claim-after-refund; swap state materializes at match time
- HTLC claim branch requires exactly-32-byte preimage (`OP_SIZE` guard)
- BTC txids reported in display-order hex (matches Bitcoin Core/explorers)
- Relay-floor fees enforced via `TxBuilder.min_absolute_fee`; raw-broadcast fees verified from the UTXO set; chain→mempool lock order documented and consistent (deadlock prevention)
- `validate_p2pkh` on staking/swap/faucet recipient paths — foreign-network addresses rejected
- SPV UTXO scan checkpoints only covered ranges and resumes to tip; SPV timestamp validation added

**Foundation Refactor (Phase B)**
- `node.rs` (2649L) split into `node/{mod,chain,p2p,mempool_bridge}.rs`; `handlers.rs` (2282L) split into `handlers/{mod,prelude,wallet,swap,torrent,staking}.rs`
- Genesis snapshot array (59k lines) → `include_bytes!` binary blob (2.4 MB)
- New `vtorrent-wallet-service` crate: single `build_payment` path for daemon/tauri (eliminates fee divergence)
- `Mempool::admit_with_chain_fee` helper deduplicates 4 admission sites

**Polish (Phase A)**
- P2P V2 bincode wire format (version-gated, JSON fallback) — 2–5× smaller blocks/txs
- Staking dashboard WebSocket push (instant status vs 5s polling)
- Torrent empty-state UX + deterministic progress (SHA1-verified bytes)

**Continuous Slices (Phase C)**
- Benchmark regression gate in CI (`scripts/bench-gate.sh`, >25% threshold, 23 committed baselines)
- `cargo machete` CI job for unused dependency detection
- Explorer/faucet deferral policy (`docs/explorer-faucet-policy.md`)
- Backup policy (`docs/backup-policy.md`)
- CLI BTC subcommands (`btc status|address|send`) — full RPC parity
- Graceful shutdown flushes mempool to disk
- AGENTS.md synced with current state (19 crates, 539 tests, 3 seeds, ops features)

**Testing & Verification**
- 595 workspace tests (was 523 at beta.2): reorg-persistence integration test, store self-heal/reconciliation tests, mempool-regression test (incl. claim fee exemption + stale eviction), fee-floor lock-step test, BIP-152 duplicate-id regression, swap-guard unit tests, genesis snapshot integrity test, node-split import test, wallet-service build_payment test, faucet-block persistence regression
- 25-hour fuzz marathon across all four targets: 60+ billion executions, zero crashes
- Upgrade/downgrade drill passed on a live soak node; full VTR↔BTC atomic swap E2E executed against BTC regtest (fund/claim/refund paths confirmed on-chain)
- cargo audit: zero vulnerabilities

### Since v2.0.0-beta.1

**Security Audit**
- Full codebase audit: 85 findings identified and resolved (5 Critical, 18 High, 32 Medium, 18 Low, 12 Info) across three fix batches plus a second-pass review of Medium/Low items
- Second full review (~40 additional findings fixed): torrent scheduler livelock (downloads of large pieces could never complete), bencode stack-overflow crash from any peer message, SPV header PoW validation (both chains), BTC wallet correctness batch, script-engine truncated-push consensus fix, BDB/snapshot parser fixes
- Overlay handshake redesigned: Noise-KK-style mutual authentication via static-key DH proofs — MITM/impersonation of any node_id no longer possible; per-IP handshake rate limiting; bounded endpoint registry
- RPC/CLI fabricated values eliminated — all reported data comes from real chain/wallet state
- Runtime staking control (`start`/`stop`) with live staking counters

**Performance**
- Criterion benchmark suite (`vtorrent-node/benches/consensus_hotpath.rs`): stake modifier ~67ns, kernel hash ~65ns, sighash 131–545ns, merkle root 173ns–23.5µs
- Incremental sighash via `Sha256::update()` — 56–91% faster than full-tx serialization, bit-identical output
- Static `LazyLock<Secp256k1<All>>` context; merkle root in-place reduction; consensus hot-path clone reduction

**BTC Wallet**
- Full send-to-address workflow with network broadcast (multi-peer fan-out, 3 concurrent)
- PSBT create/sign/finalize round-trip; P2TR (Taproot) addresses; Schnorr signing
- BIP69 input/output sorting, RBF signaling (correct `0xFFFFFFFD` sequence), multi-index signing with per-input witness pubkeys, UTXO persistence with broadcast-failure rollback
- Fee estimation with urgency multiplier; feefilter input clamping

**Testing**
- 523 workspace tests (was 424 at beta.1): BTC send-flow/HTLC integration tests, PoS multi-block staking tests, mempool-inclusion test, compact-block edge cases, ban-manager stress tests, authenticated-handshake end-to-end tests
- 4 libFuzzer targets (script engine, P2P codec, tx deser, PSBT); extended runs clean (55.9M executions on the script engine alone)

**Script Engine**
- 20+ new opcodes added; sign-magnitude `bytes_to_int` encoding fix matching Bitcoin semantics
- Truncated push-data now fails the script (was: silently evaluated whatever was on the stack)

**Networking**
- Compact block (BIP-152-style) encode/decode with SipHash short-id reconstruction
- PEX timestamps clamped; DHT responses validated by transaction id and source address; UDP tracker replies validated likewise
- Self-connection detection via sent-version-nonce registry; post-handshake idle timeout; pre-handshake message gating; bounded codec buffering

**Frontend / Desktop**
- All Tauri command stubs eliminated; 25+ IPC commands registered and wired
- BTC wallet page fully connected via `useBtc` hooks; DEX/staking result types aligned with backend

**Documentation**
- `docs/rpc-api.md`: complete reference for all 40+ endpoints
- `docs/atomic-swap-protocol.md`: cross-chain HTLC flow, timing parameters, failure modes
- `docs/wallet-recovery.md`: legacy `wallet.dat` migration and WIF import guide
- `docs/mainnet-readiness.md`: launch checklist (consensus verification, soak, seeds, release engineering)

**Infrastructure**
- Docker image verified end-to-end: multi-stage build fixed (workspace member copy, Rust ≥1.85 for edition2024 deps), single-node smoke + 3-node peered testnet with Prometheus/Grafana monitoring live
- `docker/testnet/docker-compose.yml`: reproducible local chain (3 VTR nodes + BTC regtest + monitoring)

**Fixed**
- `BtcWallet::send_to` MutexGuard deadlock (double-lock in insufficient-funds error path) that hung `cargo test --workspace`
- Ban-manager test threshold mismatch (100 vs 1000)
- Snapshot binary parsing: entries sorted after load (binary-search lookups returned 0 on unsorted files); chainstate varints use base-128 encoding per Bitcoin `serialize.h`

### Added

**Core Infrastructure**
- New Rust-based monorepo (`vtorrent-ng`) replacing the legacy C++/Qt codebase
- Cargo workspace with 17 crates: `vtorrent-core`, `vtorrent-wallet`, `vtorrent-migrate`, `vtorrent-snapshot`, `vtorrent-node`, `vtorrent-p2p`, `vtorrent-torrent`, `vtorrent-rpc`, `vtorrent-tauri`, `vtorrent-overlay`, `vtorrent-spv`, `vtorrent-store`, `vtorrent-daemon`, `vtorrent-script`, `vtorrent-onion`, `vtorrent-cli`, `vtorrent-btc`
- Full test suite: 523 tests, 0 failures; 4 libFuzzer targets with extended clean runs

**Overlay / NAT Traversal**
- `vtorrent-overlay`: Kademlia-style overlay network for NAT traversal and peer relay
- `EndpointRegistry`: real peer map wired into relay request handler — relays are now only accepted from known connected peers
- Relay requests from unknown peers are rejected with a structured error response

**SPV (Simplified Payment Verification)**
- `vtorrent-spv`: lightweight header chain with Bloom filter support for SPV clients
- SPV integrated into `vtorrent-rpc`: `GET /api/v1/spv/status` and `POST /api/v1/spv/headers` endpoints
- `AppState` carries a shared `Arc<RwLock<SpvChain>>` for concurrent access

**PEX Testnet Flag**
- `AddrBook::with_testnet(bool)` constructor — private/RFC1918 addresses accepted on testnet, rejected on mainnet
- `PeerManager::new_testnet()` and `PeerManager::with_testnet()` constructors
- `NodeConfig::testnet` field propagated end-to-end from CLI `--testnet` flag through `Node` to `PeerManager`
- `vtorrent-daemon` and `vtorrent-node` binaries both expose `--testnet` flag
- Startup log prints `Network: TESTNET` when testnet mode is active
- 6 new PEX tests covering mainnet/testnet address filtering and loopback rejection

**Desktop UI**
- `StakingPage.tsx`: full staking dashboard — status cards, start/stop controls, reward history, live polling
- `LegacyClaimPage.tsx`: 3-step claim wizard (address input → snapshot lookup → sign & broadcast)
- `useNode.tsx`: `useStakingStatus`, `startStaking`, `stopStaking`, `checkLegacyClaim`, `submitLegacyClaim` hooks
- `Layout.tsx`: Staking and Claim VTR nav items added; live node status panel with sync progress bar
- `App.tsx`: `/staking` and `/claim` routes added

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
- 60-second target block time
- 5% annual staking reward
- Minimum stake: 1 VTR, minimum age: 6 hours (maximum age: 6 days)
- Maximum supply: 20,000,000 VTR
- Deterministic genesis block with embedded legacy UTXO snapshot

**P2P Networking**
- Async TCP peer manager (Tokio-based)
- Bitcoin-compatible message framing (command + payload)
- Version handshake, `inv`/`getblocks`/`getdata` sync protocol
- DNS seed bootstrap: `seed1.vtorrent.io`, `seed2.vtorrent.io`, `seed3.vtorrent.io` (retired — see `bootstrap/peers.txt` for current bootstrap peers)

**BitTorrent Integration**
- Native BitTorrent client built into the wallet (`vtorrent-torrent` crate)
- `.torrent` file parsing (BEP-3 metainfo format), including piece-hash extraction
- Magnet link support, including BEP-9 `ut_metadata` / BEP-10 extension-protocol metadata fetch from peers
- HTTP tracker announce (BEP-3) and UDP tracker announce (BEP-15)
- Trackerless peer discovery via the Kademlia DHT (BEP-5): bootstrap, iterative `get_peers`, and `announce_peer`
- Peer wire protocol (BEP-3) with a full download/upload engine: tracker/DHT announce, peer connect, handshake, rarest-first multi-peer piece scheduling, 16 KiB block pipelining, endgame mode, SHA1 verification, disk write, resume support, and seeding
- **VTR incentive system**: earn VTR for seeding, pay VTR for priority download slots, with a periodic settlement loop
- `ut_vtr` BEP-10 extension exchanges VTR addresses between peers; settlement emits payment events that build and broadcast real VTR transactions
- Configurable seeding rate: VTR per GB uploaded
- Configurable download priority: VTR per GB downloaded

**Decentralized Trading (Atomic Swaps)**
- HTLC (Hash Time-Locked Contract) atomic swap implementation
- `OP_IF/OP_ELSE` script builder for cross-chain swaps
- Funding, claim, and refund transaction builders
- P2P order book with `SwapOrder` and `OrderStatus` types
- Built-in Bitcoin SPV wallet (`vtorrent-btc`): BIP39 mnemonic + BIP84 SegWit keys, header-chain sync, merkle proofs, UTXO tracking, and transaction building
- Bitcoin-side P2WSH HTLC primitives and a live header-sync + Bloom-filter UTXO scan
- Two-sided swap orchestration: BTC funding, VTR claim, BTC claim, and refund endpoints with an expiry sweep
- P2P order gossip via a `dexorder` message with flooding and deduplication
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
- **Network magic**: New `0x56 0x54 0x52 0x58` (`"VTRX"`); legacy was `0x19 0x3b 0x2f 0x5a`
- **P2P port**: `22526`; RPC port `22525`

### Removed

- Legacy C++/Qt codebase (archived in `vtorrent/vTorrent-Client`)
- Dependency on BerkeleyDB (replaced by pure-Rust BDB page parser)
- Dependency on OpenSSL (replaced by `ring` and `aes` crates)
- Dependency on Boost (replaced by Rust standard library and Tokio)
- Centralized exchange dependency (replaced by built-in atomic swap DEX)

### Fixed

**Wallet Security**
- Passphrase and TOTP are enforced on wallet unlock and every send — the RPC layer re-verifies the passphrase and, when 2FA is enabled, the 6-digit TOTP code before signing; wrong credentials return HTTP 401
- Wallet import encrypts the WIF with the passphrase (Argon2id + ChaCha20-Poly1305) and stores the TOTP secret; the wallet starts locked until the passphrase is verified

**Consensus**
- Block validation enforces the 20,000,000 VTR maximum supply — blocks that would mint beyond the cap are rejected; the chain tracks cumulative supply with rollback support

**DoS Hardening**
- Ban manager wired into the P2P message handler — invalid transactions/blocks, malformed payloads, and unknown commands score peers and trigger bans
- Per-peer message rate limiting (500 msgs / 10 s) bans flooders, protecting the node event loop
- Mempool conflict detection switched from an O(n²) scan to a spent-input index, making it O(1) per input
- Inbound connection cap (`MAX_INBOUND`) and a 10s handshake timeout prevent unauthenticated sockets from being held open indefinitely
- Relayed tx/block invs are no longer echoed back to the sender; `getblocks` responds to the requester only

**Correctness**
- 8-decimal denomination (`COIN = 100,000,000`) applied consistently across RPC, CLI, and UI
- Torrent info hash computed from the raw info-dict bytes (BEP-3), not the re-serialized struct
- Address decoding uses full-width base58 instead of `u128`, fixing addresses with large values
- Wallet sighash aligned with chain script verification (`Transaction::sighash` shared between wallet and node)
- PoS coinstake inputs are signed and the stake kernel is validated in the chain
- Legacy claims no longer double-count the snapshot supply; `validate_transaction` accepts no-input legacy claims
- Bitcoin SPV client: correct message length parsing, per-input sighash, full version handshake, and BIP-65 refund sequences
- Bitcoin SPV header sync no longer sends `filterload` (modern mainnet nodes disable BIP-37 and disconnect peers that send one)
- Torrent metainfo rejects zero piece length

**Atomic Swap / DEX**
- Bitcoin SPV UTXO scan via BIP-158 compact block filters (the modern, privacy-preserving alternative to BIP-37, which most mainnet nodes disable)
- Real BTC-side HTLC settlement: funding, claim, and refund transactions are built, signed, and broadcast
- Real VTR-side HTLC settlement: `vtr_claim` and `swap_refund` build, sign, and broadcast the claim/refund transactions
- Swap lifecycle UI (match / fund BTC / claim VTR / claim BTC / refund) in the Trade page
- `--btc-seed` / `VTORRENT_BTC_SEED` initializes the BTC SPV wallet in the daemon
- `--btc-regtest` / `--btc-peer` run the BTC SPV wallet against a local Bitcoin Core regtest node
- Regtest mode (`--regtest`) with a faucet endpoint (`POST /api/v1/faucet`) and mock clock (`POST /api/v1/debug/mocktime`) for local end-to-end testing
- Verified end-to-end: BTC fund/claim/refund and VTR fund/claim/refund all land in their respective mempools; BTC header sync verified against live mainnet

**CI & Tooling**
- CI jobs install GTK/WebKit system libraries so the workspace (including `vtorrent-tauri`) compiles
- Audit job generates `Cargo.lock` (gitignored) and grants `checks: write` so `rustsec/audit-check` can post its results
- Frontend: `@tauri-apps/api` dependency added (fixes `tsc`), eslint + typescript-eslint + react-hooks configured, `pnpm lint` passes clean
- Tauri build paths corrected for the crate-root config layout; desktop app compiles and bundles deb + rpm

### Security

- **CVE mitigations**: The legacy codebase was based on Bitcoin 0.8.x with ~10 years of unpatched vulnerabilities. The new codebase starts from a clean Rust foundation with no inherited CVEs.
- **Passphrase handling**: Passphrases are never stored in memory longer than needed; Argon2id derives the encryption key and the passphrase is zeroed immediately after.
- **Key isolation**: Private keys are handled exclusively in the Rust backend. The JavaScript frontend never receives key material.
- **RPC hardening**: CORS restricted to local origins; the hot wallet key is cleared when the unlock expires; legacy wallet import requires a real passphrase (no hardcoded fallback).
- **Parser hardening**: bounded allocations and length checks across bencode, GCS, bloom filter, snapshot, and BDB parsers; KDF iteration cap; unsupported derivation methods rejected instead of silently downgraded.
- **Key redaction**: `WalletKeyEntry` `Debug` output redacts the WIF private key.

---

## [1.x] — Legacy vTorrent (2014–2019)

The legacy vTorrent client was built on Bitcoin 0.8.x / PPCoin with a Qt5 GUI and custom HTML/JS interface. It was delisted from Bittrex in 2016 and subsequently lost exchange support. The blockchain data has been preserved and is included in the new chain's genesis snapshot.

[Unreleased]: https://github.com/vtorrent/vtorrent-ng/compare/HEAD...HEAD
