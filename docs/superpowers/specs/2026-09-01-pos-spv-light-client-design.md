# PoS Light-Client Validation — UTXO Commitment + Stake Proofs

**Date:** 2026-09-01
**Status:** Draft — approved via brainstorming (Approach A)
**Author:** vTorrent Dev
**Related:** `docs/consensus-parameters.md`, `docs/mainnet-readiness.md`, `vtorrent-spv/src/spv_chain.rs:20`, `vtorrent-node/src/block.rs:238`, `vtorrent-node/src/consensus.rs:81`

## 1. Overview

Remote vTorrent PoS light clients currently cannot validate stake. `SpvHeader` (`vtorrent-spv/src/spv_chain.rs:20`) and `BlockHeader` (`vtorrent-node/src/block.rs:238`) commit only to `merkle_root`; `SpvChain::add_header:147` rejects PoS headers (`nonce==0`) because it cannot prove stake ownership, age, or that the UTXO remains unspent. Full nodes validate via `chain/chain_reorg.rs:193` + `consensus.rs:118` against the live `utxo_set` (`chain.rs:138`).

This spec adds a UTXO-state commitment to headers and defines self-contained `StakeProof`s so an SPV client verifies `stake_modifier`, amount, age, kernel, signature, and proof continuity without a UTXO set.

**Recommended sequence (implemented in order):**
1. Add UTXO-state commitment to PoS block headers.
2. Define stake proofs containing coinstake transaction, Merkle inclusion, and UTXO membership proof.
3. Verify stake modifier, amount, age, signature, and proof continuity in `vtorrent-spv`.
4. Add adversarial fork and forged-stake tests.
5. Run multi-node reorg and staking soak tests.

## 2. Goals / Non-Goals

**Goals:**
- PoS headers are self-validating for light clients given `prev_header.utxo_root`.
- Reuse existing `MerkleTree`/`MerkleProof` (`vtorrent-spv/src/merkle.rs:27`, `vtorrent-node/src/block.rs:289`) — no new crypto deps.
- Preserve PoS consensus invariants: `1 VTR` min (`consensus.rs:37`), `6h–6d` age (`network.rs:62`), 5% reward (`consensus.rs:28`), kernel target `value/1000`, secp256k1 sighash (`block.rs:88`, `staking.rs:284`).
- Deterministic, benchmarkable (`vtorrent-node/benches/consensus_hotpath.rs`), version-gated hard-fork.

**Non-Goals:**
- Utreexo/accumulator (deferred; root versioned for migration).
- P2PKH-only? Initially P2PKH (`staking.rs:365` `validate_p2pkh`), script types extensible.
- Full UTXO set download for SPV; only `log(N)` proofs.

## 3. Architecture

```
Full node (Chain)                          Light client (SpvChain)
┌──────────────┐                           ┌──────────────┐
│ utxo_set     │──hash leaves──► utxo_root │ headers +    │
│ height_index │  MerkleRoot   │──header──►│  utxo_roots  │
│ stake_mod    │──compute──────► stake_mod │  verify      │
└──────────────┘                           └──────────────┘
         ▲                                        │
         │ StakeProof = (coinstake,               │
         │  tx_merkle_proof->merkle_root,         │
         │  utxo + utxo_proof->prev_utxo_root)    ▼
         └────────────────────────────────── add_pos_header()
```

Headers form a chain of `prev_hash` + `utxo_root` commitments. Each PoS header's proof targets `prev_header.utxo_root`. SPV tracks `utxo_root` per header; continuity is `utxo_proof.root == prev_header.utxo_root` and `header.utxo_root` is the post-apply commitment.

## 4. Data Structures

### 4.1 Header commitment

```rust
// vtorrent-node/src/block.rs:238
pub struct BlockHeader {
    pub version: u32,
    pub prev_block_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub utxo_root: [u8; 32], // NEW — Merkle root over UTXO set AFTER this block
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
    pub stake_modifier: u64,
}
// vtorrent-spv/src/spv_chain.rs:20
pub struct SpvHeader {
    pub version: u32,
    pub prev_hash: [u8; 32],
    pub merkle_root: [u8; 32],
    pub utxo_root: [u8; 32], // NEW
    pub timestamp: u32,
    pub bits: u32,
    pub nonce: u32,
    pub height: u32,
}
```

**Leaf:** `leaf = SHA256d( txid || vout.to_le_bytes() || value.to_le_bytes() || varint(script.len()) || script || height.to_le_bytes() || timestamp.to_le_bytes() )` where `script = Utxo.script_pubkey` (`chain.rs:35`). `height`/`timestamp` included so age is committed; `value` covers amount.

**Tree:** Leaves sorted by `(txid, vout)` ascending, hashed to `[[u8;32]]`, then `compute_merkle_root_from_txids(&mut leaves)` (`block.rs:289`) in-place reduction with last-duplication for odd count, identical to `MerkleTree::build` (`merkle.rs:37`). Empty set => `[0u8;32]` (genesis before distribution is not empty; but canonical).

**Genesis:** `create_genesis_block:96` derives `utxo_root` over 59,375 legacy outputs (sorted). Hash `11f2093333a718616ba1f2173b31487cf4be5e44a861b516685acdb1088cfb21` changes — consensus freeze doc `consensus-parameters.md` updated; new constant `GENESIS_UTXO_ROOT` added.

**Wire:** `bincode` header serialization grows 32 bytes (≈112 vs 80) and `SpvHeader::hash:39`/`BlockHeader::hash:257` double-SHA256 now covers `utxo_root`. P2P protocol version bump `vtorrent-p2p/src/message.rs:102` `2 → 3` signals `utxo_root`. `version >= 2` requires `utxo_root != [0;32]`; `version == 1` legacy headers carry zero root (rejected for PoS validation — treated as trusted-only via `add_trusted_header:156`).

**Computation:** In `chain_reorg.rs:127` `apply_block_journaled` after `for tx in &block.transactions` UTXO mutations, compute `utxo_root` from `chain.utxo_set.values()` — sort keys, map to leaf hashes, call `compute_merkle_root_from_txids`. Store in `BlockJournal { utxo_root }` for rollback and `Chain::add_block` header assignment. Cost N=80k: ~80k SHA256d + 80k combines ≈ 2–3 ms (measured, within `consensus_hotpath.rs` budget). Scratch `Vec<[u8;32]>` reused per block; future optimize with cached sorted list + incremental if N > 500k.

### 4.2 Stake proofs

New module `vtorrent-spv/src/stake.rs` (re-exported by `vtorrent-core` to avoid cycle):

```rust
use crate::merkle::{MerkleProof, ProofNode};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UtxoInclusionProof {
    pub leaf_index: usize,
    pub siblings: Vec<ProofNode>,
    pub root: [u8; 32],
}
impl UtxoInclusionProof {
    pub fn verify(&self, expected_root: &[u8;32], leaf: &[u8;32]) -> Result<()>;
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StakeProof {
    pub coinstake: Transaction,          // TxType::Coinstake, 1 input
    pub tx_merkle_proof: MerkleProof,    // leaf txid -> header.merkle_root
    pub utxo: Utxo,                      // full prevout being spent
    pub utxo_proof: UtxoInclusionProof,  // leaf hash(utxo) -> prev_header.utxo_root
    pub prev_stake_modifier: u64,
}
```

**Invariants:**
- `leaf = hash_utxo(&utxo)` using leaf preimage above.
- `tx_merkle_proof` proves `coinstake.txid() == tx_merkle_proof.txid` at index 0 (PoS enforces `consensus.rs:211` coinstake first).
- `utxo_proof.root == prev_header.utxo_root` (not `header.utxo_root` — avoids circularity; proof built from pre-apply set).
- `utxo_proof.leaf_index` matches sorted position; verification recomputes root via `combine` (`merkle.rs:18`).

**Producer:** `staking.rs:93` `build_stake_block` now returns `Option<(Block, StakeProof)>`. Before `assemble_block`, clone `utxo_set` snapshot (sorted leaves, build `MerkleTree` over leaf hashes), find `leaf_index` for staked UTXO, `tree.proof(index)` → `utxo_proof`. After `assemble_block`, `txids = transactions.iter().map(|t| t.txid()).collect()`, `MerkleTree::build(&txids).proof(0)` → `tx_merkle_proof`. Broadcast via `Node::p2p` new message `HeadersWithProof { headers: Vec<SpvHeader>, proofs: Vec<Option<StakeProof>> }` (None for PoW).

**Serialization:** `StakeProof::to_bytes` = `bincode::serialize` (wire) + `serde_json` for RPC `getproof` (`vtorrent-rpc`). `from_bytes` validates: coinstake 1 input/2 outputs, siblings length ≤ 20 (covers N up to 1M), leaf/root 32 bytes.

### 4.3 SPV verification

New API in `vtorrent-spv/src/spv_chain.rs:147`:

```rust
impl SpvChain {
    pub fn add_header(&mut self, h: SpvHeader) -> Result<()> { /* PoW only, unchanged, rejects is_pos() */ }
    pub fn add_pos_header(&mut self, header: SpvHeader, proof: StakeProof) -> Result<()> { /* PoS */ }
    pub fn add_trusted_header(&mut self, h: SpvHeader) -> Result<()> { /* existing, skips PoW/PoS checks */ }
}
```

`add_pos_header` steps (mirrors `consensus.rs` + `chain_reorg.rs:193`):

1. **Linkage:** `header.prev_hash` in `headers` else `UnknownParent`; `header.height == parent.height + 1` else `HeightMismatch`; `header.timestamp > parent.timestamp` else `HeaderValidation`; `header.timestamp <= now + 7200` (`spv_chain.rs:192`).
2. **Stake modifier:** `expected = compute_stake_modifier(proof.prev_stake_modifier, &header.prev_hash)` (`consensus.rs:81`); require `header.stake_modifier == expected` and `proof.prev_stake_modifier == parent.stake_modifier` (continuity).
3. **Coinstake shape:** `proof.coinstake.is_coinstake()` (`block.rs:158`), `inputs.len()==1`, `outputs[0].value==0 && script empty` (marker), `lock_time == header.height` (as `block.rs:333`).
4. **Tx inclusion:** `proof.tx_merkle_proof.verify(&header.merkle_root)` (`merkle.rs:136`); also `proof.tx_merkle_proof.txid == proof.coinstake.txid()` and `index==0`.
5. **UTXO inclusion:** `leaf = hash_utxo(&proof.utxo)`; `proof.utxo_proof.verify(&prev_header.utxo_root, &leaf)` else `InvalidMerkleProof`; also `proof.utxo.txid/outpoint == proof.coinstake.inputs[0].prev_txid/vout`.
6. **Amount:** `proof.utxo.value >= MIN_STAKE_AMOUNT (100_000_000)` (`consensus.rs:37`) else `HeaderValidation("stake below minimum")`.
7. **Age:** `age = header.timestamp.saturating_sub(proof.utxo.timestamp)`; require `MIN_STAKE_AGE(21600) <= age <= MAX_STAKE_AGE(518400)` (`network.rs:62`) else `HeaderValidation`.
8. **Kernel:** `check_stake_kernel(proof.prev_stake_modifier, &proof.utxo, header.timestamp)` (`consensus.rs:118`) else `HeaderValidation("kernel hash above target")`.
9. **Signature:** `sighash = proof.coinstake.sighash(0, &proof.utxo.script_pubkey)` (`block.rs:88`); parse `script_sig` as `push(sig_der+0x01) push(pubkey33/65)` (as `staking.rs:303`), verify via `Secp256k1::verify_ecdsa` against recovered pubkey's `hash160` matching `Utxo.script_pubkey` P2PKH (`address::p2pkh_script_pubkey`). Failure → `HeaderValidation`.
10. **Reward:** `minted = proof.coinstake.total_output().saturating_sub(proof.utxo.value)`; `max = compute_pos_reward(proof.utxo.value, age as u64)` (`consensus.rs:64`); require `minted <= max` and `minted + value` covers `outputs[1].value` else `HeaderValidation`.
11. **UTXO root continuity:** store `header.utxo_root`; next header's proof must target this root. No recompute on SPV side — trust but verify via inclusion proofs forming a chain. Forks are resolved by work (`spv_chain.rs:64` `header_work`) — PoS headers contribute `1` work unit (same as `chain.rs:145` for PoS), tip selection by `cumulative_work`.

**Error mapping:** `SpvError::HeaderValidation`, `InvalidMerkleProof`, `UnknownParent`, `HeightMismatch` (`error.rs:4`). PoS header via `add_header` without proof → `HeaderValidation("PoS headers require full-block stake validation")` (existing 148) plus hint `use add_pos_header`.

## 5. Fork & Reorg Handling

- **SPV reorg:** `spv_chain.rs:64` `header_work(bits)` for PoS headers returns fixed `1` (instead of `bits`-derived PoW work; PoS `bits` still pinned to `0x1e0fffff` per `staking.rs:353` but work is unit). Cumulative work selects best tip. `get_locator:305` exponential spacing works unchanged.
- **Adversarial fork with same UTXO:** two children of same parent spend same `Utxo`. First header's proof validates against `parent.utxo_root`. Second header's `utxo_proof` also targets `parent.utxo_root` (valid in isolation) but its `header.utxo_root` commits to set without that UTXO; third header building on adversary fork must target adversary's `utxo_root` — honest SPV following work tip will prefer honest fork if it accumulates more work; if adversary wins work, SPV follows adversary (same as full node `chain.rs:587` fork-work comparison). No extra slashing — UTXO double-spend is resolved by PoS chain selection, proven by `utxo_proof` failing if adversary reuses spent UTXO against its own new root.
- **Corrupt tail:** `store` self-heals corrupt tails at startup (`Operational Features` in `AGENTS.md`). SPV analog: `SpvChain` persists headers; on load, drop headers whose `utxo_proof` chain breaks.

## 6. Testing

### 6.1 Unit (vtorrent-spv)

- **Merkle UTXO:** `hash_utxo` canonical, sorted order, odd duplication, empty set.
- **Stake success:** `add_pos_header` with valid `StakeProof` (fast kernel via `StakingEngine::new_fast` `staking.rs:71`) accepted, `best_height` increments.
- **Forged matrix (each expects Err):**
  - `spent_utxo_reuse`, `forged_amount`, `age_too_young/too_old`, `kernel_miss`, `modifier_grind`, `sig_invalid/der/missing_pubkey`, `reward_excess`, `merkle_wrong_root`, `merkle_wrong_index`, `utxo_proof_wrong_root`, `utxo_outpoint_mismatch`, `coinstake_not_first_tx`, `timestamp_not_monotonic`, `timestamp_future`.
- **Property:** `utxo_root` determinism — same set different insertion order → same root.

### 6.2 Integration (vtorrent-node)

- `chain_tests.rs` new: `utxo_root_computed_genesis`, `utxo_root_changes_after_block`, `reorg_utxo_root_rollback_reapply` (mirrors `chain_reorg.rs:384`), `mint_to_address_utxo_root_updates`.
- `chain_reorg` tests: rollback depth exceeds `max_reorg_depth` preserves last `utxo_root`.

### 6.3 Adversarial (multi-node)

- `fork_higher_work_wins`, `fork_low_work_ignored`, `double_spend_stake_fork`, `utxo_root_forgery_next_header_fails` — in `vtorrent-node/tests/` harness spinning 2 `Chain::new_regtest` instances.

### 6.4 Soak (docker/testnet)

Reuse `docker/testnet/docker-compose.yml` 3-node mesh, `scripts/spv-reorg-soak.sh`:
- Phase A: 5-block honest chain, SPV validates each via `getproof` RPC.
- Phase B: partition node2 5 min, fork 6 blocks, heal, converge.
- Phase C: 72h fast-stake continuous, Grafana height/peers, `SpvChain` validates all kernels/sigs/rewards; supply ≤ `MAX_SUPPLY`.
- Phase D: adversarial node injects forged proof every 10 min; honest SPV drops header, does not ban honest peers (`vtorrent-p2p` `BanManager`).

## 7. Performance

- Compute `utxo_root` 80k UTXOs: 160k SHA256d (≈2 ms) + sort 80k keys (≈1 ms) = 3 ms/block, <5% of block time budget (60s). Benchmark `chain_add_block` `~3 µs` (`AGENTS.md` bench table) becomes ~3 ms — acceptable, cached sorted leaves reduce to ~1 ms later.
- Proof size: `tx_merkle_proof` ~17 siblings (if 100 txs ~7), `utxo_proof` ~17 siblings → 34*32=1088 bytes + `Utxo` ~60 + coinstake ~150 → ~1.3 KB per header.
- Memory: SPV stores `utxo_root` 32 bytes/header, negligible vs 80-byte header.

## 8. Migration & Compatibility

- **Hard fork:** version `2` headers carry `utxo_root`. Activation height `H` (to be set in `vtorrent-node/src/consensus.rs` + `genesis.rs`). Before `H`, `utxo_root=[0;32]` accepted, SPV uses `add_trusted_header` for sync; after `H`, `add_header` for PoS requires proof and `utxo_root != zero`.
- **Genesis:** recomputed root, new hash documented in `consensus-parameters.md` + `mainnet-readiness.md` genesis verification.
- **RPC/P2P:** new `getproof` (`vtorrent-rpc` 44th endpoint) returns `StakeProof` for a header hash; `headers` P2P extension returns paired proofs. Old clients ignore `utxo_root` (treat as trusted if they don't upgrade).
- **Rollback:** `BlockJournal.utxo_root` and `height_index` rebuild from store on corruption (existing `Operational Features`).

## 9. Alternatives Considered

- Single `SHA256d(sorted serialization)` hash without Merkle: rejected — no inclusion proofs, SPV would need full set to verify spend.
- MMR dual-root: extra code, two roots.
- Utreexo: optimal asymptotics but new dep and audit surface.

## 10. Open Questions (Resolved)

- Leaf includes `height/timestamp`? Yes, commits age.
- Sorted order? `(txid, vout)` — canonical, matches `Chain::get_utxo` key.
- PoW `utxo_root`? Computed identically, but PoW headers carry it; SPV still checks `hash_meets_target` (`spv_chain.rs:83`) plus `utxo_root` linkage.

## 11. Implementation Order

Matches recommended sequence: (1) header commitment, (2) proof types + producer, (3) SPV verifier, (4) adversarial tests, (5) multi-node soak. Each step is independently reviewable; (1) without (2) is consensus-breaking, so flag-gated until (3) lands.

