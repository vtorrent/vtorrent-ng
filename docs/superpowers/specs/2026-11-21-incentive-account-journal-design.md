# Durable incentive-account journal — design

**Status:** implemented; see the journal and daemon payment loop.

## Why

Live torrent sessions are not persisted. Today the payment task snapshots
outstanding `PeerBandwidthAccount`s only on graceful shutdown. A SIGKILL or
power loss loses:

- settled but unpaid `unpaid_satoshis`,
- a broadcast-but-unconfirmed `pending_payment`,
- the process `emitted_total` budget,
- receipt-backed bandwidth that has not reached the next five-minute
  settlement.

The existing `incentive-ledger.json` covers graceful restarts but not a crash.
The goal is a crash window bounded by the receipt/acknowledgement boundary, not
by the shutdown signal.

## Goals

- Receipt-verified debt survives SIGKILL and power loss.
- A payment transaction is never paid twice; a missing transaction stays
  pending until it confirms or an operator resolves it.
- The incentive budget survives a crash.
- Existing graceful-shutdown ledgers remain readable and are migrated.
- Journal write failure is visible and does not silently drop debt.

## Non-goals

- Persisting every downloaded block or every byte counter update.
- Reattaching cumulative counters to a re-added torrent across a peer-epoch
  change.
- Changing the receipt protocol or the six-confirmation finality rule.
- Desktop UI, release signing, or the human-review gate.

## Approach

Chosen: **durable account journal** (option B from the brainstorm). A
competing periodic-debt snapshot (option A) is smaller but cannot recover
receipt-backed accounting between snapshots. A write-ahead obligation log
(option C) is simpler but retains the same unsettled-bytes gap.

### Journal record

`incentive-journal.jsonl`, 0600, append-only newline-delimited JSON:

```json
{"version":1,"info_hash":"<hex>","peer_address":"V...","saved_at":1795...,"account":{...}}
```

`account` is the existing `PeerBandwidthAccount` (it already derives
`Serialize`/`Deserialize`), including counters, baselines, totals, receipt,
receipt epoch, peer epoch, `unpaid_satoshis`, `payment_in_flight`, and
`pending_payment`. A removal is a record with `"deleted":true` for the same
key.

Last record per `(info_hash, peer_address)` wins on replay. A truncated final
line is ignored; the previous complete record remains authoritative. The file
is compacted when it exceeds 1 MiB or 4,096 records by atomically writing the
latest live records to a temporary file and renaming it.

### Writer

One background writer owns the file:

- `upsert` / `remove` send a record plus an optional acknowledgement.
- Receipt and settlement paths **await** the acknowledgement, so the debt is
  durable before it is treated as payable.
- A full channel applies backpressure; callers never silently drop a record.
- `flush` waits for the current queue and `sync_data`; shutdown uses it.
- The writer is `spawn_blocking`/dedicated task; no fsync on the async
  reactor.

### Integration

- Add `IncentiveJournal` trait in `vtorrent-torrent::incentive`:
  `upsert(info_hash, peer, account)`, `remove(info_hash, peer)`.
- `PeerTaskContext` and `SessionManager` receive an optional
  `Arc<dyn IncentiveJournal>`.
- The peer task calls `upsert` after `record_receipt` (before it can be used
  for settlement) and after the final receipt.
- The daemon payment loop calls `upsert` after `settle_agreed`,
  `record_payment_broadcast`, `mark_paid`, `payment_failed`, and
  `payment_missing`; it calls `remove` after a payment is confirmed and the
  account has no remaining debt.
- The desktop passes `None`; it does not run the incentive payer.

### Startup recovery

1. Load the journal and the legacy `incentive-ledger.json` /
   `incentive-live.json`.
2. For each persisted account with no live session:
   - Convert receipt-backed unsettled bytes into debt with
     `agreed_owed()` + `settle_agreed(now, ...)`.
   - Pay `unpaid_satoshis` through the existing budget/cap path.
   - If `pending_payment` exists, check chain/mempool for six confirmations.
     Confirmed → remove; present/shallow → keep; missing → keep pending and
     never re-pay.
   - Remove the record only after the payment is final and the debt is zero.
3. For an account whose `info_hash` is re-added by a torrent, the live session
   starts fresh. The journal record is paid as an orphan debt; counters are
   **not** reattached across a possible peer-epoch change.
4. Legacy ledger entries are imported as journal records with a new id/version;
   the legacy files are renamed `.migrated` after the journal is fsynced.

### Budget

The journal writer persists `emitted_total` in a metadata record whenever a
payment is reserved, refunded, confirmed, or removed. On startup, recovery
payments count against that persisted budget. The cap remains principal-only;
fees are additional.

### Failure handling

- Journal write or fsync failure: log an error, surface a health metric, and
  keep the account in memory. Settlement/payment paths that require durability
  return an error rather than emitting an unrecorded payment.
- Corrupt journal: fail startup with the file path; do not silently discard
  debt. An operator can inspect/remove the file deliberately.
- File size/records are bounded; compaction failure leaves the old journal
  intact.
- Crash during an append: the partial final line is ignored; the previous
  record is used.

## Test plan

- Journal replay: upsert, remove, partial final line, last-record-wins.
- Crash simulation: write an account, drop the writer without shutdown,
  reload, settle, and pay.
- No double payment: pending txid confirmed → remove; missing → keep and do
  not re-emit.
- Receipt-backed recovery: account with a receipt and unsettled bytes becomes
  an unpaid debt at startup.
- Epoch change: stored counters are not reattached; only receipt-backed debt
  is paid.
- Budget persists across reload and caps recovery payments.
- Corrupt journal fails closed; write failure does not emit a payment.
- Legacy ledger migration is idempotent.
- Engine integration: `record_receipt` awaits a journal upsert; a missing
  journal (`None`) preserves the desktop path.

## Open decision

The boundary is **receipt-verified debt**: bytes transferred after the last
peer receipt are not recoverable and are not payable. If that is acceptable,
this design closes the crash window for every debt the protocol can prove.
