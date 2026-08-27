# Block Explorer & Faucet Policy

**Status:** 2026-08-26 — official policy for mainnet launch.

## Block Explorer — Deferred

A read-only block explorer will **not** be available at mainnet launch. This is
an explicit deferral, not an omission:

- The chain's RPC API (`docs/rpc-api.md`, 40+ endpoints) already exposes every
  primitive an explorer would serve: block-by-height/hash lookup, transaction
  lookup, mempool contents, peer info, and SPV status.
- Until a dedicated explorer ships, users can run:
  ```bash
  curl http://127.0.0.1:22525/api/v1/blockchain/block/height/<N>
  curl http://127.0.0.1:22525/api/v1/transaction/<TXID>
  ```
- A minimal explorer (read-only chain index backed by `vtorrent-store`) is
  planned post-launch; it will reuse the existing store tables rather than
  adding a second index.

## Faucet — None on Mainnet

There will be **no mainnet faucet**. Rationale:

- The genesis snapshot preserves every legacy holder's balance; new users
  acquire VTR via atomic-swap DEX trades or peer transfers, both built in.
- A faucet on a PoS chain with a hard-capped supply (20M VTR) is a permanent
  drain and an attack surface (sybil farming of claim-free coins).

**Testnet faucets** remain available in regtest mode for development:
`POST /api/v1/faucet` (`--regtest` flag required). See `docs/rpc-api.md`.
