# BTC send review findings

These findings require resolution and independent review before mainnet. They
are based on source inspection, not an end-to-end reproduction.

## Ambiguous broadcast and restore

The RPC and desktop BTC send handlers call `restore_utxos` on any broadcast
error (`vtorrent-rpc/src/handlers/btc.rs`, `vtorrent-tauri/src/commands/wallet.rs`).
A peer can receive a transaction before the connection fails. An error is not
proof that the transaction was not propagated. Conversely, the broadcast code
treats a timeout without a reject as success; this is not proof of acceptance or
confirmation (`vtorrent-btc/src/p2p.rs`).

The current restore implementation first calls `UtxoSet::add`, which refuses an
outpoint with a pending removal, and cancels pending removals only for outputs
actually added (`vtorrent-btc/src/wallet.rs`, `vtorrent-btc/src/utxo.rs`). Thus
an ordinary failed broadcast can report an error while leaving its inputs
hidden. If a scan has already retired the pending removal, the restore path may
instead re-add an input the chain has observed spent; this interleaving needs a
reproducible test.

Do not treat broadcast failure as proof that inputs are safe to reuse. A fix
needs durable transaction recovery and rebroadcast, explicit uncertain status,
and tests covering peer disconnect after receipt, scan interleaving, restart,
and eventual confirmation or safe operator recovery. Simply keeping inputs
hidden without retaining a recoverable transaction can strand funds.

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
