# Product review: make the core journeys easier to complete

This review covers the current React desktop interface and product documentation.
It is a source-based product review, not a fresh visual usability test. Priorities
are judgments based on user impact and implementation scope. Usage, retention,
and market-demand data were not available.

## Product direction

The strongest promise is one app where legacy holders recover VTR, keep custody,
stake, and exchange VTR for BTC. Torrent incentives provide differentiation, but
users need clear evidence of what earns rewards and when those rewards settle.

The immediate opportunity is to turn the existing capabilities into guided
workflows. The current navigation presents nine peer-level destinations, while
the swap panel still requires order IDs, addresses, and a preimage.

## Fix incomplete interactions first

| Priority | Observation | Next action |
|---|---|---|
| P0 | `SecurityCenterPage.tsx:36` accepts a six-digit code without calling backend verification. `useWallet.tsx:334` marks 2FA enabled during setup. | Stage enrollment, verify a real TOTP in Rust, then persist activation. Cancel must leave the previous state intact. Test an incorrect six-digit code and cancel after setup. |
| P0 | `TorrentPage.tsx:268` renders pause and resume without an action handler. | Connect the control to a real backend operation, or mark the action unavailable until supported. |
| P0 | `DashboardPage.tsx:163` displays a fixed fee notice. The send modal has no transaction review step. | Show a backend-built quote with recipient, amount, fee, total debit, and network before signing. |
| P1 | `Layout.tsx:40` treats a responsive node as online, while `Layout.tsx:147` labels any non-syncing node fully synced. | Distinguish local node running, peer connectivity, sync progress, and readiness to transact. Show the reason when readiness cannot be established. |

Status: verified 2FA enrollment is fixed in `3fefa6b`. The torrent pause/resume
control is marked unavailable in `ea92f22`. The payment review is fixed in
`86aae3a`. Verified encrypted backup and restore is implemented in `3c580c1`.
The guided swap workspace is implemented in `bd35c33`. The staking readiness
assistant is implemented in `aa12b3c`. The activity and recovery center is
implemented in `82d9383`. The cold-stake delegation wizard is implemented in
`3a61254`. Node settings and diagnostics are implemented in `d6c82f4`.

## Prioritized feature roadmap

Effort is relative: small is a bounded interface change, medium spans a workflow
and backend, and large requires a new subsystem or substantial integration.

| Priority | Improvement | User outcome | Effort | First deliverable |
|---|---|---|---|---|
| P0 | Verified encrypted backup and restore | Users can recover after losing their device. | Medium | A backup workflow that checks the archive by restoring it into an isolated location. Inventory wallet, BTC recovery state, swap recovery data, and cold-stake metadata. Explain that a BTC mnemonic alone is not a VTR backup. |
| P1 | Guided swap workspace | Users can finish or recover a swap without manipulating protocol fields. | Large | Select an order to open a persistent timeline with role-specific actions, confirmed chain evidence, deadline warnings, and recovery guidance. Keep raw IDs and preimages in an advanced view. |
| P1 | Unified payment experience | VTR and BTC sends have consistent units, fees, and confirmation states. | Medium | Exact decimal parsing, BTC and satoshi unit selection, network-aware validation, send-max, quote review, and a persistent transaction receipt. |
| P1 | Staking readiness assistant | Users understand why funds are eligible or waiting. | Medium | Show available and eligible balances, maturity, connectivity, wallet lock state, and the next action. Explain that rewards are estimates. |
| P1 | Cold-stake delegation wizard | Users can delegate without copying raw scripts or confusing hot and cold keys. | Medium | Separate owner and staking-node flows. Show roles, export the hot-key package explicitly, and verify the funded contract. Keep spending-key material in the Rust backend except explicit export. |
| P1 | Activity and recovery center | Users can find pending payments, swaps, and failures after restart. | Medium | Searchable history with pending, broadcast, confirmed, reorged, and failed states. Add a details drawer and links from notifications to the affected action. |
| P1 | Node settings and diagnostics | Users can resolve connectivity problems without environment variables. | Medium | Network and BTC-peer settings, restart controls, data-directory visibility, and a redacted diagnostic export with preview. |
| P2 | Useful torrent controls and reward accounting | Users can control bandwidth and understand seeding income. | Medium | Download directory, bandwidth limits, seed-ratio targets, and separate estimated, earned, and settled rewards. |
| P2 | Simpler navigation | New users can find the next action without learning every subsystem. | Small | Group wallet actions, earning actions, and advanced network tools. Onboarding asks whether the user is restoring, receiving, staking, trading, or seeding. |
| P2 | Accessibility and display consistency | More users can operate the app reliably. | Medium | Keyboard and focus handling, labels for icon controls, consistent money formatting, contrast checks, and narrow-window layouts. |

## Delivery order

1. Complete 2FA enrollment, functional controls, payment review, and backup recovery.
2. Build the guided swap workspace and staking readiness assistant.
3. Improve activity history, diagnostics, and torrent reward accounting.

Hardware-wallet support and offline signing are worthwhile later. They require
an explicit compatibility design for VTR and P2CS signing. Adding more trading
pairs, charts, or automation would expand scope before the core journeys are
easy to complete.

## Measure whether the changes help

- Observe whether new users can create or restore a wallet and verify a backup.
- Measure time to a first successful receive, stake, swap, or seeding session.
- Record swap completion and recovery outcomes on controlled test sessions.
- Check whether users can identify why staking is waiting without support.
- Use local diagnostics and opt-in usability sessions. Never collect wallet
  contents, passphrases, keys, or transaction details as product telemetry.

Verified 2FA enrollment is implemented in `3fefa6b`, the torrent pause/resume
control is marked unavailable in `ea92f22`, the reviewed send quote is
implemented in `86aae3a`, and verified encrypted backup and restore is
implemented in `3c580c1`, and the guided swap workspace is implemented in
`bd35c33`, the staking readiness assistant is implemented in `aa12b3c`, and the
activity and recovery center is implemented in `82d9383`, and the cold-stake
delegation wizard is implemented in `3a61254`, and node settings and diagnostics
are implemented in `d6c82f4`. The next substantial new feature is torrent controls
and reward accounting, followed by navigation and accessibility.
