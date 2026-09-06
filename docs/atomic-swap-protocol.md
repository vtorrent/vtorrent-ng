# vTorrent-NG Atomic Swap Protocol

The maker sells VTR, generates the secret, and claims BTC first. The taker
learns the secret from that BTC claim and uses it to claim VTR.

This implementation is not yet approved for real-value swaps. Automatic
recovery, fee-bumped replacement, and adversarial multi-node testing remain open in
[the security review](security-review-2026-09-05.md).

## Funding and claim sequence

1. The maker creates an order through `POST /api/v1/dex/order`. The node
   generates the preimage and hash lock. Example fields:

   ```json
   {
     "maker_address": "V...",
     "maker_btc_address": "bc1q...",
     "offer_asset": "VTR",
     "offer_amount_satoshis": 100000000,
     "request_asset": "BTC",
     "request_amount_satoshis": 100000,
     "expiry_secs": 172800,
     "passphrase": "..."
   }
   ```

2. The maker funds VTR through `POST /api/v1/dex/match`, supplying
   `order_id`, `taker_address`, `passphrase`, and `otp_code` when required.
   The output locks the order's exact amount, hash, recipient, refund address,
   and absolute expiry. The maker's wallet must authorize the funding.

3. Wait for at least six confirmations on the local VTR chain. Before BTC
   funding, shared policy verifies the exact unspent funding output at vout 0:
   transaction ID, amount, and full locking script. An in-memory funded flag
   or mempool transaction is insufficient.

4. The taker funds BTC through `POST /api/v1/swap/btc-fund`:

   ```json
   { "order_id": "...", "btc_refund_address": "bc1q..." }
   ```

   The refund address must belong to the local BTC wallet. Funding requires
   one confirmed wallet UTXO large enough for amount plus fee. The signing key
   is selected for that UTXO's address, including nonzero address indices.

5. `POST /api/v1/swap/btc-claim` takes `order_id`. Before signing or revealing
   the maker's locally held secret, the shared RPC/desktop handler runs a fresh
   BTC contract scan. It checks the exact funding txid and vout 0, amount,
   P2WSH script (hash, recipient, refund address, expiry), six confirmations,
   and absence of a confirmed spend through the scanned tip. Cached wallet
   UTXOs or a recorded broadcast ID cannot authorize revelation.
   It also checks the secret and terms against the order, rechecks unspent VTR
   funding, and refuses revelation within one hour of BTC refund eligibility
   or after a locally submitted VTR refund. The deadline is rechecked after
   network verification; recent BTC header times also bound the claim window.

6. The taker obtains the preimage from the BTC claim and calls
   `POST /api/v1/swap/vtr-claim` with `order_id`, `preimage`, and
   `taker_wif`. VTR claim/refund admission verifies input scripts and fees
   against the local chain. Transaction submission is not confirmation.

## Timing policy

| Rule | Value |
|---|---|
| Minimum VTR funding confirmations | 6 |
| Minimum BTC funding confirmations before revelation | 6 |
| Gap before maker VTR refund eligibility | 6 hours |
| Minimum BTC window at funding | 1 hour |
| Maximum BTC window at funding | 48 hours |
| BTC refund eligibility | min(now + 48h, VTR expiry - 6h) |
| Default VTR order duration | 48 hours |
| Preimage length | Exactly 32 bytes |

A newly funded default-duration VTR order therefore gives BTC approximately
42 hours, less time elapsed before BTC funding. Orders with fewer than seven
hours remaining cannot fund BTC.

These are wallet policy limits, not changes to consensus. CLTV specifies when
a refund becomes valid; it does not disable the preimage claim branch after
that time. Safety depends on confirmation, monitoring, and timely action.
Bitcoin time-based locktime uses its chain's median-time-past, so advancing a
local debug clock does not make a transaction valid on Bitcoin.

### BTC verification limits

The claim scan uses the existing anchored header verifier and BIP-157/158
filter validation, downloads matching blocks, and checks their transaction
commitments. Filters must agree across at least two distinct peer IPs outside
regtest (one configured peer suffices on regtest). With a configured peer,
only that hostname's resolved addresses are used; a single non-regtest IP
fails closed. Without a configured peer, DNS discovery is mainnet-only.

Each verification scans at most the latest 1,008 blocks, requires a tip timestamp
within two hours of the local clock, and has a 120-second network-verification
timeout. Missing older funding, incomplete scans, changed tips, insufficient
peer agreement, and immature coinbase funding are rejected. Headers should
already be synced; a cold mainnet sync can exceed the verification timeout.

These are SPV observations, not full-node validation or proof against eclipsing
all queried peers. The scan detects confirmed spends, not mempool conflicts,
and cannot prevent a reorg after verification. Continued monitoring, durable
recovery, and independent-node adversarial validation remain release requirements.

## Independent refunds

Select the chain explicitly:

```json
POST /api/v1/swap/refund
{ "order_id": "...", "leg": "btc" }
```

Use `"leg": "vtr"` for the maker's VTR refund. The desktop exposes separate
Refund BTC and Refund VTR buttons. If `leg` is omitted, the server selects a
single eligible unsettled leg; ambiguous selection returns an error.

BTC refund does not wait for VTR expiry or require an unlocked VTR wallet.
A failure on one leg does not prevent requesting the other. Opposing actions
on the same leg are rejected after an action is recorded. VTR refunds also
require the local chain timestamp to satisfy the script; the debug clock alone
cannot override that check.

Responses use `VtrClaimSubmitted`, `BtcClaimSubmitted`,
`VtrRefundSubmitted`, and `BtcRefundSubmitted`. These describe local
submission, not on-chain confirmation or completion of both sides.

## Reservations and restart recovery

Funding serializes the swap check and broadcast attempt. Before broadcasting,
the BTC wallet reserves its input and records the signed funding transaction
and public contract terms. The reservation survives a rescan: the old input
cannot become spendable merely because its original block is scanned again.

An ambiguous broadcast failure retains the reservation. A same-process retry
uses the same signed transaction, and refuses a changed refund address or an
elapsed funding window. Concurrent orders cannot reuse the reserved input.

With BTC persistence enabled, contract terms and signed refunds survive
restart. The BTC refund endpoint can reconstruct its leg without the original
in-memory VTR order and retry the same refund after a lost acknowledgement.
The daemon now uses `<data-dir>/btc_utxos.json`; desktop uses its configured
wallet UTXO file. Both fail on unreadable/corrupt state instead of starting an
empty wallet. Keep the BTC seed, network, and state file together when restoring.

### Encrypted maker and VTR journal

The daemon stores per-wallet/per-network records under `<data-dir>/swaps/`;
desktop uses the wallet path with its extension replaced by `.swaps` (or
`./swaps` if the node was started without a configured wallet path). Back up
that entire directory alongside the wallet and the BTC state file.

The existing Argon2id + ChaCha20-Poly1305 wallet format encrypts each record
with a domain-separated password incorporating the VTR wallet's WIF and the
wallet/network namespace. No user passphrase is retained for journal writes.
Recovering requires the same VTR private key and networks; changing only the
wallet passphrase does not invalidate the journal. Possession of the WIF also
allows decryption of its journal, so protect key backups accordingly.

Order creation saves the maker's secret before returning success. VTR funding,
claims, and refunds retain the exact signed transaction before mempool admission
or relay. Invalid signatures fail before a recovery action is reserved. Writes
use private temporary files, file sync, atomic replacement, and directory sync
on Unix. Corrupt records and attempts to overwrite existing contract terms or
prepared transaction IDs are rejected.

Wallet unlock authenticates all records before restoring order and swap state;
corruption fails the unlock rather than silently dropping recovery data. Desktop
node start/open also restores records, and desktop lock clears the shared hot
signing key. The order view includes locally tracked funded swaps after recovery.
Restoration does not automatically transmit transactions. Retry matching with
the same taker, or retry the appropriate VTR claim/refund endpoint, to revalidate
and submit the identical saved transaction. A transaction already in the local
chain or mempool is recognized. An expired, conflicting, or otherwise invalid
transaction is not blindly rebroadcast.

Automatic BTC settlement monitoring, fee-bumped recovery, release of unused
reservations, and protection against rollback to an older authentic journal remain
open. Locking does not yet wipe every in-memory preimage copy. Existing swaps do
not gain secrets lost before journaling. Do not delete recovery records to retry
an ambiguous broadcast.

### VTR settlement observations

`GET /api/v1/swap/{order_id}/status` recomputes VTR state from the active local
chain and current mempool. `POST /api/v1/swap/reconcile` with `{"order_id":"..."}`
also journals changed observations. Both endpoints require the configured RPC
API key. Neither returns secrets, signs transactions, or broadcasts anything.
The desktop swap panel exposes the same read-only check.

Daemon and desktop refresh VTR observations every 30 seconds while the wallet
is unlocked. Journal writes occur on state/anchor changes, not every increase
in confirmation count. Prepared transactions, mempool submissions, shallow
confirmations, and six-confirmation claims/refunds are separate states. A spend
whose ID is not a locally recorded claim/refund is reported as `spent_elsewhere`,
not automatically classified as a successful local settlement. Pending competing
spends are reported separately. Expiry never marks a swap refunded.

Each response identifies the local tip and the funding/spending block anchors.
If an old anchor is absent from the current active chain, its confirmation is
discarded and `reorg_count` increases. This is a conservative local reorg/resync
warning, not proof that the wider network reorganized. Signed recovery transactions
and their original IDs remain intact for revalidation/retry. Cached observations
loaded after restart are never used as fresh confirmation evidence by the status
endpoint. A six-confirmation status can still change after a later reorg.

The response sets `btc_reconciled: false`: this request does not perform a fresh
BTC scan. Its optional `btc` field is a cached, timestamped snapshot, never proof
of current BTC settlement or completion of the entire swap.

### BTC settlement observations

`POST /api/v1/swap/btc-reconcile` with `{"order_id":"..."}` and the configured
API key requests a fresh BTC scan; the desktop exposes the same action. The
network phase is bounded to 120 seconds, with one settlement scan at a time.
It reuses anchored header validation and compact-filter agreement (two distinct
peer IPs outside regtest, one on regtest), but uses an isolated scan tracker,
not the wallet's persisted UTXO cache. Downloaded blocks must match their header,
transaction Merkle root, and witness commitment. It checks the exact funding
txid, output zero, amount, and contract script, and retains confirmed spends.

The result identifies the network, observation time, scan range, tip, confirmation
anchors, and reorg/resync count. Six confirmations distinguish `claimed` and
`refunded` from their confirming states; unknown spend IDs are `spent_elsewhere`.
Expired contracts can be scanned, but expiry alone is never settlement evidence.
Headers are followed by parent height with ancestor locators so a higher-work
competing branch can invalidate old anchors. Changed wallet/contract metadata
or failed scans cannot replace the previous journaled observation.

This is an explicit, dated SPV snapshot, not automatic BTC monitoring or full-node
transaction validation. Scans cover the latest 1,008 blocks only, with no BTC
mempool query. `funding_not_observed` may mean unconfirmed, outside that window,
or absent from the observed branch; it does not prove a failed broadcast or make
an input safe to reuse. Results depend on the selected peers and remain subject
to eclipse attacks and later reorgs. Observations persist in the encrypted
recovery journal when enabled. No secrets/witnesses are returned; no transaction
is signed, rebroadcast, fee-bumped, deleted, or automatically claimed, and no
input reservation is released. Fee-bumped replacement lineage remains open.

## Implementation

- `vtorrent-wallet-service/src/swap_policy.rs`: VTR contract verification and timing
- `vtorrent-wallet-service/src/lib.rs`: shared transaction builders
- `vtorrent-rpc/src/handlers/swap.rs`: shared RPC and desktop orchestration
- `vtorrent-rpc/src/swap_recovery.rs`: encrypted order/secret and signed VTR journal
- `vtorrent-rpc/src/swap_reconciliation.rs`: live VTR confirmation/spend observations
- `vtorrent-rpc/src/btc_reconciliation.rs`: explicit BTC scan and encrypted observations
- `vtorrent-btc/src/sync/swap_observation.rs`: bounded confirmed funding/spend tracker
- `vtorrent-btc/src/wallet.rs`, `utxo.rs`: BTC reservation and recovery records
- `vtorrent-rpc/src/handlers/swap/tests.rs`: concurrency, failure, and restart regressions
