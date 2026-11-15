# BTC send review findings

These findings require resolution and independent review before mainnet. They
include findings from source inspection and targeted tests.

## Ambiguous broadcast

**2026-11-14 remediation in progress:** Confirmed local sends now carry
their pending-send metadata in new undo entries, and journal clearing or
pruning preserves those holds. Corrupt undo data fails wallet loading. Existing
undo entries written before this change have no such metadata, so they cannot
reconstruct an already-retired local send. A production-safe legacy recovery
policy, broader reorg and rescan coverage, and independent review remain open.

The ignored `local_btc_send_reorg_after_restart_keeps_input_held` integration
test passed against an isolated Bitcoin Core 26 regtest node with compact
filters enabled. It confirms a local send, restarts the daemon, replaces the
confirming block with blocks that exclude the send, and restarts again. The
test checks that the input is unavailable and the signed transaction remains
recoverable. It does not cover old wallet records, competing spends, or deep
reorgs.

On Unix, `UtxoSet::save` now syncs the temporary file, renames it, and syncs
the parent directory. A directory-sync failure after rename is reported as an
uncertain save. Send, address index, swap refund, reservation release, and
restore callers retain the visible committed state on such a failure. Tests
inject the failure and reopen the renamed record. They do not simulate power
loss. Directory sync on non-Unix platforms, failure injection for the remaining
background save paths, and actual power-loss testing remain open.

Before the fix, `strace` of
`wallet::tests::orphaned_partial_temp_file_does_not_release_committed_send_after_restart`
showed `fsync` on `utxos.tmp` followed by `rename`, with no parent-directory
`fsync`. After the fix, the same trace shows `fsync` of the parent directory
after the rename. The before trace is `/tmp/opencode/btc-durability-before.log`
and the after trace is `/tmp/opencode/btc-durability-after.log`.

The same review found that `BtcWallet::set_utxo_path` adopted the new path
before saving it. A failed save left the wallet pointed at a path it never
wrote, so every later background save failed silently while the wallet
appeared to persist. The wallet now adopts a path only after the save
succeeds. The regression
`failed_persistence_path_change_keeps_the_working_path` failed before the
change and passes after it.

Legacy wallet files are now handled explicitly. A set written before schema
version 2 did not record which confirmed removal came from a local send, so a
reorg could restore an input whose transaction was still live in a mempool. On
load, `UtxoSet::adopt_legacy_journal_removals` turns each such removal into a
hold with no signed bytes and a zero transaction id. The hold keeps the input
unselectable until a scan observes a spend of that outpoint, which retires it,
so an ordinary external spend is hidden for at most one scan. A file written by
the current code is never converted. Tests cover the conversion, the absence of
conversion for current files, and the upgrade path across a wallet restart.

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

## Claim-replacement recovery

`swap_recovery` now validates the BTC claim chain when loading and persisting,
refuses to replace or shorten it, and adopts the saved raw transaction when it
takes a longer chain. The merge also rejects conflicting in-memory and saved
claim txids. Tests cover a broken parent link, a stale raw claim, a shortened
persist, and the restore adoption path; each guard was verified red without the
fix.

The validator is structural: it checks the parent chain, txids, funding
outpoint, fee monotonicity, and that `btc_claim_raw` matches the last
replacement. It does not re-derive the signed transaction from the HTLC terms.
Independent review should confirm that the structural checks are sufficient and
that no supported flow can persist a divergent chain.
