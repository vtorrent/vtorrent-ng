# Internal Security Review — 2026-10-09

An adversarial pass over the custody and consensus surfaces (the external
review's Phase 1 scope, `docs/security-review-brief.md`), done internally while
the external review is commissioned. **Three real bugs found and fixed.**

## Findings

### S1 — CRITICAL: P2CS R1 rule allowed stake theft (fixed `7796958`)
The cold-staking re-lock rule checked only that the P2CS script **appeared**
among the coinstake outputs, not the amount. A hot staking key could pay **1
satoshi** back to the P2CS script and redirect the entire stake (and reward) to
itself — defeating cold staking's whole security claim.
**Fix:** require **all value** in a P2CS coinstake to re-lock to the same script
(empty marker exempt). Extracted to `block::p2cs_redirected_value` + unit test.

### S2 — HIGH: SPV accepted the redirecting coinstake (fixed `26a5d55`)
The SPV verifier checked the P2CS signature but **not** the R1 re-lock rule, so
a light client would accept a stake-redirecting coinstake that full nodes
reject — a **consensus divergence** (light client forks from the network).
**Fix:** the SPV verifier now enforces the same re-lock rule; test extended.

### S3 — HIGH: six endpoints shipped unrouted (fixed `f302a5a`)
Six handlers added this session compiled but were **never routed** (silent
no-op edits on non-matching anchors): `/wallet/preview`, `/wallet/export`,
`/wallet/sign-message`, `/wallet/verify-message`, `/wallet/cold-stake`,
`/swap/deadlines`. The endpoints returned 404 — the features were unreachable.
**Fix:** all registered (auth-protected) + a regression test asserting each
feature endpoint is routed (non-404).

## Reviewed and found sound

- **Wallet encryption** (`encryption.rs`): Argon2id (m=65536, t=3, p=4, OWASP),
  OsRng salt (32B) + nonce (12B), ChaCha20-Poly1305 AEAD, zeroized key. Fresh
  salt+nonce per encrypt. No nonce reuse.
- **RPC auth**: constant-time compare (hashed to fixed length first), auth layer
  is outer (rejects before consuming concurrency permits), default bind
  `127.0.0.1`. Sensitive endpoints (`cold-stake` → private keys, `export` →
  full history) are auth-protected.
- **Debug preimage endpoint**: gated on `regtest`.
- **OP_RETURN eligibility**: correctly retains genesis-distribution markers,
  excludes data carriers; chain + producer use the same predicate.

## Open (design-stage, not exploitable yet)

- **Incentive receipts**: `settlement_id` (anti-replay) is defined but the
  receipt exchange is **not wired** into settlement — so there is no live replay
  surface. The single-use requirement is now documented on `build_receipt`
  (`9733ec1`). Enforce with a `HashSet<u64>` per-peer per-torrent when the
  exchange lands.
- **`cold-stake` locktime** was unbounded — capped at ~2036 (unix 2,100,000,000)
  in `9733ec1`.
- **Payment-request / preview amount** was unbounded — capped at `MAX_SUPPLY`
  in `9733ec1`.

## Verdict

The consensus batch's security hinge (R1) had a **critical** flaw that would
have shipped cold staking with a stake-theft vector. It is fixed and tested.
The external review should still be commissioned (independent eyes on the
crypto), but the internal pass caught the highest-severity issues.

## References

- `docs/cold-staking-p2cs-design.md`, `docs/security-review-brief.md`.
- Fixes: `7796958` (R1), `26a5d55` (SPV), `f302a5a` (routes), `9733ec1` (bounds+doc).
