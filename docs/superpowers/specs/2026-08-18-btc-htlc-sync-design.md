# BTC HTLC Primitives + Live Sync — Design

Date: 2026-08-18
Status: Approved

## Goal

Add the Bitcoin-side HTLC primitives (P2WSH) and the live BTC header-sync +
Bloom-filter UTXO scan to the vTorrent client. This is the second sub-project of
the cross-chain atomic-swap DEX roadmap, and the foundation the two-sided swap
orchestration (next sub-project) builds on.

## Scope

This sub-project delivers:

- A `BtcHtlc` type mirroring the VTR-side `Htlc`, producing a P2WSH locking
  script and funding/claim/refund transactions on the Bitcoin chain.
- A live BTC sync loop: DNS-seed discovery, `getheaders` from genesis, BIP37
  Bloom-filter `merkleblock` fetching, merkle-proof verification, and UTXO-set
  population.

Out of scope (later sub-projects): the two-sided maker/taker orchestration,
claim/refund RPC endpoints, expiry sweep, and P2P order gossip.

## Decisions

| Topic | Decision |
|---|---|
| BTC HTLC script type | P2WSH (native SegWit) |
| Swap scope | Full two-sided flow (across this + next sub-project) |
| Order discovery | Both P2P gossip and direct order_id (gossip deferred) |
| Sequencing | Sequential sub-projects |
| BTC sync depth | Header sync from genesis + BIP37 Bloom filter |

## Architecture

### 1. BTC HTLC primitives (`vtorrent-btc/src/htlc.rs`)

A `BtcHtlc` struct mirroring `vtorrent-node::atomic_swap::Htlc`, but built on
`rust-bitcoin`:

- Fields: `hash_lock: [u8; 32]`, `recipient: String` (BTC address),
  `refund_address: String`, `expiry: u32`, `amount: u64`.
- `build_script()` — the P2WSH witness script with the same
  `OP_IF / OP_SHA256 <hash> OP_EQUALVERIFY … OP_ELSE <expiry>
  OP_CHECKLOCKTIMEVERIFY OP_DROP … OP_ENDIF` structure as the VTR side, using
  BTC address hashes.
- `build_funding_tx()` — a transaction with a P2WSH output locking `amount`.
- `build_claim_tx()` — spends the funding output via the hashlock branch,
  revealing the preimage.
- `build_refund_tx()` — spends via the timelock branch after expiry.

The `vtorrent-spv::BloomFilter` (BIP-37) is reused for the sync side.

### 2. Live header sync (`vtorrent-btc/src/sync.rs`)

- Resolve BTC DNS seeds (`seed.bitcoin.sipa.be`, `dnsseed.bluematt.me`,
  `dnsseed.bitcoin.dashjr.org`).
- Connect via the existing `BtcPeer`, perform the version handshake, send
  `getheaders` from genesis, and store headers in `HeaderChain`.
- Send `filterload` (BIP37) with the wallet's addresses, then `getdata` for
  `merkleblock`s, verify inclusion via `merkle::verify_inclusion`, and populate
  the `UtxoSet`.
- A `BtcSync` task runs the loop and updates `BtcWallet` state.

### 3. Integration

- `BtcWallet` gains a `sync()` method and a `synced` flag.
- The daemon's placeholder BTC task is replaced with the real sync loop.
- RPC `/api/v1/btc/status` reflects real sync progress.

## Error handling

`thiserror` variants in `vtorrent-btc::error` for DNS, P2P, and sync failures.

## Testing

- Unit tests for `BtcHtlc` script/tx building (structure, hash-lock inclusion,
  wrong-preimage rejection, refund-before-expiry rejection).
- Unit tests for the sync loop's header/filter logic (using the existing
  `HeaderChain` and `BloomFilter`).
- Integration tests for the RPC status endpoint reflecting sync state.
