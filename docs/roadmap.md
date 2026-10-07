# vTorrent-NG Roadmap — Design Index & Sequencing

Status: living doc. Indexes the design docs in `docs/` by area, consensus
impact, dependencies, and recommended ship order. Update as designs land.

## 1. How to read this

- **Consensus?** — `yes` means the change alters consensus rules and needs
  `docs/network-upgrade-design.md` activation (or a fresh genesis pre-mainnet).
  `no` means it ships independently.
- **Depends on** — designs that should land first.
- **Tier** — recommended sequencing (0 = post-soak consensus batch; 1 = quick
  wins; 2 = next; 3 = later).

## 2. Consensus-change designs (Tier 0 — post-soak batch)

| Design | Area | Depends on | Notes |
|---|---|---|---|
| `op-return-utxo-exclusion-design.md` | consensus | — | exclude unspendable outputs; blocker R1 (producer must match) |
| `cold-staking-p2cs-design.md` + `-implementation-plan.md` | consensus | op-return (shares fresh-genesis) | the headline security feature |
| `governance-design.md` | consensus | network-upgrade | bounded params; no treasury v1 |
| `state-rent-design.md` | consensus | governance | recommend reclaimable-dust only |
| `multi-asset-swaps-design.md` | no (VTR) | — | other-chain adapters; VTR side unchanged |

**Sequencing:** `network-upgrade` (mechanism) → `op-return` + `cold-staking`
(fresh genesis) → `governance` → `state-rent`. Each needs its own soak.

## 3. Operational / infrastructure (Tier 1)

| Design | Depends on | Notes |
|---|---|---|
| `network-upgrade-design.md` | — | BIP-9 activation, min-version, fork policy |
| `node-diagnostics-design.md` | — | **SHIPPED** (`85af58e`): `GET /api/v1/diagnostics` |
| `reindex-rescan-design.md` | diagnostics | operator rebuild of derived state |
| `fast-sync-design.md` | — | snapshot verified against `utxo_root` |
| `light-client-design.md` | — | checkpointed SPV; post-state proofs later |
| `block-explorer-design.md` | — | read-only over the store; no second index |
| `webhooks-integrations-design.md` | — | signed at-least-once event delivery |
| `btc-spv-soak-plan.md` | — | restore + soak BTC SPV; header-memory item |

## 4. Security & custody (Tier 1–2)

| Design | Depends on | Notes |
|---|---|---|
| `watch-only-wallets-design.md` | — | track without keys; pairs with cold staking |
| `hardware-wallet-signing-design.md` | watch-only | Signer trait + PSBT-lite |
| `multisig-wallets-design.md` | hardware-signing | P2SH P2MS; partial signing |
| `wallet-backup-recovery-design.md` | — | seed vs file; verify; OTP hygiene |
| `passphrase-rotation-design.md` | backup-recovery | fresh salt/nonce; key rotation |
| `message-signing-design.md` | — | proof of ownership; domain-separated |
| `hd-discovery-design.md` | backup-recovery | gap-limit scan (restore correctness) |
| `transaction-preview-design.md` | — | dry-run; shared build path |

## 5. Wallet & UX (Tier 1)

| Design | Depends on | Notes |
|---|---|---|
| `earnings-view-design.md` | — | **SHIPPED** (`4498fe5`): `GET /api/v1/earnings/summary` + Earnings page |
| `notifications-design.md` | — | toasts + money-critical alerts |
| `atomic-swap-ux-design.md` | — | guided wizard + deadline tracker |
| `payment-requests-design.md` | — | VTR URI + receive QR |
| `wallet-organization-design.md` | — | **PARTIAL** (`5b86513`): contacts + tx notes shipped; full-history fix pending multi-address wallet |
| `wallet-export-statements-design.md` | wallet-organization, earnings-view | CSV/JSON + statements |
| `staking-efficiency-design.md` | — | stake health + consolidation |
| `staking-pools-design.md` | cold-staking | trustless delegation first |
| `mobile-companion-design.md` | notifications, watch-only | scoped paired tokens |

## 6. Torrent economy (Tier 1–2)

| Design | Depends on | Notes |
|---|---|---|
| `incentive-verification-design.md` | — | reciprocal signed receipts (fixes unverifiable pay) |
| `torrent-reputation-design.md` | incentive-verification | verified signals + Sybil cost |
| `incentive-micropayments-design.md` | incentive-verification | netting/batching/channels |
| `torrent-discovery-design.md` | op-return | on-chain registry + DHT + indexer |
| `torrent-creator-design.md` | discovery | build/seed/publish |
| `torrent-streaming-design.md` | — | sequential priority + range server |
| `seeding-policy-design.md` | — | caps, ratio/seed-time, queue |
| `torrent-peer-encryption-design.md` | — | MSE/PE (defeat DPI) |
| `tracker-server-design.md` | — | self-hosted BEP-3/15 tracker |

## 7. DEX & privacy (Tier 2)

| Design | Depends on | Notes |
|---|---|---|
| `dex-orderbook-design.md` | — | price-time priority, partial fills |
| `privacy-design.md` | — | Dandelion++, strict onion-only, PEX/torrent hardening |
| `name-service-design.md` | op-return | `.vtr` names via OP_RETURN registry |
| `fee-market-design.md` | — | target-based estimation, CPFP |

## 8. Dependency highlights

- **Fresh genesis cluster:** `op-return`, `cold-staking`, `governance`,
  `state-rent` all touch the commitment/UTXO set → batch them into one
  activation to avoid repeated fresh-genesis events.
- **Cold-staking unlocks:** watch-only, hardware-signing, multisig, pools.
- **Verification unlocks:** reputation, micropayments (both consume verified
  receipts).
- **Wallet-organization fixes a bug** (`get_transactions` change-address-only)
  that also blocks `wallet-export-statements`.
- **Network-upgrade is the gate** for every consensus change.

## 9. Suggested first three (after sign-off)

1. **Wallet-organization** (fixes the history bug; unblocks export).
2. **Earnings-view** (product value, no consensus).
3. **Network-upgrade + op-return + cold-staking** (the consensus batch).

## 10. Existing (pre-session) design docs

`block-body-pruning-design.md`, `utxo-commitment-scratch-design.md`,
`memory-observability-design.md`, `parallel-fetch-design.md`,
`atomic-swap-protocol.md`, `consensus-parameters.md`, `explorer-faucet-policy.md`,
`dns-seeds.md`, `backup-policy.md`, `oncall-runbook.md`, `rpc-api.md`,
`mainnet-readiness.md`, `soak-log.md`, `wallet-recovery.md`, the code/security
reviews, and `release-notes-beta.3.md`.
