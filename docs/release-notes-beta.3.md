# vTorrent 2.0.0-beta.3 Release Notes

*Draft — finalize version number at tag time.*

## Highlights

vTorrent 2.0.0 is a ground-up Rust rewrite of the vTorrent (VTR) client:
Proof-of-Stake consensus, BitTorrent incentive integration, a built-in
atomic-swap DEX, and legacy wallet recovery. Beta 3 is the mainnet-readiness
candidate: three geographically distributed seed nodes are live, the full
protocol surface has been through twelve security/edge-case audit passes,
and the swap + claim paths are now exercised end-to-end on testnet.

## Critical fixes since beta.2

- **Timelock enforcement (consensus)** — `OP_CHECKLOCKTIMEVERIFY` and
  `OP_CHECKSEQUENCEVERIFY` now consult the blockchain's actual height/time
  (BIP-65/BIP-68). Previously a spender could self-declare locktimes and
  spend time-locked outputs (e.g. HTLC refunds) before expiry.
- **Mempool script gating** — transactions with invalid scripts (bad
  signatures, unsatisfied timelocks) are rejected at relay instead of
  poisoning stakers' block templates network-wide.
- **Compact-block relay** — the `blocktxn` index mapping was broken for all
  real block shapes; reconstruction now works and falls back correctly.
- **Faucet-block persistence** — regtest faucet blocks are persisted via the
  event bridge; a restart no longer truncates the chain to genesis.
- **Legacy claims relayable** — snapshot claims (fee-less by design) are
  exempt from the relay fee floor and dust policy.

## Security hardening

- Script-invalid transaction admission gate (`Chain::verify_tx_scripts`)
- Tor control-password hex-encoding; I2P destination charset validation
- `inv`/`getdata`/`headers` size caps; PEX address truncation; WS
  subscription caps; per-IP RPC rate limiting (100 req/min)
- Escalating bans for repeated connection failures; peer user-agent capped
- BTC master seed zeroized on drop; wallet.json written 0600 from creation
- Faucet per-address cooldown; DEX cancel ownership check enforced under
  lock; `btc_fund` requires the VTR leg funded first

## New features

- `GET /api/v1/blockchain/utxo/:txid/:vout` — single-UTXO query for wallets
- Mempool TTL (48 h stale eviction); faucet per-address cooldown
- P2P escalating bans for repeated connection failures
- `--regtest-fast-stake` for soak testing (60 s stake age)
- Per-IP RPC rate limiting (100 req/min sliding window)

## Performance

- `Chain::block_height` O(n) → O(1) (hot path for every tx lookup)
- `getblocktxn` O(height) scan → O(1) hash lookup
- Real mempool byte accounting in `/metrics`
- Incremental sighash, static secp256k1 context, in-place merkle reduction
  (see AGENTS.md benchmark table)

## Infrastructure

- Three seed nodes live (DE/FI/US) with DNS seeds at IONOS
- Bootstrap via hardcoded peers, GitHub `bootstrap/peers.txt` + CDN, DNS seeds
- Prometheus/Grafana/Alertmanager monitoring with ntfy push alerts
- On-call runbook, seed provisioning script, backup policy

## Testing

- 625 workspace tests (was 523 at beta.2), zero-fuzz-marathon clean
- Twelve full audit passes; all findings fixed with regression tests
- Docker testnet: 3-node mesh, restart persistence, self-healing verified

## Known limitations

- SIGHASH types are accepted but all signatures verify against the full tx
  (stricter than SIGHASH_NONE/SINGLE semantics; not exploitable)
- Unconfirmed transaction chains are not supported (inputs must be
  confirmed) — chained spends require each parent to confirm first
- Compact blocks are receive-only (we reconstruct but do not yet send
  `cmpctblock`); blocks propagate via `inv` + `getdata`
- Hidden services are ephemeral (`DiscardPK`); seeds use clearnet addresses

## Upgrade notes

- Wire protocol v3 is a hard compatibility boundary because UTXO commitments
  change the header layout and block hash; all peers must upgrade together
- Seeds must run beta.3 for VTRX magic compatibility (beta.2 fleet already
  restarted)
- Existing pre-v3 block stores are rejected; start v3 with a new data directory

## Checksums

*Populated at tag time by the release pipeline.*

## Addendum — changes since this draft (2026-09-10 → 2026-09-13, unreleased)

- **P2P bulk-sync hardening** — fresh joiners stalled permanently (responder
  announced 501 invs / 2001 headers while receivers cap `getdata` at 500 /
  headers at 2000, scoring misbehaviour into 24h bans). Responders now cap
  exactly, requesters chunk to ≤500; bulk `block`/`tx` traffic draws from a
  dedicated 5000/10s budget; any flood-control-passing message resets ping
  liveness; node-loop disconnect sends are fire-and-forget. Proven with a
  full 0 → 4885 fleet join at byte-identical tip hash. New auth-gated
  `POST /api/v1/peers/unban` for false-positive recourse.
- **Sync-status fix** — followers stuck at 99.9% `syncing: true` after
  reconnect (stale high-water peer height, lost disconnect events on clean
  TCP EOF). Status is now recalculated from live peers + local height.
- **PEX eclipse hardening** — candidate selection draws uniformly from the
  top `count × 4` quality pool instead of deterministic top-N (no wire
  change).
- **Desktop testnet mode** — Mainnet/Testnet picker, persisted + auto-detected
  seed peers, TESTNET badge, separate `~/.vtorrent/testnet` datadir,
  network-mismatch guard; staking dashboard with lazy reward history via new
  `GET /api/v1/staking/rewards`.
- **Soak status** — three-node regtest soak running `vtorrent/node:7db5da3`;
  sign-off pending (see `docs/soak-log.md`). No fleet deploy until sign-off.

## Addendum 2 — changes since the first addendum (2026-09-15 → 2026-09-20)

Two full codebase reviews (`docs/code-review-2026-09-15.md` and
`-second-pass.md`) covered all 19 crates plus the frontend; every finding is
now fixed or documented as accepted (`docs/code-review-2026-09-15-fix-status.md`).

### Consensus

- **Stake-kernel normalization (C1)** — the v1 target saturated at `u32::MAX`
  for any UTXO worth ≥ 42,949.67 VTR, so such an owner could produce every
  block; 72 legacy addresses hold 85.4% of the legacy supply. Replaced with a
  proportional rule (`P = value / total_staked`), so a staker's block share
  equals its stake share. `Chain::total_staked` is tracked incrementally over
  stakeable UTXOs and journaled across reorgs. **This is a consensus-rule
  change and requires a coordinated fleet upgrade.**
- **Legacy-claim signature binding (S1)** — the v1 claim signature signed only
  the address, so anyone observing a claim in the mempool could redirect the
  outputs to themselves and permanently lock out the real owner. The v2 hash
  commits to the outputs; validation and both signing paths use it.
- **Legacy-claim fund safety (L5/L6)** — a claim must now match the snapshot
  balance exactly (a partial claim stranded the remainder), and the mempool
  rejects a competing claim for an address that already has one pending.

### Security

- **RPC auth boundary** — wallet/staking/DEX read endpoints now require the API
  key, and auth runs before the concurrency limiter so unauthenticated
  requests cannot starve the wallet owner.
- **BTC spend authorization (M12)** — `btc/send` and `btc-fund` now require an
  unlocked wallet; the API key alone no longer authorizes moving BTC. The
  refund path is deliberately not gated (documented invariant).
- **Tracker SSRF (M8)** — tracker URLs from untrusted torrents are restricted
  to http/https and rejected when they resolve to loopback, private,
  link-local, CGNAT, benchmarking, reserved, multicast, or IPv6 ULA addresses;
  redirects are disabled.
- **Wallet import overwrite (M10)** — requires an explicit `overwrite: true`.
- **TOTP replay (M15)** — the matched time-step is tracked, so a code cannot be
  reused within its ±1-step validity window.
- **Key material** — the migrate tool no longer dumps WIFs via `--json` or
  prints the derived AES key; decryption and signing intermediates are
  zeroized.
- **Remote panics** — non-ASCII hex no longer panics RPC handlers; DHT bencode
  length overflow, snapshot amount overflow, and torrent piece-length overflow
  are all bounded.

### DoS hardening

- Mempool byte budget (was count-only, ~10 GB reachable), dependency-ordered
  block templates, duplicate-input rejection.
- Per-peer `getdata` egress budget, PEX contribution quota, overlay relay
  quota, torrent session cap, bounded chain scans, linear eviction.
- WebSocket connection cap + idle timeout; rate-limiter prune cost and tracked
  client cap; constant-time API-key compare no longer leaks key length.

### Operations

- **Wallet auto-unlock** — `--wallet-passphrase-file` reads a 0600 file at
  boot, unlocks the wallet, and resumes staking automatically. This removes
  the post-restart stall that caused a ~46-minute outage on 2026-09-18. Fails
  closed on a missing, unreadable, or non-file path.
- **`MALLOC_ARENA_MAX=2`** pinned on the soak fleet: node1's staker dropped
  from 154 MiB / 4 arenas to ~115 MiB / 1 arena, under the 150 MiB budget.
- **`DNS_SEEDS`** now includes `seed3.vtorrent.org`.
- **Production seeds upgraded** from a stale 2026-08-29 binary (217 commits
  behind) to current `main`, fixing a false-positive `PeerCountZero` alert.

### Upgrade notes (in addition to those above)

- The C1 stake-kernel change is consensus-breaking. All nodes must upgrade
  together; a mixed fleet would diverge once `total_staked` exceeds
  42,949.67 VTR.
- The legacy-claim v2 signature is replay-compatible with the current chain
  (which contains no legacy claims), but a fleet on the old binary would
  reject v2 claims.
