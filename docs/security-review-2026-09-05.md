# Focused security review — 2026-09-05

Scope: wallet encryption and secret lifetime, RPC access controls, and the
RPC/desktop atomic-swap funding, claim, and refund paths. This is an internal
source review with local regression tests; the external review remains open.

## Fixed in this change

- **TOTP lost on daemon restart.** RPC wallet persistence previously saved only
  an encrypted WIF. The in-memory TOTP secret disappeared on restart, allowing
  unlock with the passphrase alone. New imports encrypt a versioned payload
  containing both WIF and optional TOTP secret. Authentication checks that
  payload on every decrypt. A restart regression reloads the actual saved
  ciphertext into fresh state and checks missing, invalid, and valid codes.
- **Failed TOTP import partially replaced the wallet.** Import wrote the new
  encrypted key and address before validating the supplied TOTP secret. Invalid
  configuration now fails before changing wallet state; a regression verifies
  that the original ciphertext and passphrase still work.
- **Secret buffers survived deallocation.** Decrypted wallet bytes, serialized
  plaintext, RPC signing keys, hot-wallet/staking key copies, RPC passphrases
  and WIF request fields, and desktop wallet lifecycle inputs now use wiping
  buffers. OTP configuration debug output redacts the secret; sensitive RPC
  request and staking command types no longer derive secret-revealing Debug.
  This does not establish complete process-memory erasure: transport buffers,
  third-party crypto internals, OS copies, and other key paths remain outside
  the guarantees of these changes. The desktop wallet still retains its
  guarded passphrase while unlocked for subsequent saves.
- **Regtest debug endpoints bypassed RPC authentication.** Preimage disclosure
  and mock-clock mutation now pass through the configured API-key middleware.
  Tests cover absent, wrong, and correct keys in regtest and non-regtest modes.
- **Funding and spending scripts could disagree on VTR expiry.** Funding used
  `Htlc::new` to recompute wall-clock expiry after calculating the remaining
  order duration. A clock tick or mock-clock offset changed the locking script,
  while claim/refund signatures reconstructed the original order expiry. Both
  RPC and desktop funding now use `Htlc::with_expiry(order.expiry)` and retain
  duration bounds. A deterministic offset-clock regression compares the funded
  script to the one reconstructed for spending.
- **Invalid mock timestamps changed the clock.** Negative/fractional numbers
  previously became `None`, resetting it; oversized values were accepted and
  later truncated. They now return 400 without changing the clock.

## Upgrade note

Older WIF-only ciphertext is still accepted. A TOTP secret omitted by the old
persistence format cannot be recovered from that file: existing users must
re-import with their original TOTP secret to persist 2FA. Downgrading to an old
binary cannot unlock a newly imported structured payload. No live wallets were
rewritten by this review.

## Validation

Workspace tests with all features, strict workspace Clippy across all targets
and features, formatting, Cargo Machete, and whitespace checks passed locally.
The wallet/RPC fixes were committed as `d128ed7`, whose GitHub Actions run is
green. The swap follow-up below is still local and has not been deployed to
the ongoing soak. Workspace tests, strict Clippy, Cargo Machete, formatting,
and frontend lint/build passed for the follow-up.

## Original swap findings and follow-up

The descriptions below record the original findings. Their current mitigation
and remaining release requirements are listed explicitly.

### High: cross-chain expiry ordering is not enforced

The maker creates and retains the preimage (`handlers/dex.rs::place_dex_order`).
The VTR contract refunds to that maker; the BTC contract pays that maker on
revelation. `vtorrent-wallet-service/src/lib.rs::build_btc_htlc_funding` sets
BTC expiry to wall clock plus 48 hours without taking VTR expiry as an input.
VTR orders may expire much earlier. A maker can wait until VTR is refundable,
recover VTR, then claim the still-live BTC contract with the retained secret.
The existing protocol document's claim order and timeout explanation do not
match secret ownership in the implementation.

Required work: settle the role/claim sequence, enforce a BTC claim deadline
before the maker's VTR refund deadline with an explicit safety margin, verify
the actual funded contracts and confirmations before the counterparty funds,
and test adversarial boundary timing on independent nodes. Changing one
constant does not cover those requirements.

Implemented locally: shared policy now checks the exact unspent VTR contract
and six confirmations, requires both amounts to exceed their spend fees, and
chooses BTC refund eligibility at least six hours before VTR expiry. BTC
funding requires at least one hour remaining. Maker BTC claim refuses late
secret revelation. The protocol document now follows maker-secret ownership:
maker claims BTC first, then taker claims VTR. CLTV is refund eligibility,
not a hard deadline on the preimage branch.

Follow-up implemented locally: BTC claim now requires a fresh isolated SPV scan
of the exact P2WSH contract before signing. It verifies txid/vout, amount, full
script, six confirmations, and no confirmed spend through a complete scanned
tip. Persisted wallet UTXOs cannot authorize claims. Non-regtest scans require
filter agreement from distinct peer IPs. Stale/future tips, partial scans,
changed tips, immature coinbase outputs, invalid commitments, and deadlines
crossed during verification fail closed. The handler also checks order/secret
consistency and rechecks confirmed unspent VTR funding before revelation.

Simulated P2P tests cover valid funding, contract-field mismatches, shallow
funding, spent/missing outputs, corrupt responses, and chain-time boundaries.
RPC tests ensure failures never broadcast the secret or update claim status.
The scan now preserves queued filters arriving before a requested full block.

Still open: adversarial validation with independent nodes and the SPV trust
limits (eclipse attacks, mempool conflicts, and reorgs after verification).
Claim scans are bounded to the latest 1,008 blocks and 120 seconds; older or
unverifiable funding is rejected, not assumed safe. These changes do not provide
full per-chain settlement reconciliation; durable maker/VTR recovery is covered below.

### High: swap recovery combines independent legs

`handlers/swap.rs::swap_refund` and the desktop equivalent require VTR expiry
before any refund, try VTR before BTC, and use a single `Refunded` status for
both legs. A failure on the VTR path prevents reaching a refundable BTC leg;
if one refund is submitted before the second fails, retries can collide with
the first transaction. Claims similarly use a shared `Claimed` status.

Required work: persist separate per-chain funding/claim/refund outcomes, make
each leg independently retryable, and reconcile submissions with on-chain
confirmation/spend state. Test one-leg success followed by failure/restart.

Implemented locally: RPC and desktop share one orchestration path, track
separate claim/refund submissions, and expose explicit refund legs. BTC refund
does not require VTR expiry or unlock. Signed BTC refunds and public contract
metadata are saved with the BTC wallet; a fresh RPC state can reconstruct and
retry that refund without the VTR order book. VTR claim/refund submissions
must pass chain-backed script and fee validation.

Follow-up implemented locally: a per-wallet/network encrypted journal now retains
maker secrets, order terms, swap metadata, and exact signed VTR funding/claim/refund
transactions. It uses the existing wallet encryption format with domain-separated
WIF-based key material; user passphrases are not retained for journaling. Records
are synced and atomically replaced before mempool admission or relay. Corruption,
changed contract terms, or replacement of prepared transaction IDs fail closed.
All records authenticate before installation on wallet unlock. Desktop funding
shares the RPC path and desktop wallet open/lock now updates the shared signing
state. Funded local swaps remain visible after restoration.

Restart regressions cover maker-secret recovery, exact funding/claim/refund
retries, and refusal to admit funding when persistence fails. Invalid claim
signatures are rejected before recording a pending claim. Corrupt-file tests
check that failed restoration does not overwrite the file or install its order.

Follow-up implemented locally: live VTR observations now separate prepared,
mempool, shallow-confirmed, and six-confirmation settlement states. Confirmations
are anchored to the active local chain; missing/replaced anchors downgrade the
observation and record a reorg/resync warning without destroying signed recovery
transactions. Confirmed unknown spends and pending competing transactions are
reported explicitly. Authenticated status/reconcile endpoints and a 30-second
unlocked-wallet worker expose and persist these observations. The daemon's
expiry sweep no longer fabricates `Refunded` status without a transaction.

Tests cover confirmation boundaries, removal/reappearance of a claim on the
active chain, persisted stale observations after restart, competing spends,
expiry without refund, and API authentication/no-secret responses.

Follow-up implemented locally: explicit BTC scans now retain funding and spend
confirmation anchors separately from submission IDs and persist dated snapshots
in the encrypted journal. Header sync uses ancestor locators and parent heights
to follow higher-work forks. Failed scans preserve prior evidence; mismatched
contract terms and concurrent scans are rejected. Simulated-peer tests cover
spent outputs, expired contracts, invalid commitments, and a higher-work branch
that removes an observed spend. RPC tests cover confirmation boundaries,
reorg downgrades, encrypted restoration, and secret-free authenticated views.
These are bounded 1,008-block SPV scans, not mempool monitoring or full Bitcoin
validation; missing transactions remain inconclusive and peer eclipse risk remains.

Still open: automatic BTC monitoring, fee-bumped replacement lineage,
and automatic resolution (not merely detection) of conflicting spends. Submission
IDs do not prove confirmations. This journal does not prevent replay of an older
authentic file or wipe all in-memory preimages on lock. Existing swaps created
before journaling do not gain missing recovery metadata retroactively.

### High: BTC funding has no reservation across broadcast

RPC and desktop BTC funding check `VtrFunded` under a read lock, release it,
then build/broadcast and record `BtcFunded`. Concurrent calls can both pass the
guard. The shared builder reads UTXOs without reserving/removing them, so
different swaps can select the same output and local swap state can describe
conflicting funding transactions.

Required work: reserve the swap and selected inputs before broadcast; reconcile
ambiguous broadcast failures and restart state before allowing retries. Cover
concurrent calls and competing orders with deterministic broadcast hooks.

Implemented locally: serialized funding guards, atomic BTC input reservations,
and saved signed funding transactions precede broadcast. Failed persistence
prevents broadcast and restores the in-memory input. Ambiguous broadcast errors
retain the reservation; same-process retries reuse the signed transaction.
Rescanning cannot re-add a reserved outpoint. The daemon uses persistent BTC
state, and desktop no longer falls back to empty state on a load error.
BTC receiving-address indices also persist, so refund recovery can find
nonzero-index signing keys after restart.

Regression tests cover duplicate concurrent requests, competing orders,
ambiguous funding/refund broadcasts, reservation persistence failures, and two
restarts during BTC refund recovery. Script and confirmation-boundary tests
cover the funding policy.

Still open: automatic reconciliation/release of unused reservations after
restart. Signed funding transactions and contract terms remain available in
the BTC wallet file for recovery, but they are not automatically rebroadcast
without re-establishing the counterparty funding facts.
