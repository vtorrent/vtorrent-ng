# Cold-stake funding attestation design

## Intent

`vtorrent-cli cold-stake spend` signs a VTR P2CS withdrawal on a machine that
has no node. It currently trusts the funding transaction the operator pastes in,
so it cannot tell whether that output is confirmed, still unspent, or real. This
design adds a node-signed attestation that the offline signer verifies before it
will produce a signature.

The funding outpoint is a VTR chain output, not a Bitcoin one. The attestation
therefore comes from the VTR chain view, not the BTC wallet.

## Goals

- Refuse to sign unless a node the operator trusts attests that the funding
  outpoint is confirmed in the active VTR chain, unspent there, and not spent by
  a transaction in its mempool.
- Make the attestation verifiable on the offline machine with no network access.
- Show the operator what was attested (height, confirmations, block hash, issue
  time) before asking for confirmation.
- Fail closed on any mismatch, bad signature, expired attestation, or too few
  confirmations.

## Non-goals

- Proving that the attestor node is honest. Pinning its public key is the trust
  decision.
- Protecting against a reorg after signing. The signer shows the confirmation
  count and enforces a minimum; burying the funding output is an operator
  responsibility.
- Replacing the existing max-fee, amount, and interactive confirmation checks.

## Trust model

The operator runs a node they control, or trusts someone else's node, and pins
that node's attestor public key on the offline machine. A valid signature over
the attested fields then means "this node saw this outpoint confirmed, unspent,
and free of mempool conflicts at this time". A compromised node can lie. That is
why the pin is explicit, and why the signer displays the attested height and
block hash so the operator can compare them with a second source.

## Attestation format

One JSON object plus a detached signature over a canonical byte encoding.

```
{
  "version": 1,
  "network": "vtorrent-regtest",
  "txid": "<64 hex>",
  "vout": 0,
  "value_satoshis": 100000000,
  "script_pubkey": "<hex>",
  "block_hash": "<64 hex>",
  "block_height": 850000,
  "confirmations": 12,
  "tip_hash": "<64 hex>",
  "tip_height": 850011,
  "mempool_spend": false,
  "issued_at": 1794700000,
  "expires_at": 1794703600,
  "attestor_pubkey": "<66 hex compressed secp256k1>",
  "signature": "<DER hex>"
}
```

Canonical bytes are a fixed field order, each length-prefixed, preceded by the
ASCII domain tag `vtorrent-cold-stake-funding-attestation-v1`. String fields are
their raw bytes. Integers are little-endian fixed width. `attestor_pubkey` and
`signature` are excluded.

Signature: secp256k1 ECDSA over SHA256 of the canonical bytes, DER encoded, hex.
The workspace already depends on `secp256k1` with `rand-std`.

`expires_at` is a short window, 1 hour by default. The offline signer refuses an
attestation whose `expires_at` is before its local clock and prints both
timestamps so the operator can see clock skew.

## Node side

New endpoints:

- `POST /api/v1/cold-stake/funding-attestation`
- `GET /api/v1/cold-stake/funding-attestation/pubkey`

Request for the POST:

```
{ "txid": "<64 hex>", "vout": 0 }
```

The handler refuses with a specific reason when:

- the chain view is not ready;
- the transaction is unknown or only in the mempool;
- the output index does not exist;
- the outpoint is spent in the active chain;
- a transaction in the mempool spends the outpoint;
- confirmations are below the configured minimum (default 6);
- the node cannot sign.

It signs only after all checks pass.

Attestor key storage: `<data-dir>/cold-stake-attestor.key`, mode 0600,
secp256k1, generated on first use. The public key is logged at startup and
exposed by the GET endpoint. The key never leaves the node and cannot move
funds.

Authentication: both endpoints sit behind the existing API-key middleware. They
do not require an unlocked wallet, because they read chain state only.

## Offline signer side

New required arguments for `cold-stake spend`:

- `--attestation <file>`: the JSON returned by the node.
- `--attestor-pubkey <hex>`: the pinned public key.

Optional: `--min-confirmations <n>`, default 6.

Verification order:

1. Parse the attestation and reject unknown or missing fields.
2. Recompute the canonical bytes and verify the signature against the pinned
   key. The embedded `attestor_pubkey` must equal the pin.
3. Check `network` matches the node network the operator names.
4. Check `txid`, `vout`, `value_satoshis`, and `script_pubkey` equal the supplied
   funding transaction and the requested outpoint.
5. Check `mempool_spend` is false and `confirmations` is at least the minimum.
6. Check `expires_at` against the local clock.
7. Print the attested height, confirmations, block hash, tip height, and issue
   time, then require the existing `YES` confirmation.

The existing fee checks stay: derived value, maximum fee, the 10 percent cap,
and the displayed amount, fee, and change.

## Failure modes and messages

Each failure names the check that failed and the values involved. A signature
failure says which key was pinned. A mismatch says which field disagreed. A
stale attestation says when it expired and what the local clock read.

## Tests

- Canonical encoding is stable and rejects unknown or missing fields.
- Sign and verify round trip.
- Tampering with each signed field fails verification.
- A different pinned key fails.
- A wrong txid, vout, value, or script fails.
- `mempool_spend: true`, low confirmations, and an expired attestation fail.
- The node handler refuses unknown, mempool-only, spent, and mempool-spent
  outpoints, and signs only the good case.
- The attestor key file is created 0600 and stays stable across restarts.
- The interactive pseudo-terminal CLI test supplies a valid attestation and
  still checks signature validity and the fee guards.

## What is left to the builder

- Exact canonical encoding helper location. `vtorrent-core/src/attestation.rs`
  is the intended home.
- Minimum confirmations default and whether it is configurable per call.
- Whether the desktop sets the attestor key path. If it does not, the endpoint
  returns a clear "not configured" error.

## Open risks

- Clock skew on the offline machine can reject a fresh attestation. The message
  shows both times so the operator can act.
- A node that is behind reports a stale tip. The signed `tip_height` is shown so
  the operator can compare it.
