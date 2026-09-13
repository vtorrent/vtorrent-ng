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
