# vTorrent-NG Atomic Swap Protocol

The maker sells VTR, generates the secret, and claims BTC first. The taker
learns the secret from that BTC claim and uses it to claim VTR.

This implementation is not yet approved for real-value swaps. Automatic
verification of the confirmed BTC contract before secret revelation, durable
maker secret/VTR recovery, and adversarial multi-node testing remain open in
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

5. Before revealing the secret, the maker must independently verify the actual
   BTC contract, amount, funding outpoint, and confirmation depth. This is
   still a manual prerequisite: the current claim handler does not establish
   those BTC-chain facts from the recorded funding transaction ID.
   `POST /api/v1/swap/btc-claim` takes `order_id` and uses the maker's locally
   held preimage and BTC recipient key. It refuses revelation within one hour
   of BTC refund eligibility or after a locally submitted VTR refund.

6. The taker obtains the preimage from the BTC claim and calls
   `POST /api/v1/swap/vtr-claim` with `order_id`, `preimage`, and
   `taker_wif`. VTR claim/refund admission verifies input scripts and fees
   against the local chain. Transaction submission is not confirmation.

## Timing policy

| Rule | Value |
|---|---|
| Minimum VTR funding confirmations | 6 |
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

Automatic reconciliation with confirmed BTC spends, release of unused
reservations, and preservation/recovery of maker secrets and VTR state remain
required. Do not delete reservation records to retry an ambiguous broadcast.

## Implementation

- `vtorrent-wallet-service/src/swap_policy.rs`: VTR contract verification and timing
- `vtorrent-wallet-service/src/lib.rs`: shared transaction builders
- `vtorrent-rpc/src/handlers/swap.rs`: shared RPC and desktop orchestration
- `vtorrent-btc/src/wallet.rs`, `utxo.rs`: BTC reservation and recovery records
- `vtorrent-rpc/src/handlers/swap/tests.rs`: concurrency, failure, and restart regressions
