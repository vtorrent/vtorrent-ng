# BTC SPV Wallet — Design

Date: 2026-08-18
Status: Approved

## Goal

Add a built-in Bitcoin (BTC) SPV wallet to the vTorrent client so users can
manage BTC directly from the desktop app. This is the first sub-project of the
cross-chain atomic-swap DEX roadmap feature; the BTC wallet is the foundation
the BTC leg of a swap depends on.

## Scope

This sub-project delivers a functional BTC wallet with:

- BIP39 mnemonic + BIP32 HD key derivation, shared with the VTR wallet via a
  single seed.
- BIP84 native SegWit addresses (`m/84'/0'/0'/0/x` → `bc1q...`).
- A minimal Bitcoin P2P client for header sync and transaction broadcast.
- SPV UTXO tracking and merkle-proof confirmation.
- Transaction building, signing, and broadcasting.
- RPC endpoints, Tauri commands, and a UI page.

Out of scope (later sub-projects): the cross-chain HTLC protocol, P2P order
gossip, and the DEX lifecycle. Those build on top of this wallet.

## Decisions

| Topic | Decision |
|---|---|
| Counterparty chain | Bitcoin |
| BTC integration | Built-in SPV (no external node) |
| SPV depth | Full SPV wallet (general-purpose, not swap-only) |
| Key management | HD upgrade to the VTR wallet; a single mnemonic is stored in the wallet and used as the seed for BTC derivation (VTR addresses remain random-key based for now) |
| Existing wallet migration | Opt-in (non-HD wallets keep working) |
| BTC address standard | BIP84 native SegWit |
| Networking | `rust-bitcoin` ecosystem crates |
| Architecture | New `vtorrent-btc` crate + minimal HD layer in `vtorrent-wallet` |

## Architecture

### 1. HD layer in `vtorrent-wallet`

The existing VTR wallet is non-HD: it stores random keys and legacy WIF imports
with no mnemonic or seed. We add an optional HD foundation without breaking
existing wallets.

- New types: `Mnemonic` (BIP39, 12/24 words), `MasterKey` (BIP32 root from
  seed), and `HdAccount` (mnemonic + derivation metadata).
- `WalletData` gains `hd: Option<HdAccount>`. `None` means a legacy non-HD
  wallet, whose behavior is unchanged.
- `Wallet::create` generates a 24-word mnemonic and stores it in `hd`. The
  existing random-key generation for VTR addresses is unchanged; the mnemonic
  is now available as the shared seed for BTC derivation. (VTR addresses are
  not yet derived from the seed — that is a future change; this sub-project
  only uses the seed for BTC.)
- Opt-in migration: `Wallet::enable_hd()` generates a mnemonic and sets `hd` on
  an existing wallet. Legacy WIF imports and random keys are left untouched.
- The mnemonic lives inside the already-encrypted `WalletData` blob (same
  trust model as the WIFs stored there today), so no new encryption path is
  required.
- Mnemonic/seed material is `Zeroize`d after derivation, matching existing
  conventions.

### 2. `vtorrent-btc` crate

A new workspace crate owning all Bitcoin logic. Modules:

- `keys.rs` — BIP32 derivation from the shared seed at `m/84'/0'/0'/0/x`;
  BIP84 native SegWit address derivation; key → address mapping.
- `headers.rs` — Bitcoin header-chain store (height → header, tip tracking,
  difficulty/chainwork validation). Mirrors the `vtorrent-spv` pattern.
- `merkle.rs` — merkle-proof verification for confirming a tx is in a block.
- `utxo.rs` — UTXO set tracking for the wallet's addresses (scan blocks for
  outputs, mark spent).
- `tx.rs` — build/sign/broadcast Bitcoin transactions (P2WPKH inputs, change
  output, fee estimation).
- `p2p.rs` — minimal Bitcoin P2P client: connect, version handshake,
  `headers`/`getheaders` sync, `getdata` for blocks, `inv`/`tx` broadcast.
- `wallet.rs` — `BtcWallet` facade tying keys + headers + utxo + tx together;
  balance, send, receive, sync status.

Dependencies added to the workspace: `bitcoin` (0.32, matching the existing
`secp256k1 0.29`), `bip39`, `bip32` (or `bitcoin::bip32`), `bech32`.

### 3. Data flow

- **Sync**: on startup, `BtcWallet` connects to BTC peers, performs a
  `getheaders` sync from the last known tip, validates chainwork, and stores
  headers. It then requests blocks (via `getdata`) for the wallet's addresses
  to build the UTXO set.
- **Send**: user enters a `bc1q` destination + amount → `BtcWallet` selects
  UTXOs, builds a P2WPKH tx with change, signs with the derived key, and
  broadcasts via `inv`/`tx` to connected peers.
- **Receive**: derive the next unused BIP84 address and display it; incoming
  txs are detected during block scan and added to the UTXO set.

### 4. Integration points

- `vtorrent-rpc`: new endpoints under `/api/v1/btc/*` (status, balance,
  address, send, sync progress), mirroring the VTR wallet endpoints.
- `vtorrent-tauri`: register `get_btc_*` / `send_btc` commands.
- `vtorrent-ui`: a BTC wallet page (balance, receive address, send form, sync
  status).
- `vtorrent-daemon`: spawn the BTC sync task alongside the VTR node.

## Error handling

`thiserror` enum in `vtorrent-btc`. RPC maps to HTTP status codes: 401 for a
locked wallet, 400 for a bad address/amount, 503 for not-yet-synced.

## Testing

- Unit tests for key derivation (BIP84 test vectors), merkle proofs, tx
  building/signing (known vectors), and header-chain validation.
- Integration tests for the RPC endpoints.
