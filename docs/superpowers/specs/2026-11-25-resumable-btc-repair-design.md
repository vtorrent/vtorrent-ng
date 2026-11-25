# Resuming an interrupted desktop BTC repair

`repair_btc_state` creates mainnet and regtest state for an imported wallet after its owner attests that it has no prior BTC history. Today, an interruption after the first write can leave a valid `.initialized` marker without a state file, or a complete mainnet state without regtest state. The marker also protects a wallet whose state was lost after use. Its presence alone cannot authorize repair.

## Scope and approach

Resume only repairs started by the new implementation. Keep existing partial wallets on the assisted recovery path. Inferring intent from an empty balance or a marker could mistake lost history for a new wallet. Deleting the first file on failure would not handle process termination.

## Recovery contract

1. On every attempt, require the existing passphrase, configured TOTP, and no-prior-history attestation. Bind authorization and both network paths to the same wallet identity for the duration of the operation.
2. Before the first repair write, exclude competing writers and check both networks, including legacy files and initialization markers. If any exist without a matching pending repair, preserve them and return the assisted recovery error.
3. Persist a versioned repair record bound to the wallet identity, networks, and this operation before publishing either network's files. Store no seed or credentials. Stage identifiable artifacts so a retry can verify they belong to this operation. A pending record alone is not proof of ownership of a file found later.
4. Publish without replacing existing files. On retry, keep completed files byte-identical and finish only attributable pending writes. Reject malformed records, identity mismatches, conflicts, and changes to completed files. Block normal use of either network while repair is pending so a missing file cannot mean that an active wallet lost state.
5. Report success only after both networks' state and markers are durable and the pending record is durably retired. A retry after completion must preserve the existing states. Ordinary wallet startup must retain its existing missing-state protection.

## Acceptance tests

Use disposable wallets. Interrupt repair after each durable write, including after either marker, between networks, and after both states exist but before cleanup. Reopen in a fresh process and retry. Check that both networks load and that every previously completed file has the same bytes.

Also check that a competing start or repair cannot write during recovery. Verify that an older partial wallet, a mismatched record, a changed state file, and a failed authorization leave wallet files untouched.

Develop and test separately from `be78829`. Selecting the fix for release requires a new candidate decision.
