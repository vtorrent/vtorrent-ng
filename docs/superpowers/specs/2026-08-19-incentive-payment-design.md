# Incentive Payment System — Design

Date: 2026-08-19
Status: Approved

## Goal

Make the VTR torrent incentive system actually pay out: exchange VTR addresses
between peers, record bandwidth, and build real VTR payment transactions on
settlement. This is the final sub-project of the BitTorrent integration roadmap.

## Scope

This sub-project delivers:

- A BEP-10 `ut_vtr` extension to exchange VTR addresses between peers.
- Bandwidth recording wired into the download engine.
- Payment events emitted on settlement, consumed by the daemon to build and
  broadcast VTR transactions.

## Decisions

| Topic | Decision |
|---|---|
| Scope | Full incentive payments |
| Payment tx building | Emit payment events (torrent crate stays decoupled) |

## Architecture

### 1. `ut_vtr` address exchange (BEP-10 extension)

- Extension handshake advertises `ut_vtr` (like `ut_metadata`).
- A `ut_vtr` message carries the peer's VTR address (a short string).
- On connect, both peers exchange their VTR addresses; the engine stores the
  peer's address in `PeerBandwidthAccount.peer_address` (replacing the IP:port
  placeholder).

### 2. Bandwidth recording

- The engine's `run_peer_task` calls `record_download`/`record_upload` as
  blocks are received/sent, keyed by the peer's VTR address (or IP:port if no
  address was exchanged).

### 3. Payment events

- The torrent crate emits a `PaymentDue { peer_address, amount_satoshis }`
  event on settlement (via a channel).
- The daemon/RPC layer receives these events and calls the existing `send_vtr`
  flow to build/broadcast the actual VTR transaction.

### 4. Integration

- The daemon wires the payment-event channel to the wallet's `send_vtr` logic.

## Error handling

`TorrentError` variants for extension negotiation failures. A peer without
`ut_vtr` support is simply not paid (its address is unknown).

## Testing

- Unit tests for the `ut_vtr` message encode/decode.
- Unit tests for bandwidth recording keyed by VTR address.
- Unit tests for the payment-event emission on settlement.
