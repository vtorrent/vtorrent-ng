# Two-Sided Swap Orchestration — Design

Date: 2026-08-18
Status: Approved

## Goal

Complete the cross-chain atomic-swap DEX by wiring the two-sided settlement
lifecycle: the taker funds a BTC HTLC, the taker claims VTR (revealing the
preimage), the maker claims BTC with that preimage, and both sides can refund
after expiry. This is the third sub-project of the DEX roadmap.

## Scope

This sub-project delivers:

- The deferred Bloom-filter UTXO scan (merkleblock fetch → verify → populate
  the `UtxoSet`), which the orchestration depends on.
- A `SwapState` type tracking a swap's lifecycle across both chains.
- RPC endpoints for BTC funding, VTR claim, BTC claim, and refund.
- A daemon expiry sweep.

Out of scope (next sub-project): P2P order gossip.

## Decisions

| Topic | Decision |
|---|---|
| Orchestration scope | Full claim/refund lifecycle |
| UTXO scan dependency | Include the Bloom-filter UTXO scan first |
| BTC HTLC script type | P2WSH (from prior sub-project) |

## Architecture

### 1. Bloom-filter UTXO scan (`vtorrent-btc/src/sync.rs`)

Complete the deferred UTXO scan:

- After header sync, the peer sends `merkleblock`s for blocks matching the
  loaded Bloom filter.
- On `NetworkMessage::MerkleBlock`, call `extract_matches` to recover matched
  txids, verify the merkle root against the header, and record matched outputs
  into the `UtxoSet`.
- `BtcSync` regains the `utxos: Arc<Mutex<UtxoSet>>` field and populates it.

### 2. Swap state tracking (`vtorrent-node/src/atomic_swap.rs`)

Add a `SwapState` type tracking a swap across both chains:

- Fields: `order_id`, `hash_lock`, `preimage` (held by the maker),
  `vtr_funding_txid`, `btc_funding_txid`, `status`.
- Status transitions: `Funding` → `VtrFunded` → `BtcFunded` → `Claimed` or
  `Refunded`.

### 3. RPC endpoints (`vtorrent-rpc`)

- `POST /api/v1/swap/btc-fund` — taker funds the BTC HTLC using the maker's
  `hash_lock`; records `btc_funding_txid`.
- `POST /api/v1/swap/vtr-claim` — taker claims VTR, revealing the preimage.
- `POST /api/v1/swap/btc-claim` — maker claims BTC using the revealed preimage.
- `POST /api/v1/swap/refund` — refund either side after expiry.
- Expiry sweep in the daemon (reuses the existing DEX maintenance loop).

### 4. Integration

- Wire the endpoints into the router.
- Add Tauri commands and a UI surface for the swap lifecycle.
- Extend the daemon sweep to refund expired swaps.

## Error handling

`thiserror` variants for swap-state and chain-watching failures; RPC maps to
HTTP status codes (401 locked, 400 bad request, 404 unknown order, 409 wrong
state).

## Testing

- Unit tests for the UTXO scan (merkleblock → UtxoSet) and `SwapState`
  transitions.
- Unit tests for each RPC endpoint's happy path and error paths.
- Integration tests for the full maker/taker flow.
