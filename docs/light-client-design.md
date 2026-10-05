# Light Client / SPV Mode — Design

Status: DRAFT (no consensus change; SPV + P2P + wallet)
Scope: `vtorrent-spv`, `vtorrent-p2p` (header sync), `vtorrent-wallet`, RPC/UI.
Motivation: `vtorrent-spv` has VTR PoS headers, stake proofs, tx inclusion, and a
bloom filter — but it is **not a standalone light client**: it is fed headers by
the *local full node*, and its PoS validation has a **documented gap**. A real
light mode (phone, low-power device) needs standalone sync and honest trust
boundaries.

## 1. What exists (and the gaps)

- `SpvChain` (`spv_chain.rs:195`): `add_header`, `add_trusted_header`,
  `add_pos_header`, `verify_tx_inclusion`, `get_locator`.
- **Gap 1 — state-transition proof.** `add_pos_header` (`:387`) states the stake
  proof "authenticates the parent UTXO but not the committed post-state; use
  `add_trusted_header` until state-transition proofs are implemented." So PoS
  validation is **incomplete** and falls back to trust.
- **Gap 2 — not standalone.** Headers are pushed by the local full node
  (`main.rs:446`) or via `add_spv_headers` (`handlers/mod.rs:276`); there is no
  peer-to-peer header sync (`getheaders`/`getblocks`).
- **Gap 3 — bloom filter unused.** `BloomFilter` (`bloom.rs:38`) exists but no
  light wallet consumes it; and BIP37 bloom filters leak the queried set.
- No checkpoints / assumevalid for fast bootstrap.

## 2. Design

### 2.1 Standalone header sync

- Implement **`getheaders`/`headers`** P2P exchange so a light client syncs
  headers from peers (using `get_locator`), not from a local full node.
- Validate the header chain: PoW/PoS target (`hash_meets_target`), difficulty
  retarget, and the stake proof. Where the state-transition proof is missing
  (§2.2), apply a **checkpoint** trust boundary.

### 2.2 The state-transition gap (be honest)

- Two paths:
  1. **Implement post-state proofs** — the stake proof would commit to the
     post-state (e.g. the UTXO commitment), letting a light client verify the
     transition. This is a **consensus/format change** (larger) and the correct
     long-term fix.
  2. **Checkpoint + assumevalid** — ship a recent trusted checkpoint (hash +
     height) and validate forward from it. This is a **trust assumption** (you
     trust the checkpoint), standard for light clients, and must be documented.
- Recommend **(2) now, (1) later**: a checkpointed light client is useful today;
  full validation is a separate consensus effort.

### 2.3 Wallet integration

- A **light wallet** consumes the SPV chain: derive addresses, match txs via
  inclusion proofs (`verify_tx_inclusion`), and show balances.
- **Filter privacy**: prefer **compact block filters (BIP157/158)** over BIP37
  bloom filters — they don't reveal the client's addresses to peers. The bloom
  filter can remain for compatibility but should not be the default.
- Balances from SPV are **unconfirmed-trust** until a checkpoint/confirmation
  policy is met; surface the trust level in the UI.

### 2.4 Bootstrap

- **Checkpoints**: a signed/hardcoded recent checkpoint for fast sync (with a
  clear "trusted" label).
- **Assumevalid**: skip script validation below a checkpoint height (standard),
  documented.

## 3. Adversarial review

- **R1 — don't claim full validation.** The current PoS SPV path trusts headers;
  a light client must state its trust assumptions (checkpoint) plainly, not
  imply it fully validates.
- **R2 — bloom filters leak.** BIP37 reveals the client's addresses to peers;
  default to compact block filters (BIP157/158) for privacy.
- **R3 — header chain is unbounded memory.** The SPV header map grows with chain
  height (same class as the BTC-SPV header issue, `docs/btc-spv-soak-plan.md`);
  bound/persist it.
- **R4 — checkpoint trust.** A checkpoint is a trust anchor; it must be
  distributed securely (signed release) and updatable, and a wrong checkpoint
  forks the client.
- **R5 — reorg handling.** A light client must handle reorgs (roll back headers
  and re-sync); test a reorg at the checkpoint boundary.
- **R6 — DoS.** Header sync from untrusted peers needs bounds (rate, batch size,
  `MAX_SPV_HEADERS_PER_REQUEST` already exists).
- **R7 — no consensus change** for the checkpointed client; post-state proofs
  would be a consensus change.

## 4. Test plan

- Standalone sync: a light client syncs headers from a peer via
  `getheaders`/`headers` and reaches the tip.
- Checkpoint: sync from a checkpoint; a header below it is assumed valid; a
  header above is validated.
- Reorg: the client rolls back and re-syncs across a reorg.
- Tx inclusion: `verify_tx_inclusion` accepts a real proof, rejects a forged one.
- Compact block filters: a light wallet matches its txs without revealing
  addresses.
- Header memory is bounded/persisted.

## 5. Non-goals

- Not full consensus validation (that's the post-state-proof effort).
- Not changing the SPV wire format beyond adding `getheaders`.
- Not part of the current soak window.

## 6. References

- `vtorrent-spv/src/spv_chain.rs:195,227,233,387,523` `SpvChain`, trusted/PoS
  headers, the state-transition gap, tx inclusion.
- `vtorrent-spv/src/bloom.rs:38` `BloomFilter`.
- `vtorrent-daemon/src/main.rs:388,446` SPV fed by the local node.
- `vtorrent-rpc/src/handlers/mod.rs:263,276` SPV status/headers.
- `docs/btc-spv-soak-plan.md` (header-chain memory).
