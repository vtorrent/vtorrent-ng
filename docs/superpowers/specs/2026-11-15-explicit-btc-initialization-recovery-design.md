# Explicit BTC initialization and legacy recovery design

## Intent

The v3 guard rejects unknown BTC wallet state because neither absence nor a
pre-v3 file can establish whether a seed previously spent. This proposal restores
BTC availability without weakening that guard.

## Goals

- Genuinely new seeds can be initialized.
- Pre-v2 and already-converted v2 files receive one reviewed recovery route.
- A failure or interruption cannot silently clear an attestation or open an
  unapproved wallet.
- External initialization can later be separated from the node process.

## Non-goals and anti-goals

- Do not infer seed novelty from scan results, `last_scanned_height`, addresses,
  UTXOs, journal size, or `next_index`; those can be true of used wallets.
- Do not auto-open any pre-v3 file.
- Do not let a missing state file silently become a new wallet.
- Do not bypass the ordinary wallet passphrase/TOTP controls for sends.

## Constraints

- Preserve the original legacy and staged bytes exactly.
- The operator must independently establish that every known seed/state copy is
  accounted for, the BTC wallet was drained, has never held BTC, and has no
  outstanding sends or swap obligations.
- Human review of the recovery path remains a launch gate; this design does not
  authorize recovery.

## Approaches

1. **Current blanket block:** Safest, but no new or legacy BTC wallet can run.
   Rejected for launch because it prevents the intended BTC wallet.
2. **Optional explicit attestation:** Default startup keeps the block; an
   explicit local flag may create a v3 seed-scoped file and only after that
   durable grant can the wallet open. This is the selected approach.
3. **Interactive CLI wizard:** Safer prompts, but desktop production would need
   a second UX path and the interactive flow would be harder to automate and
   audit. Deferred.

## Selected design

`BtcWallet::with_persistence` gains an `allow_missing_state` parameter. Daemon,
Tauri and ordinary test fixture paths pass `false`; only the startup option
`--btc-attest-new-seed` passes true. When the state file does not exist and this
capability is absent, initialization fails closed. When the capability is
present, one atomic v3 save creates the file before any scan, send or swap is
performed.

The option is an assertion that the operator has independently established the
conditions above. The code must not pretend to verify them. In an already
initialized state, the option must not weaken the version guard or rewrite an
existing file.

## Legacy recovery

`btc-state-stage` remains experimental and does not authorize opening legacy
state. Extend it with a deliberately separate, locally explicit promote or
migrate command that validates byte equality, seed-scoped destination, network,
permission and staged-copy provenance before marking a file v3. The promote
command is the only place allowed to convert a pre-v3 copy; it must still keep
the original archive intact and publish the result atomically.

No production use is authorized by this design. A reviewer must examine the
exact command and its tests before it is enabled for real wallets.

## Verification

- Missing state without the attestation fails closed.
- Missing state with the attestation creates exactly one v3 file, never two
  partial states under interruption.
- A v2 or older file is rejected even when new-seed attestation is present.
- A corrupt, symlinked, or inaccessible target fails without overwriting.
- A failed write leaves the seed-scoped destination absent and startup blocked.
- Legacy promotion preserves archive bytes, verifies byte equality, survives
  interruption, and leaves retry blocked rather than silently completing.
