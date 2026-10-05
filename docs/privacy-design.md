# Privacy & Anonymity — Design

Status: DRAFT (no consensus change; P2P + mempool + wallet)
Scope: `vtorrent-p2p` (tx relay, PEX), `vtorrent-node` (mempool), `vtorrent-onion`,
`vtorrent-torrent` (peer IPs).
Motivation: the transport layer already supports Tor/I2P
(`vtorrent-onion`), but **transaction origin** is trivially linkable (no
Dandelion), PEX leaks peer graphs, and torrent peers expose IPs. This closes the
gaps and is honest about what "privacy" means here.

## 1. What exists

- `vtorrent-onion`: Tor SOCKS5 + I2P SAM + clearnet, with `TransportMode`
  (`transport.rs:21`) and a `prefer_onion` option.
- `vtorrent-p2p`: peer manager, PEX (`pex.rs`), ban manager.
- **No tx-origin privacy**: a tx is broadcast directly, so the first peer to
  see it can link it to the broadcaster's IP.

## 2. Design

### 2.1 Transaction-origin privacy (Dandelion++)

- **Stem phase**: relay a new tx to **one** random peer (not a flood), which
  forwards it to one random peer, for a few hops.
- **Fluff phase**: with probability `p` per hop (or after `k` hops), switch to a
  normal flood (gossip) so the tx reaches the network.
- This breaks the direct link between the tx and its originator's IP, which is
  the main on-chain-analysis input.
- Implement in the mempool/relay path; the tx format is unchanged (no consensus).

### 2.2 Onion-only mode

- A **strict** mode: no clearnet connections at all (inbound/outbound via Tor/I2P
  hidden services). Today `prefer_onion` can fall back to clearnet, which
  deanonymizes. Strict mode must **refuse** clearnet rather than fall back.
- Trade-off: fewer peers, higher latency, possible partitioning — document it.

### 2.3 PEX hardening

- PEX (`pex.rs`) shares peer addresses, building a linkable graph. In
  onion-only mode, only share `.onion`/I2P addresses; never leak a peer's
  clearnet IP learned out-of-band.
- Consider disabling PEX in strict mode (rely on DNS seeds / DHT over onion).

### 2.4 Torrent peer IPs

- Torrent peers exchange IPs by design; the torrent client exposes the host IP
  unless tunneled. Route torrent peer connections through the onion transport
  (or a SOCKS proxy) in strict mode, and warn that torrents are IP-exposing
  otherwise.
- DHT announces also leak IP; route over onion or disable in strict mode.

### 2.5 Wallet hygiene

- Address reuse links funds; the wallet already supports labels/HD
  (`wallet.rs`), so surface a "fresh address per receive" default and warn on
  reuse.

## 3. Adversarial review

- **R1 — Dandelion++ is subtle.** Stem/fluff with the wrong probability or hop
  count gives little privacy; follow the Dandelion++ paper's parameters and test
  the origin-linkability empirically, not by assertion.
- **R2 — clearnet fallback is a deanonymization bug.** `prefer_onion` falling
  back to clearnet silently defeats the point. Strict mode must refuse, not fall
  back.
- **R3 — don't overclaim.** This is **IP-privacy**, not "anonymous." On-chain
  analysis, timing, and amount correlation remain. Say so in the UI/docs.
- **R4 — connectivity vs privacy.** Onion-only reduces peers and can partition;
  make it opt-in and surface the trade-off.
- **R5 — torrent is IP-exposing by default.** Don't imply the torrent feature is
  private unless tunneled; warn.
- **R6 — no consensus change.** Relay/mempool/transport only.
- **R7 — interaction with the DEX/swap.** Swap txs are time-sensitive; Dandelion
  latency must not break HTLC deadlines. Allow a fast-path for swap txs (or
  document the added latency).

## 4. Test plan

- Dandelion: a tx traverses stem hops then fluffs; origin is not the first peer
  to flood (empirical linkability test on a simulated network).
- Strict mode: a clearnet address is refused, not dialed.
- PEX in strict mode shares only onion/I2P addresses.
- Torrent over onion: peer connections use the tunnel.
- Swap tx fast-path bypasses Dandelion without breaking deadlines.
- No consensus/chain change.

## 5. Non-goals

- Not a mixnet / Chaumian ecash / ring signatures (much larger).
- Not hiding amounts or the transaction graph itself.
- Not part of the current soak window.

## 6. References

- `vtorrent-onion/src/transport.rs:21` `TransportMode`, `prefer_onion`.
- `vtorrent-p2p/src/pex.rs` (peer exchange), `peer_manager.rs`.
- `vtorrent-node/src/mempool.rs` (tx relay).
- `vtorrent-torrent/src/dht.rs`, `peer_wire.rs` (peer IPs).
