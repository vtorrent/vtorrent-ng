# Desktop recovery-data runbook

This is the reviewed, manual procedure for restoring wallet-adjacent recovery
data that the desktop app refuses to restore automatically. It applies to the
BTC UTXO state, swap recovery records, and the cold-stake registry.

**Hard rule:** there is no in-app override. Automatic restore remains blocked
because a backup cannot prove that its recovery records are the newest ones. A
newer pending send, swap reservation, or cold-stake registration may exist only
in the live directory. Replacing it with older bytes can lose funds or make a
live contract unmanageable.

Read this whole document before touching a data directory. If any precondition
is not met, stop.

## What is in the data directory

| Path | Contents | Risk if replaced with an older copy |
|---|---|---|
| `wallet.vtr` | Encrypted wallet, 2FA config, address index | Reverts addresses and 2FA; wallet-only restore is the supported path |
| `btc_utxos-<network>-<id>.json` | BTC UTXOs, pending sends, swap contracts, reservations, scan cursor | Can drop a pending send or reservation, or re-expose spent outputs |
| `btc_utxos-<network>-<id>.initialized` | Record that this seed's BTC state was created | Must travel with its state file; never restore alone |
| `wallet.swaps/*.json` | Swap recovery records | Can drop an in-flight swap or its signed refund |
| `wallet.cold_stake.json` | Script hashes authorized for cold-stake funding | Can remove or re-add funding authorization |
| `backups/pre-restore-*.vtr` | Previous wallet file kept by a restore | Wallet only; not a recovery-data backup |

Do not copy `*.lock` files. Locks are host-local and must be recreated by the
running process.

Older installations may still have a network-only `btc_utxos-<network>.json`
file. It is wallet state too; do not rename or migrate it as part of this
procedure, and include it in the snapshot and decisions.

## Preconditions

All of these must hold:

1. The desktop app and every daemon using this data directory are stopped. A
   cooperating writer holds an OS lock; the manual procedure does not prove that
   a non-cooperating process is absent. Check with `lsof +D <data-dir>` and stop
   anything that appears.
2. Two people review the plan: one performs the steps, one checks each command
   and the resulting hashes.
3. The whole current data directory has been copied to separate media and the
   copy's hashes recorded. This is the rollback point.
4. The backup bundle, its passphrase, and any OTP secret are available and
   verified with the app's preview/verify action.
5. No VTR or BTC send is awaiting confirmation, or the operator can account for
   every pending send. Check the app's activity view and, if the node is
   available, `/api/v1/btc/pending-sends`.
6. No swap order is active, or the operator can account for every order.
7. The wallet seed is known. Never re-attest a seed that may have history. Mainnet
   legacy BTC promotion stays disabled; this runbook does not approve it.

## Procedure

### 1. Snapshot and inventory

```bash
DATA=<data-dir>
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
tar -C "$DATA" -czf "/secure/$STAMP-data.tgz" .
sha256sum "$DATA"/wallet.vtr "$DATA"/btc_utxos-* "$DATA"/wallet.cold_stake.json 2>/dev/null
find "$DATA/wallet.swaps" -type f -maxdepth 1 -print -exec sha256sum {} \;
```

Write the file list and hashes into the incident record before changing
anything. If a file is missing, record that explicitly.

### 2. Restore the wallet file

Use the app's wallet-only restore when it is available; it verifies the
passphrase and 2FA and keeps a `pre-restore` copy. If the app cannot be used,
copy `wallet.vtr` in place, preserve mode `0600`, and open it once with the app
to confirm the passphrase and address.

A wallet-only restore is the only automatic path. It intentionally does not
touch any recovery file.

### 3. Decide on BTC state

Compare the live BTC state with the backup copy. Restore the state only when all
of these are true:

- the backup belongs to the same seed and network;
- the live state has no pending send or reservation that the backup lacks;
- the operator has reviewed the backup's pending sends and swap contracts and
  accepts them;
- the live file and its `.initialized` record are preserved in the snapshot.

Copy the state file and its `.initialized` record **together**. If the record
exists but the backup has no state, do not create a state file and do not
re-attest. Stop and escalate.

After copying, start the node and verify `/api/v1/btc/status` and
`/api/v1/btc/pending-sends`. A restored wallet may hide its balance until the
rescan completes; do not treat a hidden balance as a failure by itself.

### 4. Decide on swap records

Restore `wallet.swaps/*.json` only when every live order is accounted for and
the restored records are the ones needed to finish or refund those orders.
Prefer the live record when both exist and their contents differ; merge only
fields the operator can explain. Never replace a live record whose refund
window is closer than the backup's.

After restoring, open the swap view and confirm every expected order appears
with the expected state. If any order is missing or inconsistent, stop and roll
back from the snapshot.

### 5. Decide on the cold-stake registry

The registry is an authorization list, not key material. Prefer the live file.
Replacing it removes funding authorization for scripts generated since the
backup. Adding old hashes re-authorizes older scripts; only do that if the
corresponding spending keys are still under control and the funding is intended.

If a merge is needed, edit a copy, validate it as JSON, show the diff to the
second reviewer, then install it with mode `0600`.

### 6. Start and verify

```bash
# Start the desktop or daemon, then:
curl -s localhost:22525/api/v1/info
curl -s localhost:22525/api/v1/btc/status
```

If the RPC API key is set, add `-H "X-Api-Key: <key>"` or use the desktop app
instead.

Confirm:

- the chain resumes from the expected height, not genesis;
- the wallet opens with the expected address and 2FA state;
- BTC balance and pending sends match the review;
- every expected swap order is present;
- the cold-stake registry lists the intended scripts.

If any check fails, stop the process and restore the snapshot from step 1. Do
not iterate on the live directory.

## Evidence to record

- Date, operator, reviewer, and candidate version.
- Full path of the data directory and the snapshot archive with its SHA-256.
- The before/after hashes of every file that changed.
- The reason each recovery file was restored, merged, or left alone.
- Verification output from step 6.

## Limits

- This procedure cannot prove that a backup is current. It relies on the
  operator accounting for sends, swaps, and registrations.
- It does not protect against another process writing the same directory or a
  copy of the same seed running on another host.
- It does not approve legacy BTC migration, `btc-state-stage promote`, or real
  power-loss recovery. Those remain separately reviewed operations.
