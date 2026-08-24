# vTorrent-NG Atomic Swap Protocol

Cross-chain atomic swap between VTR (vTorrent) and BTC using Hash Time-Locked Contracts (HTLCs).

## Overview

The protocol enables trustless exchange of VTR for BTC without intermediaries. Both parties lock funds in HTLCs with the same hash lock; the preimage reveals on claim, enabling the counterparty to claim.

## Protocol Flow

```
Maker (VTR seller)                 Taker (BTC seller)
       |                                   |
  1. Generate preimage + hash
  2. Place DEX order (VTR side)
       |                                   |
  3. Fund BTC HTLC with hash lock -------->|
       |                                   |
  4. Fund VTR HTLC <----------------------|
       |                                   |
  5. Taker claims VTR (reveals preimage) --|
       |                                   |
  6. Maker claims BTC (uses preimage) ---->|
```

## Step-by-Step

### 1. Order Placement (Maker)

The maker (VTR seller) creates a DEX order:

```json
POST /api/v1/dex/order
{
  "side": "sell",
  "give_asset": "VTR",
  "give_amount": 100000000,
  "want_asset": "BTC",
  "want_amount": 5000000,
  "maker_btc_address": "tb1...",
  "taker_address": "V..."
}
```

The server generates a random preimage and computes `hash_lock = SHA256(preimage)`.

### 2. BTC HTLC Funding (Taker)

The taker funds a BTC HTLC:

```json
POST /api/v1/swap/btc-fund
{ "order_id": "..." }
```

This creates a P2WSH HTLC on Bitcoin:
- **Claim:** With preimage that hashes to `hash_lock`
- **Refund:** After `btc_expiry` (default 48 hours, time-based CLTV), back to taker

The funding txid is recorded in the swap state.

### 3. VTR HTLC Funding

The maker funds a VTR HTLC with the same hash lock:

```json
POST /api/v1/blockchain/broadcast
{ "raw_tx": "<signed VTR HTLC funding tx>" }
```

The VTR HTLC has a longer expiry (144 blocks = ~2.4 hours at 60s blocks) to give the taker time to claim.

### 4. Taker Claims VTR

The taker reveals the preimage to claim VTR:

```json
POST /api/v1/swap/vtr-claim
{
  "order_id": "...",
  "preimage": "a1b2c3d4...",
  "taker_wif": "7..."
}
```

The server verifies `SHA256(preimage) == hash_lock`, then broadcasts a claim transaction that reveals the preimage on-chain.

### 5. Maker Claims BTC

The maker (now knowing the preimage from the VTR claim tx) claims BTC:

```json
POST /api/v1/swap/btc-claim
{
  "order_id": "...",
  "maker_btc_wif": "...",
  "refund_address": "tb1..."
}
```

This broadcasts a BTC claim transaction spending the HTLC output with the preimage witness.

### 6. Refund (If Swap Fails)

If the taker never claims VTR, the BTC HTLC expires and the taker can refund:

```json
POST /api/v1/swap/refund
{ "order_id": "...", "taker_wif": "7..." }
```

Similarly, if the maker never claims BTC, the VTR HTLC expires and the maker can refund their VTR.

## HTLC Script Structure

### BTC HTLC (P2WSH)

```
OP_IF
  OP_SIZE 32 OP_EQUALVERIFY
  OP_SHA256 <hash_lock>
  OP_EQUALVERIFY
  <maker_pubkey>
  OP_CHECKSIG
OP_ELSE
  <btc_expiry> OP_CHECKLOCKTIMEVERIFY
  OP_DROP
  <taker_pubkey>
  OP_CHECKSIG
OP_ENDIF
```

### VTR HTLC (P2PKH with script)

Same logic using the script engine's `OP_SHA256`, `OP_CHECKLOCKTIMEVERIFY`, and `OP_CHECKSIG` opcodes.

## Timing

| Parameter | Value | Purpose |
|---|---|---|
| BTC HTLC expiry | 48 hours (time-based CLTV vs MTP) | Taker refund window |
| VTR HTLC expiry | 144 blocks (~2.4 hours at 60s blocks) | Taker claim window |
| Preimage size | 32 bytes | SHA256 preimage |

The BTC window is deliberately much longer than the VTR window: the taker must
claim VTR (revealing the preimage) well before the BTC HTLC expires, giving the
maker time to claim the BTC. Both expiries are absolute — the BTC side as a
Unix timestamp checked against median-time-past, the VTR side via its own
consensus rules.

## Security Properties

1. **Trustless**: Neither party needs to trust the other
2. **Atomic**: Either both claims succeed or both can refund
3. **Time-locked**: Both HTLCs have expiry, preventing indefinite lock
4. **Hash-locked**: Preimage reveals only on claim, enabling cross-chain atomicity

## Failure Modes

- **Taker doesn't fund BTC HTLC**: Maker's VTR is never locked (no action needed)
- **Maker doesn't fund VTR HTLC**: Taker's BTC is locked; taker can refund after BTC expiry
- **Taker doesn't claim VTR**: Maker's VTR is locked until VTR HTLC expires; then maker refunds
- **Maker doesn't claim BTC**: Taker's BTC is locked until BTC HTLC expires; then taker refunds

## Implementation

- `vtorrent-btc/src/htlc.rs`: BTC HTLC construction, funding, claim, refund
- `vtorrent-node/src/atomic_swap.rs`: VTR HTLC and swap state management
- `vtorrent-rpc/src/handlers.rs`: RPC endpoints for swap operations
- `vtorrent-btc/tests/btc_send_flow.rs`: 10 integration tests covering full swap lifecycle
