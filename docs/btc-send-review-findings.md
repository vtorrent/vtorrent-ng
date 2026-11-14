# BTC send review findings

These findings require resolution and independent review before mainnet. They
are based on source inspection, not an end-to-end reproduction.

## Ambiguous broadcast

A peer can receive a transaction before the connection fails, so an error is
not proof that the transaction did not propagate. The broadcast code also
treats a timeout without a reject as success, which is not proof of acceptance
or confirmation (`vtorrent-btc/src/p2p.rs`).

The working-tree change stops restoring inputs after a broadcast error. The
signed transaction is persisted with the pending removal, the response reports
uncertain status, and `GET /api/v1/btc/pending-sends` plus
`POST /api/v1/btc/rebroadcast` (and the desktop equivalents) let the operator
retry. A test covers retaining the raw transaction across restart.

Tests now cover the ambiguous paths:

- a mock peer completes the handshake, receives the transaction, then
  disconnects without a reject (`sync::tests`);
- a lost rebroadcast response keeps the signed bytes and inputs
  (`handlers::btc::tests`);
- a successful rebroadcast does not retire the record; only a scan does;
- a scan-observed spend retires the record and its raw transaction
  (`wallet::tests`).

Not covered: a daemon-level run against a real peer that accepts then drops,
and a rebroadcast racing a confirmation that lands between the wallet lookup
and the network write. The second is benign because the record is only retired
by a scan, but it is not directly exercised.

A pending record written by an older build has no raw transaction and cannot be
rebroadcast. The list endpoint reports `has_raw_tx: false` and the UI disables
retry for it; the rebroadcast endpoint returns a specific error. No production
wallet exists yet; if one did, that record would need an operator-led recovery
path before its inputs could be used again.

## Pending-removal capacity

Pending removals previously dropped older records at the journal limit. The
working-tree change retains these records during pruning and rejects sends
that would exceed the 200,000-record limit. Regression tests cover pruning and
record insertion and a send at the capacity boundary. The boundary fixture
repeats a synthetic pending record rather than creating 200,000 real sends.

The working-tree change also retains pending removals when a full reset or
abandoned journal recovery clears chain-derived records. A unit test exercises
the full-reset guard and reopens a persisted wallet after reset. Real rescans
and abandoned-recovery behavior still need coverage before this can be
considered safe.

## Claim-replacement recovery requires further verification

`swap_recovery::restore` checks VTR refund lineage compatibility, but selects
the longer BTC claim-replacement list without an analogous prefix check. It
also fills `btc_claim_raw` only when absent, even when it takes a longer saved
replacement list. Investigate whether two conflicting lineages or a stale raw
claim can arise through supported persistence and restart flows, and test the
resulting fee calculation and retry behavior. Source inspection alone does not
establish that such a state is reachable or exploitable.
