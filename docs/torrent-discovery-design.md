# Torrent Discovery & Registry — Design

Status: DRAFT (no consensus change; convention + indexer + P2P)
Scope: `vtorrent-torrent` (DHT, announce), `vtorrent-node`/`vtorrent-rpc`
(registry convention + indexer), UI.
Motivation: the torrent feature has **no discovery** — a user must already have a
magnet link (`add_torrent`, `handlers/torrent.rs:42`). The DHT client exists
(`dht.rs`) but is not wired into the daemon. This design makes torrents
**findable**: an on-chain announce convention (the chain as a censorship-resistant
registry) plus DHT peer discovery and a read-only indexer.

## 1. What exists (and the gaps)

- `MagnetLink` parsing (`metainfo.rs:47`), `add_torrent` RPC.
- `DhtClient` (`dht.rs:248`) with `get_peers`/`announce_peer` — **not used by the
  daemon** (no wiring in `main.rs`/RPC).
- OP_RETURN data-carrier support (`vtorrent-script` `build_op_return`).
- **Gap**: no registry, no search, no DHT discovery in the running node.

## 2. Design

### 2.1 On-chain announce convention (the registry)

- A publisher **announces** a torrent with a transaction carrying an OP_RETURN:
  ```
  "VTRT1" | info_hash(20) | metadata_hash(32) | category(1) | flags(1)
  ```
  - `info_hash` identifies the torrent; `metadata_hash` binds the off-chain
    metainfo (fetched from the torrent/DHT) so content can't be swapped.
  - Cost = a normal tx fee → spam-resistant, and the announce is permanent and
    censorship-resistant.
- **Synergy**: with `docs/op-return-utxo-exclusion-design.md`, announce outputs
  are unspendable and pruned from the UTXO set — the registry costs no state.

### 2.2 Off-chain metadata

- The metainfo (name, size, files) is **not** on-chain (too big). It lives in the
  torrent/DHT; the OP_RETURN carries only `info_hash` + `metadata_hash`. The
  indexer verifies the fetched metadata hashes to the announce.

### 2.3 Indexer (read-only)

- A read-only indexer scans OP_RETURN announces and builds a **searchable
  catalog** (by name, category, publisher, recency). Reuse the explorer's
  read-only service pattern (`docs/block-explorer-design.md`) — no second chain
  index, just an OP_RETURN scan + a catalog.
- Rank by **publisher reputation** (`docs/torrent-reputation-design.md`) and
  recency, not raw presence (anti-spam).

### 2.4 DHT peer discovery

- Wire the existing `DhtClient` into the daemon: for a given `info_hash`, use
  `get_peers` to find peers and `announce_peer` to advertise. DHT is for
  **ephemeral peer discovery**, the chain for the **canonical catalog** — don't
  conflate.
- Route DHT over the onion transport in strict privacy mode
  (`docs/privacy-design.md`).

### 2.5 UI

- A **Discover** page: search the catalog, one-click add (builds the magnet from
  the announce), and a **Publish** flow (announce a local torrent, with the
  privacy warning).

## 3. Adversarial review

- **R1 — announcing is public and permanent.** An OP_RETURN links the publisher's
  address to the torrent forever. Make it **opt-in**, use a **fresh address**,
  and warn. This is a privacy decision, not a default.
- **R2 — spam.** A fee deters casual spam but not a funded attacker. Rank by
  reputation/stake and cap the indexer's per-publisher rate.
- **R3 — illegal/abusive content.** A censorship-resistant registry is a legal
  and abuse surface. This needs an explicit **policy** (like
  `docs/explorer-faucet-policy.md`): what the *official* indexer lists, takedown
  handling, and a clear "the chain is permissionless; the indexer is curated"
  split.
- **R4 — metadata integrity.** The indexer must verify fetched metadata against
  `metadata_hash`, or a publisher swaps content after announcing.
- **R5 — DHT ≠ registry.** DHT is ephemeral peer discovery; the chain is the
  permanent catalog. Don't store the catalog in the DHT (it's not durable) or
  peer discovery on-chain (it's not live).
- **R6 — indexer cost.** Scanning OP_RETURNs over the chain must be bounded and
  cached; don't rescan per query.
- **R7 — no consensus change.** OP_RETURN is already valid; this is a convention
  + indexer + DHT wiring.

## 4. Test plan

- Announce: a well-formed OP_RETURN is recognized; a malformed one is ignored.
- Indexer: catalog builds from announces; metadata hash verified; ranking by
  reputation.
- DHT: `get_peers` finds peers for an info_hash; `announce_peer` advertises.
- Publish flow warns and uses a fresh address.
- Policy: the official indexer's curation/takedown path is defined and testable.
- No consensus change; announce outputs are pruned (with the OP_RETURN design).

## 5. Non-goals

- Not storing metainfo on-chain.
- Not a general web search engine.
- Not part of the current soak window.

## 6. References

- `vtorrent-rpc/src/handlers/torrent.rs:42` `add_torrent`.
- `vtorrent-torrent/src/dht.rs:248` `DhtClient`, `:272,334` get/announce peers.
- `vtorrent-torrent/src/metainfo.rs:47` `MagnetLink`.
- `vtorrent-script` `build_op_return`; `docs/op-return-utxo-exclusion-design.md`.
- `docs/torrent-reputation-design.md`, `docs/block-explorer-design.md`,
  `docs/privacy-design.md`, `docs/explorer-faucet-policy.md`.
