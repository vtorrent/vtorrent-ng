# P2P Order Gossip — Design

Date: 2026-08-18
Status: Approved

## Goal

Make the DEX order book networked by propagating order announcements between
nodes. This is the fourth and final sub-project of the cross-chain atomic-swap
DEX roadmap.

## Scope

This sub-project delivers:

- A public-safe, serializable `OrderAnnouncement` type (excludes the secret
  preimage and private funding txid).
- A new P2P message command (`dexorder`) carrying a serialized announcement.
- Flooding gossip with a bounded seen-set for deduplication.
- Broadcast of new orders on placement, and re-broadcast of received orders.

## Decisions

| Topic | Decision |
|---|---|
| Gossip payload | Public `OrderAnnouncement` type (no preimage) |
| Gossip mechanism | New P2P message + flooding |
| Dedup & expiry | Bounded seen-set of order_ids |

## Architecture

### 1. `OrderAnnouncement` type (`vtorrent-node/src/atomic_swap.rs`)

A serializable, public-safe type:

- Fields: `order_id`, `maker_address`, `vtr_amount`, `target_asset`,
  `target_amount`, `hash_lock`, `expiry`.
- Excludes `preimage` and `funding_txid` (secret/private).
- `Serialize`/`Deserialize` via serde; `from_order(&SwapOrder)` and
  `to_order()` conversions.

### 2. P2P message (`vtorrent-p2p`)

- New command `"dexorder"` carrying a bincode-serialized `OrderAnnouncement`.
- The node's `handle_message` gains a `"dexorder"` arm: deserialize, add to the
  local order book, and re-broadcast to peers (flooding).

### 3. Dedup + broadcast

- A bounded `seen_orders: HashSet<[u8; 32]>` on the node; skip re-broadcast of
  already-seen order_ids.
- On `place_dex_order`, the node broadcasts the new announcement to peers.

### 4. Integration

- The node needs access to the shared order book (currently only in
  `AppState`). Wire a shared `Arc<RwLock<SwapOrderBook>>` into the node so it
  can add received orders and broadcast new ones. The daemon constructs the
  order book once and shares it between the node and `AppState`.

## Error handling

Malformed `dexorder` payloads are dropped and score the peer via the ban
manager (reusing the existing misbehaviour path).

## Testing

- Unit tests for `OrderAnnouncement` round-trip and preimage exclusion.
- Unit tests for the `dexorder` message encode/decode.
- Unit tests for the node's gossip handler (add + re-broadcast + dedup).
