# vTorrent 2.0.0 Release Notes

**Status: release candidate. Not released.** These notes are a draft for the
build that passed the internal test suite and is running the fresh 7-day soak.
Do not publish them as final until the external security review and the soak
sign-off are complete. Update the commit, image, checksums, and review outcome
at tag time.

## Highlights

vTorrent 2.0.0 is the mainnet release of the Rust rewrite: Proof-of-Stake
consensus, BitTorrent incentives, a built-in atomic-swap DEX, and legacy wallet
recovery. This candidate adds on-chain governance, cold staking, a bounded
stake-proof service, and real-bitcoind validation of the BTC SPV and swap
paths.

## Changes since beta.3

### Governance

- Signed votes use recoverable ECDSA over a domain-separated digest bound to
  the genesis hash, proposal, choice, and voting outpoint.
- Votes are unique per original outpoint. Vote locks extend to the longest
  obligation and move with a constrained coinstake re-lock without changing the
  voting identity.
- Activation is deterministic and one-shot. Rejected blocks and reorgs restore
  governance, parameter, UTXO, and supply state atomically.
- Approved policy: 100 VTR proposal deposit burned on pass or failure, voting
  windows of 2,880 to 10,080 blocks, activation delay of 2,016 to 20,160 blocks,
  33% quorum, 66% yes threshold of non-abstain votes, and bounded parameter
  ranges. `MAX_SUPPLY` and fee policy are not votable.
- Governance remains inactive. `deployments::governance_deployment()` returns
  `None`; activation is unscheduled and gated on the external review.

### Consensus and staking

- The P2CS R1 rule requires all positive coinstake value to return to the
  identical P2CS script, preventing the hot staking key from redirecting funds.
  The SPV verifier enforces the same rule.
- OP_RETURN outputs are excluded from the UTXO commitment consistently in the
  chain and the staking producer.
- Cold-stake RPC locktimes are capped at `2100000000`, and payment-request and
  preview amounts are capped at `MAX_SUPPLY`.
- The staking producer consumes the next block's consensus parameters.

### Stake-proof service

- Canonical stake proofs are reconstructed from retained undo history without
  mutating the chain. Received proofs bind to the exact canonical coinstake,
  staking outpoint, Merkle leaf and root, parent modifier, and UTXO path.
- Journal replay and Merkle construction run outside the chain lock. The
  handler rechecks canonicality before caching or serving a proof.
- Cold reconstruction is limited to 100 attempts per source IP and 200
  process-wide in a sliding 60-second window. Cached proofs bypass the limit.
- Unavailable proofs are retried for recent canonical blocks: at most 100
  pending targets, four attempts, 65-second spacing, and a five-minute lifetime.
- Measured on the 59,430-UTXO workload: reconstruction lock hold fell from about
  17 ms to 3.5 ms, and eight-peer RPC p95 fell from about 37.6 ms to 2.6 ms.
  Batch throughput was unchanged.

### Bitcoin SPV and atomic swaps

- The BTC header store has an opt-in retention window
  (`--btc-header-retention`, default 100,000 headers) and keeps the retained set
  contiguous.
- Header pruning is amortized with a 1,024-header slack. A mainnet-scale test
  inserts 870,000 headers and keeps the retained store within 100,000 plus the
  slack.
- Two independent daemons agree on a real regtest SPV tip, discover the same
  funded outputs, persist them across restarts, and follow a reorg that
  re-includes the funding transactions.
- A full HTLC swap settles against real BTC SPV: VTR funding, BTC funding, maker
  BTC claim with a fresh isolated contract scan, and taker VTR claim.
- A conflicting-spend test proves that a reorged-out funding output disappears
  after restart.

### Security and RPC

- Six unrouted RPC endpoints now have routes and regression tests.
- The test-only regtest miner requires the configured API key and the protected
  request limiter. It is absent from production builds.
- Cold-staking E2E amount assertions and the SPV proof route were strengthened.
- The independent genesis verifier (`scripts/verify-genesis.py`) parses the
  snapshot without the Rust decoder and reproduces the transaction ids, Merkle
  root, UTXO root, and genesis hash. CI runs it after the workspace tests.
- Coinstake validation now requires a stakeable script, closing the P2PK
  kernel-denominator bypass.
- Governance votes require stake that predates the proposal block, closing the
  flash-stake window while preserving transferred vote locks.
- Desktop sends now relay through the P2P broadcast channel instead of stopping
  at the local mempool.
- BTC amounts and fees are capped at `MAX_MONEY` and use checked arithmetic.
- DEX eviction only removes open orders; funded and in-progress swaps survive
  gossip pressure.
- `getblocktxn` caps and deduplicates indexes and serializes outside the chain
  lock. The torrent assembler rejects overlapping blocks and caps buffered
  blocks per piece.
- `PaymentUri` decoding no longer panics on multi-byte characters.
- Sensitive BTC, torrent, and peer reads require the API key. P2P egress and PEX
  quotas and ban scores are keyed by normalized IP, so reconnects and IPv6
  rotation do not reset them.

## Testing

- The full release-mode workspace suite passed at `ff6f472`, including the
  node, daemon recovery, RPC, governance, and proof-service tests. The
  changed crate suites and Clippy passed at `669ee36`, which adds the review
  remediation fixes. The BTC birthday flag, desktop commands, desktop health fields, shared BTC
  sync loop, and desktop chain persistence compile and pass Clippy. Rerun the
  full workspace suite before tagging.
- The RPC library suite passes 142 tests, including the test-miner auth
  regression.
- Real-bitcoind tests cover two-daemon SPV agreement, reorg re-inclusion, the
  full HTLC swap, and reorged-out funding.
- The ignored mainnet-scale header retention test inserts 870,000 headers and
  verifies the retained store stays within the bound.
- The encrypted-wallet restart test covers import, unlock, send, SIGTERM,
  `wallet.json` mode 0600, locked restart, wrong-passphrase rejection, restored
  address and balance, and mempool persistence.
- The legacy extraction test builds a synthetic encrypted `mkey`/`ckey` wallet
  and verifies decryption, address derivation, and wrong-passphrase rejection.
- The real-time proof quota test drains the per-IP allowance, observes the
  initial denial, and verifies automatic recovery after the 65-second retry.
- The fresh 7-day soak runs from 2026-10-21T02:47:54Z to 2026-10-28T02:47:54Z
  on `vtorrent/node:batch-e7691fb`. Sign-off is pending.

## Upgrade notes

- Protocol v3 remains a hard compatibility boundary. All peers must upgrade
  together.
- The fleet upgrade replayed 51,713 persisted blocks to rebuild UTXO
  commitments. Expect roughly 14 minutes of startup time at that chain height
  before RPC and staking resume.
- Governance is inactive in this build. BTC SPV remains disabled on the soak
  fleet, so the soak does not exercise the swap paths.
- Existing chain data is preserved. Do not delete `chain.db` during the
  upgrade.
- The core soak fleet runs `batch-e7691fb`. The release build includes the BTC
  wallet birthday flag, desktop commands, desktop health-field updates, the
  shared BTC sync loop, desktop chain persistence, and the cold-stake/BTC/
  compact-block/desktop hardening from the adversarial review and must be
  rebuilt from `1ff179b` (`bab2456` plus the Playwright E2E setup, the live
  cold-stake browser test, the Vite dev-origin CORS allowance, the Tauri path
  capability fix, the durable cold-stake registry persistence fix, the desktop
  restart/persistence parity fixes, the wallet-path confinement, import
  overwrite guard, version bump, IPC error surfacing, and staking ownership
  warnings, and the live desktop journey, restart, and BTC discovery evidence).
  The frontier-model review's ten findings and three follow-up concerns are addressed across `61f4be4`, `cb765ce`, `f272e64`, `3975693`, `6f82b63`, `07a6881`, `326ce27`, `39da204`, `afe5974`, and `2287026`; verified 2FA enrollment, the torrent control availability, the reviewed send quote, verified encrypted backup and restore, the guided swap workspace, the staking readiness assistant, the activity and recovery center, the cold-stake delegation wizard, node settings and diagnostics, and torrent controls and reward accounting are in `1ff179b`. The external review should verify them. The fresh soak window started 2026-10-21T02:47:54Z.

## Known limitations

- Governance activation is unscheduled and requires the external review.
- SPV PoS header acceptance remains fail-closed.
- The real BTC SPV multi-node swap test passes on regtest. Separate maker and
  taker VTR nodes remain open.
- BTC SPV header retention is bounded and tested at 870,000 headers. The
  mainnet-scale BTC SPV soak and separate maker and taker VTR node validation
  remain launch gates because swaps are enabled at launch. BTC wallet discovery defaults to
  the retained floor; set `--btc-birthday` to the wallet creation height to
  scan older funds.
- A real-bitcoind reorged-out claim test is not covered. The settlement-state
  downgrade is covered by unit tests.
- Cold proof reconstruction is unavailable beyond the retained undo window.
  The retry queue is in memory and resets on restart.
- The proof admission budget bounds request counts, not CPU milliseconds.
- Desktop chain persistence was added. Desktop restart, peer status, and BTC
  SPV sync/address discovery are covered; the live Tauri cold-stake journey
  (including durable registry persistence) and two-node propagation are
  covered. Mainnet-scale BTC SPV soak remains.
- Desktop BTC SPV on testnet/regtest uses `VTORRENT_BTC_PEER` (for example
  `127.0.0.1:18448`); mainnet uses DNS seeds.
- Desktop wallet paths are confined to the app data directory; legacy import
  refuses to overwrite an existing wallet. The desktop version is beta.3.
- Desktop backups include the encrypted wallet, cold-stake registry, swap
  recovery records, and wallet-scoped BTC state. They do not include `chain.db`,
  `mempool.json`, or `staking.json`; the chain and mempool rebuild, and staking
  must be restarted after restore.
- Desktop staking returns the RPC ownership warning and the Staking page
  displays it when the selected address is not owned by the hot wallet.
- Frontier-model review follow-ups are fixed: the SPV P2CS verifier enforces
  principal preservation (`39da204`), BTC reservation release requires a
  confirmed spend of the reserved input (`f803cf9`/`afe5974`), and the passphrase-retention
  docs are corrected (`2287026`). See `docs/frontier-review-2026-10-23.md`.
- Cold-stake funding uses the raw P2CS script via `fund_cold_stake`; the
  P2SH-hash-as-P2PKH burn address was removed from the API/UI.

## Checksums

*Populated at tag time by the release pipeline. Include the daemon binary, the
desktop assets, and the independent genesis verifier output.*
