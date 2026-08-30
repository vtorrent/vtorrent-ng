# vTorrent-NG RPC API Reference

Base URL: `http://127.0.0.1:22525`

All request/response bodies are JSON. Authentication via `X-Api-Key` header.

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

Fund the BTC side of an atomic swap (taker action).

**Request:**
```json
{
  "order_id": "..."
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

### POST /api/v1/swap/btc-claim

Claim BTC using the revealed preimage (maker action).

**Request:**
```json
{
  "order_id": "...",
  "maker_btc_wif": "...",
  "refund_address": "tb1..."
}
```

### POST /api/v1/swap/refund

Refund VTR after HTLC expiry (taker action).

**Request:**
```json
{
  "order_id": "...",
  "taker_wif": "7..."
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

Advance mock time (testnet only).

---

## WebSocket

### GET /ws

Upgrade to WebSocket for real-time events (new blocks, mempool txs, peer events).

---

## Metrics

### GET /metrics

Prometheus-compatible metrics endpoint.
