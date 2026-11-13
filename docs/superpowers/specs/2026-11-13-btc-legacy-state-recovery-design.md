# BTC legacy state recovery design

Status: proposed. This is not an approved production recovery procedure.

An experimental `btc-state-stage` binary now has two subcommands. `stage`
copies exact bytes from an operator-isolated source to a seed-scoped path.
`archive` refuses unless that destination already holds identical bytes, then
copies the legacy file to a new archive, verifies both copies, and only then
removes the active legacy filename. Neither subcommand verifies wallet ownership
or stops writers; their confirmation flags are operator assertions. The tool is
Linux-oriented and unshipped. Do not use it with production state until real
process-interruption tests and independent review are complete. It does not
start a wallet or finish recovery by itself.

## Goal and boundary

Recover a daemon wallet previously stored as `btc_utxos-<network>.json`
without silently associating it with the wrong seed or losing swap recovery
state. Do not change genesis or consensus. Do not operate on a running wallet.
The daemon's unshipped legacy-file guard is a stop, not a migration.

## Evidence and limitations

`UtxoSet` stores no seed fingerprint, wallet identifier, or network field.
It can hold UTXO addresses, reserved outputs, swap refund addresses, signed
funding transactions, and signed refunds. Some older contracts do not record
their input outpoint. Empty state and historical activity absent from the file
cannot prove a seed's identity. A matching derived address is evidence of an
association, not proof that every stored contract belongs to that wallet.

Operators must verify the seed and network against independent wallet records,
including records for any outstanding swaps. A mismatch, missing evidence,
unparseable state, or disagreement about a contract stops recovery. No code
should infer ownership solely from the legacy path, the file being empty, or a
partial address match.

On startup, `BtcWallet::with_persistence` discards the undo journal and tip.
An existing cursor or UTXOs can trigger a header rebuild and rescan. The reset
clears chain UTXOs, but preserves reservations, signed swap transactions,
contracts, and address index. A rescan is not a replacement for swap metadata.
The retained-history guard and its operational limitations still apply.

## Approaches considered

1. Automatically adopt the legacy file for the supplied seed. Rejected: its
   identity is not recorded and a previous operator may have switched seeds.
2. Rebuild from the chain and discard the file. Rejected: chain scanning does
   not reconstruct all reservations, contracts, or signed transactions.
3. Offline, explicitly authorized recovery after independent verification.
   Preferred: retain the legacy bytes and stop on ambiguity. This requires a
   tested tool and a reviewed operator procedure; neither exists yet.

## Proposed offline recovery contract

- Stop every process that can write either wallet file. Confirm exclusive
  access; do not operate on an active soak, daemon, desktop wallet, or backup
  job. Keep verified offline backups of the original state and seed material
  according to the existing backup policy. Do not expose seeds in arguments or
  logs.
- The current daemon and desktop wallet do not share a recovery lock. A new
  tool's lock would not stop an older binary from writing, and the wallet save
  path replaces files by rename. Do not claim that advisory locking or a stable
  open file descriptor alone proves exclusive access. Define how the operator
  isolates the directory from all writers before the tool is usable.
- Require the operator to select the network and independently verify the seed
  and the legacy wallet's outstanding swap records. Present the evidence and
  require an explicit confirmation for this particular source and destination.
  The confirmation is an operator assertion, not cryptographic proof.
- Parse and validate the source without changing it. Reject malformed input,
  non-regular files, inaccessible paths, ambiguous ownership, and existing or
  conflicting destination state. Do not merge files or overwrite a destination.
- Copy the exact source bytes into a private, exclusive staging file in the
  destination directory. Flush the file and verify its contents against the
  source. Publish without replacing an existing destination, then sync the
  directory. Do not expose a partially written destination.
- Keep a verified copy of the original outside the active legacy filename. The
  current guard refuses startup while that filename exists. Removal or movement
  of the active legacy name is an operator-controlled step only after the copy
  is durable and independently verified. Define and test the exact archival
  mechanism before implementing it; never silently delete the only copy.
- On interruption or retry, inspect the source, staging file, destination, and
  archive. Do not assume an existing destination came from this recovery. Stop
  on a conflict; never overwrite or resume automatically on ambiguous state.
- Restart only after a reviewed procedure confirms the destination and backup.
  Verify swap recovery and retained-history coverage before spending or resuming
  swaps. A successful load or a zero balance alone is not proof of recovery.

## Required verification before use

Covered by current focused tests: exact-byte staging, refusal to overwrite a
destination, malformed input, a pre-existing interrupted stage file, wrong
network names, symlink sources, paths outside `--data-dir`, missing confirmations,
archive refusal without a matching staged copy, and archive of a matching copy.
An end-to-end test stages,
archives, starts the daemon, and confirms the loaded wallet's address comes from
the staged index. These are simulated filesystem states, not real process kills.
Archive must run after `stage` and before the wallet starts; a wallet that has
modified its output afterward no longer matches and will not archive. The archive
is a hard link to the source, so both paths must be on the same filesystem; a
cross-filesystem archive fails without changing either file.

Remaining before any production use: real interruption and power-loss testing,
crash-state inspection at each publication and archival step, full workspace
verification, and independent security review.

Use disposable fixtures, never live wallet files. Exercise the actual recovery
tool and daemon, including an old-format file with reserved and spent-reserved
inputs, pending funding transactions, contracts, and signed refunds. Check
exact byte preservation before startup and semantic preservation after load,
rescan/reset, and restart. Include a contract without an input outpoint.

Test both networks, an empty legacy state, mismatched and unknown ownership,
malformed state, source read errors, symlinks, existing destinations, and
concurrent access. Inject failures and process interruption before and after
staging, file sync, publication, directory sync, and archival. Each run must
leave at least one intact original copy and must not overwrite a destination.
Retry must either safely finish a provably identical operation or stop for
operator review. Test the current legacy-file guard with both old and new files
present, and confirm a fresh wallet without a legacy file still starts.

Run the full workspace suite and an independent security review before release.
Any shipping candidate containing the guard or recovery tool needs its own
168-hour soak and manual evidence review.
