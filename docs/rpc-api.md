# vTorrent-NG RPC API Reference

Base URL: `http://127.0.0.1:22525`

All request/response bodies are JSON. Authentication via `X-Api-Key` header.

New hot-wallet imports encrypt the WIF and optional TOTP secret together, so
2FA remains enforced after restart. Older WIF-only files remain readable, but
they contain no saved TOTP configuration: re-import with the original TOTP
secret to persist 2FA. Old binaries cannot unlock the new encrypted payload.

---

## Node Info

### GET /api/v1/info

Returns node version, network, block height, peer count.

**Response:**
```json
{
  "version": "0.1.0",
  "network": "mainnet",
  "block_height": 12345,
  "best_block_hash": "...",
  "peer_count": 8,
  "mempool_size": 0,
  "synced": true
}
```

---

## Blockchain

### GET /api/v1/blockchain/height

Returns current chain height.

**Response:** `{ "height": 12345 }`

### GET /api/v1/blockchain/block/:hash

Returns block header and transactions by block hash.

### GET /api/v1/blockchain/height/:height

Returns block by height.

### GET /api/v1/blockchain/tx/:txid

Returns transaction details by txid.

### GET /api/v1/blockchain/utxo/:txid/:vout

Returns a single unspent transaction output. 404 if spent or unknown.

**Response:**
```json
{
  "txid": "<64-hex>",
  "vout": 0,
  "value_satoshis": 100000000,
  "script_pubkey": "<hex>",
  "height": 42,
  "coinbase": false
}
```

### POST /api/v1/blockchain/broadcast

Broadcast a signed transaction.

**Request:**
```json
{ "raw_tx": "<hex-encoded signed tx>" }
```

---

## Mempool

### GET /api/v1/mempool

Returns all transactions currently in the mempool.

### GET /api/v1/fee/estimate

Returns fee estimate in sat/vB.

---

## Wallet

### GET /api/v1/wallet/balance

Returns confirmed and unconfirmed balance.

**Response:**
```json
{
  "confirmed": 100000000,
  "unconfirmed": 0,
  "immature": 0
}
```

### GET /api/v1/wallet/addresses

Returns all wallet addresses.

### GET /api/v1/wallet/utxos

Returns all confirmed UTXOs for the wallet.

### GET /api/v1/wallet/transactions

Returns wallet transaction history.

### POST /api/v1/wallet/import

Import a wallet from WIF or wallet.dat.

**Request:**
```json
{
  "wif": "7...",
  "label": "my-wallet"
}
```

### POST /api/v1/wallet/send

Send VTR to an address.

**Request:**
```json
{
  "to_address": "V...",
  "amount": 100000000,
  "fee": 10000
}
```

**Response:**
```json
{
  "txid": "...",
  "hex": "..."
}
```

### POST /api/v1/wallet/unlock

Unlock the wallet with passphrase.

**Request:**
```json
{
  "passphrase": "my-secret",
  "timeout_seconds": 300
}
```

### POST /api/v1/wallet/lock

Lock the wallet immediately.

---

## Staking

### GET /api/v1/staking/status

Returns current staking status.

**Response:**
```json
{
  "staking": true,
  "address": "V...",
  "staked_amount": 100000000,
  "blocks_staked": 5,
  "rewards_earned": 5000000
}
```

### POST /api/v1/staking/start

Start staking with the wallet.

**Request:**
```json
{
  "address": "V...",
  "passphrase": "my-secret"
}
```

### POST /api/v1/staking/stop

Stop staking.

---

## DEX

### GET /api/v1/dex/orders

Returns all open DEX orders.

### POST /api/v1/dex/order

Place a new DEX order.

**Request:**
```json
{
  "side": "buy",
  "give_asset": "VTR",
  "give_amount": 100000000,
  "want_asset": "BTC",
  "want_amount": 5000000,
  "maker_btc_address": "tb1...",
  "taker_address": "V..."
}
```

### DELETE /api/v1/dex/order/:id

Cancel a DEX order.

### POST /api/v1/dex/match

Match two DEX orders.

---

## Torrents

### GET /api/v1/torrent/sessions

Returns all active torrent sessions.

### POST /api/v1/torrent/add

Add a torrent by magnet link or .torrent file.

**Request:**
```json
{
  "magnet": "magnet:?xt=...",
  "download_path": "/path/to/downloads"
}
```

### DELETE /api/v1/torrent/:id

Remove a torrent session.

---

## Atomic Swap

### POST /api/v1/swap/btc-fund

Fund the BTC side after the exact VTR contract has six local confirmations.
BTC refund eligibility is at least six hours before VTR refund eligibility,
with at least one hour remaining to fund BTC. The refund address must belong
to the local BTC wallet. Retrying a pending attempt reuses its signed transaction.

**Request:**
```json
{
  "order_id": "...",
  "btc_refund_address": "bc1q..."
}
```

### POST /api/v1/swap/vtr-claim

Claim VTR by revealing the preimage (taker action).

**Request:**
```json
{
  "order_id": "...",
  "preimage": "<64 hex chars>",
  "taker_wif": "7..."
}
```

### GET /api/v1/swap/{order_id}/status

Authenticated, read-only VTR settlement view. Reports `order_id`, `vtr` (state,
tip hash/height, funding/spend txids and confirmation anchors, pending competing
spend ID, and `reorg_count`), plus `btc_reconciled: false`.

States distinguish `funding_prepared`, `funding_pending`, `funding_confirming`,
`funded`, `claim_prepared`, `claim_pending`, `claim_confirming`, `claimed`,
and the equivalent refund states. Other states include `not_funded`,
`funding_missing`, `invalid_funding`, `competing_spend_pending`,
`spent_elsewhere`, and `spend_unknown`. Six active-chain confirmations are
required for `funded`, `claimed`, and `refunded`; these are not irreversible
finality guarantees. Expiry alone cannot produce a refund status.

### POST /api/v1/swap/reconcile

Accepts `{"order_id":"..."}` and returns the same response, saving changed
VTR observations to the encrypted journal. Persistence requires an unlocked
wallet when enabled. A 30-second daemon/desktop worker does the same while
unlocked. No transaction is signed, rebroadcast, replaced, or deleted.
`reorg_count` records invalidated local confirmation anchors (including local
resync), while the original signed recovery transactions remain available.
BTC settlement is not freshly reconciled by these endpoints. The optional `btc`
field is the last cached BTC observation, with its original `observed_at` time.

### POST /api/v1/swap/btc-reconcile

Accepts `{"order_id":"..."}`. Requires the configured API key and, when
persistence is enabled, an unlocked wallet. Requests a fresh, isolated BTC
compact-filter scan with a 120-second network timeout; concurrent settlement
scans are rejected. Returns and journals a BTC observation containing `network`,
`observed_at`, `tip_hash`, `tip_height`, `scan_start`, `state`, `funding`, `spend`,
and `reorg_count`. Anchors contain standard Bitcoin display txids/block hashes,
height, and confirmations. Six confirmations are required for `funded`,
`claimed`, and `refunded` (coinbase funding also requires maturity).

Other states: `funding_not_observed`, `invalid_funding`, `funding_immature`,
`funding_confirming`, `claim_confirming`, `refund_confirming`, `spent_elsewhere`.
Only the last 1,008 blocks are scanned, and no BTC mempool is queried. Missing
funding is inconclusive, not proof of a refund or permission to reuse reserved
inputs. Unknown spend IDs are not classified as local claims/refunds. The scan
requires agreement from two compact-filter peer IPs outside regtest (one on
regtest). Stale tips and incomplete/inconsistent scans fail without replacing
the previous snapshot. This is dated SPV evidence, not irreversible finality.
No signing, broadcast, automatic claims, or reservation release occurs.

### POST /api/v1/swap/btc-claim

The maker claims BTC first, revealing the locally held preimage to the taker.
Before signing, the handler performs a fresh BTC SPV contract scan: exact txid,
vout 0, amount, P2WSH script, at least six confirmations, and no confirmed spend
through the scanned tip. Failed or incomplete verification never broadcasts the
preimage. Secret revelation is rejected within one hour of BTC refund eligibility,
including a deadline recheck after the scan and a conservative BTC chain-time check.
Unspent VTR funding is also rechecked before signing.

The scan is bounded to 1,008 recent blocks and 120 seconds, requires a tip timestamp
within two hours of the local clock, and requires two distinct compact-filter peer
IPs outside regtest (one in regtest). A configured hostname resolving to only one
non-regtest IP cannot satisfy verification. See [verification limits](atomic-swap-protocol.md#btc-verification-limits).
This does not establish mempool conflict absence or finality against future reorgs.

With daemon persistence enabled, wallet unlock restores encrypted local order and
swap recovery records. VTR funding/claim/refund retries reuse the saved signed
transaction and revalidate it before submission. The order listing includes
locally tracked funded swaps so their IDs remain discoverable after restart.
Persistence or record-authentication failures stop the action; restoration alone
does not broadcast. See [encrypted recovery](atomic-swap-protocol.md#encrypted-maker-and-vtr-journal)
for backup locations and limitations.

**Request:**
```json
{
  "order_id": "..."
}
```

### POST /api/v1/swap/refund

Refund one chain after that chain's expiry: VTR for the maker, BTC for the
taker. BTC refund is independent of VTR expiry and VTR wallet unlock. If
omitted, `leg` is inferred only when a single unsettled leg is eligible.

**Request:**
```json
{
  "order_id": "...",
  "leg": "btc"
}
```

---

## Legacy Claims

### POST /api/v1/claim/check

Check eligibility for a legacy UTXO claim.

**Request:**
```json
{
  "address": "V...",
  "signature": "..."
}
```

### POST /api/v1/claim/submit

Submit a legacy UTXO claim transaction.

---

## BTC Bridge

### GET /api/v1/btc/status

Returns BTC bridge status (connection, balance, UTXOs).

### GET /api/v1/btc/address

Returns a fresh BTC deposit address.

### POST /api/v1/btc/send

Send BTC to an address.

**Request:**
```json
{
  "to_address": "tb1...",
  "amount_sat": 500000,
  "target_blocks": 6
}
```

---

## SPV

### GET /api/v1/spv/status

Returns SPV header chain sync status.

### POST /api/v1/spv/headers

Submit compact block headers for SPV sync.

---

## Peers

### GET /api/v1/peers

Returns connected peers with score, ban status, and protocol version.

---

## Faucet (Testnet)

### POST /api/v1/faucet

Mint regtest VTR to an address (regtest only). Rate-limited to one claim
per address every 10 seconds.

**Request:**
```json
{
  "address": "V...",
  "amount_satoshis": 10000000000
}
```

**Response:**
```json
{
  "address": "V...",
  "amount_satoshis": 10000000000,
  "txid": "<64-hex>",
  "block_height": 1
}
```

---

## Debug

### POST /api/v1/debug/mocktime

Set the regtest mock clock with `{"timestamp": <integer>}`. The value must be
between 0 and `u32::MAX`; `null` resets the clock. Invalid values return 400
without changing it. Requires the configured API key.

### GET /api/v1/debug/order/:id/preimage

Return an order's secret preimage for regtest testing. Requires the configured
API key. Both debug endpoints return 403 outside regtest after authentication.

---

## WebSocket

### GET /ws

Upgrade to WebSocket for real-time events (new blocks, mempool txs, peer events).

---

## Metrics

### GET /metrics

Prometheus-compatible metrics endpoint.
