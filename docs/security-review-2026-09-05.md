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
The previously pushed `4a3e86e` also has a green GitHub Actions run; these review
changes are still local and have not been deployed to the ongoing soak.

## Open release blockers

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

### High: swap recovery combines independent legs

`handlers/swap.rs::swap_refund` and the desktop equivalent require VTR expiry
before any refund, try VTR before BTC, and use a single `Refunded` status for
both legs. A failure on the VTR path prevents reaching a refundable BTC leg;
if one refund is submitted before the second fails, retries can collide with
the first transaction. Claims similarly use a shared `Claimed` status.

Required work: persist separate per-chain funding/claim/refund outcomes, make
each leg independently retryable, and reconcile submissions with on-chain
confirmation/spend state. Test one-leg success followed by failure/restart.

### High: BTC funding has no reservation across broadcast

RPC and desktop BTC funding check `VtrFunded` under a read lock, release it,
then build/broadcast and record `BtcFunded`. Concurrent calls can both pass the
guard. The shared builder reads UTXOs without reserving/removing them, so
different swaps can select the same output and local swap state can describe
conflicting funding transactions.

Required work: reserve the swap and selected inputs before broadcast; reconcile
ambiguous broadcast failures and restart state before allowing retries. Cover
concurrent calls and competing orders with deterministic broadcast hooks.
