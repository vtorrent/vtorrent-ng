# Genesis Bootstrap + Stakeable-Script Restriction — Implementation Plan

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax for tracking. Each
> task is TDD: write the failing test, run it, implement, re-run, then run the
> full workspace gate.

**Goal:** Close review findings T3 (genesis bootstrap deadlock) and T4
(`is_stakeable` over-counts non-P2PKH outputs) without changing any existing
block's validity, so the running soak chain replays unchanged.

**Architecture:** T4 tightens `is_stakeable` to P2PKH-only (one function, both
call sites). T3 adds a height-1 "bootstrap block" whose only transaction is a
`LegacyClaim`; its P2PKH output seeds `total_staked`, and the block is produced
by a new `POST /api/v1/blockchain/bootstrap` endpoint that mirrors the regtest
faucet's direct-`Chain` block construction.

**Tech Stack:** Rust 2021, `cargo test --workspace`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo fmt --all -- --check`, `cargo machete`.

**Spec:** `docs/superpowers/specs/2026-09-21-genesis-bootstrap-and-stakeable-script-design.md`

---

## File Structure

**Modified:**
- `vtorrent-node/src/consensus.rs` — `is_stakeable` (T4); `validate_block_inner`
  height-1 bootstrap acceptance (T3)
- `vtorrent-node/src/chain/chain_reorg.rs` — bootstrap output requirement; age
  exemption
- `vtorrent-node/src/chain.rs` — `Chain::apply_bootstrap_claim`
- `vtorrent-rpc/src/handlers/dex.rs` — extract `build_legacy_claim_tx`; add
  `bootstrap_chain`
- `vtorrent-rpc/src/handlers/mod.rs` — re-export (already `pub use dex::*`)
- `vtorrent-rpc/src/models.rs` — `BootstrapRequest` / `BootstrapResponse`
- `vtorrent-rpc/src/server.rs` — route
- `docs/code-review-2026-09-20-fix-status.md` — move T3/T4 to Fixed
- `docs/mainnet-readiness.md` — note the bootstrap path

**No new files.**

---

### Task 1: T4 — restrict `is_stakeable` to P2PKH

**Files:**
- Modify: `vtorrent-node/src/consensus.rs:147-152`
- Test: `vtorrent-node/src/consensus.rs` (extend `test_is_stakeable_excludes_op_return_and_dust`)

- [ ] **Step 1: Write the failing test**

In `vtorrent-node/src/consensus.rs`, extend the existing test near line 697.
Add a helper that builds a UTXO with an arbitrary script, then assert each
non-P2PKH class is rejected:

```rust
#[test]
fn test_is_stakeable_accepts_only_p2pkh() {
    use crate::chain::Utxo;

    fn utxo_with_script(script: Vec<u8>) -> Utxo {
        Utxo {
            txid: [7u8; 32],
            vout: 0,
            value: MIN_STAKE_AMOUNT,
            script_pubkey: script,
            height: 1,
            timestamp: 0,
        }
    }

    // P2PKH (25 bytes) is the only stakeable class.
    let mut p2pkh = vec![0x76, 0xa9, 0x14];
    p2pkh.extend_from_slice(&[0x11u8; 20]);
    p2pkh.extend_from_slice(&[0x88, 0xac]);
    assert!(is_stakeable(&utxo_with_script(p2pkh)));

    // P2SH: OP_HASH160 <20> OP_EQUAL
    let mut p2sh = vec![0xa9, 0x14];
    p2sh.extend_from_slice(&[0x22u8; 20]);
    p2sh.push(0x87);
    assert!(!is_stakeable(&utxo_with_script(p2sh)));

    // P2PK: <33-byte pubkey> OP_CHECKSIG
    let mut p2pk = vec![33];
    p2pk.extend_from_slice(&[0x33u8; 33]);
    p2pk.push(0xac);
    assert!(!is_stakeable(&utxo_with_script(p2pk)));

    // OP_RETURN
    assert!(!is_stakeable(&utxo_with_script(vec![0x6a, 0x01, 0x00])));

    // NonStandard
    assert!(!is_stakeable(&utxo_with_script(vec![0x51, 0x52])));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p vtorrent-node test_is_stakeable_accepts_only_p2pkh -- --nocapture`
Expected: FAIL — P2SH/P2PK/NonStandard currently return `true`.

- [ ] **Step 3: Implement the change**

Replace `is_stakeable` (`consensus.rs:147`):

```rust
/// Whether `utxo` counts toward the staked supply for the v2 kernel rule.
///
/// Only P2PKH outputs at or above the minimum stake amount can ever win a
/// kernel: the staking engine only stakes a UTXO whose script equals its own
/// P2PKH script (`staking.rs:242`). Counting P2SH/P2MS/P2PK/HTLC/NonStandard
/// outputs would let a holder park coins to dilute every honest staker's hit
/// probability without ever winning a kernel.
pub fn is_stakeable(utxo: &Utxo) -> bool {
    utxo.value >= MIN_STAKE_AMOUNT
        && vtorrent_script::classify_script(
            &vtorrent_script::Script::from_bytes(utxo.script_pubkey.clone()).unwrap_or_default(),
        ) == vtorrent_script::ScriptType::P2PKH
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-node test_is_stakeable -- --nocapture`
Expected: PASS (both the new test and `test_is_stakeable_excludes_op_return_and_dust`).

- [ ] **Step 5: Run the node test suite**

Run: `cargo test -p vtorrent-node`
Expected: PASS — `test_total_staked_tracks_stakeable_utxos` and all chain tests
still green (the soak chain is P2PKH-only, so this is a no-op there).

- [ ] **Step 6: Commit**

```bash
git add vtorrent-node/src/consensus.rs
git commit -m "fix(node): count only P2PKH outputs as stakeable (T4)"
```

---

### Task 2: T3a — accept a height-1 bootstrap claim block

**Files:**
- Modify: `vtorrent-node/src/consensus.rs:269-289`
- Test: `vtorrent-node/src/consensus.rs` (new test module helper already present)

- [ ] **Step 1: Write the failing test**

Add to `vtorrent-node/src/consensus.rs` tests:

```rust
#[test]
fn test_height1_bootstrap_claim_block_is_valid() {
    // A PoS block at height 1 whose only tx is a LegacyClaim is a bootstrap
    // block. Build a minimal claim and validate it as a block.
    let claim = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: MIN_STAKE_AMOUNT,
            script_pubkey: {
                let mut s = vec![0x76, 0xa9, 0x14];
                s.extend_from_slice(&[0x11u8; 20]);
                s.extend_from_slice(&[0x88, 0xac]);
                s
            },
        }],
        lock_time: 1,
        claim_address: Some("VTest".into()),
        claim_signature: Some(vec![0u8; 65]),
    };
    let mut block = make_test_block(claim, GENESIS_BITS, 0);
    block.header.prev_block_hash = [9u8; 32];
    block.header.stake_modifier = compute_stake_modifier(0, &[9u8; 32]);
    block.header.merkle_root = block.compute_merkle_root();

    // prev_height = 0, allow_pow = false (production).
    let res = validate_block_inner(
        &block,
        0,
        1_700_000_000,
        GENESIS_BITS,
        0,
        [9u8; 32],
        false,
    );
    assert!(res.is_ok(), "bootstrap claim block must validate: {:?}", res);
}

#[test]
fn test_height2_claim_block_is_rejected() {
    // The bootstrap rule is height-1 only.
    let claim = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: MIN_STAKE_AMOUNT,
            script_pubkey: vec![0x76, 0xa9, 0x14, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11,
                0x11, 0x11, 0x11, 0x11, 0x88, 0xac],
        }],
        lock_time: 2,
        claim_address: Some("VTest".into()),
        claim_signature: Some(vec![0u8; 65]),
    };
    let mut block = make_test_block(claim, GENESIS_BITS, 0);
    block.header.prev_block_hash = [9u8; 32];
    block.header.stake_modifier = compute_stake_modifier(0, &[9u8; 32]);
    block.header.merkle_root = block.compute_merkle_root();

    let res = validate_block_inner(&block, 1, 1_700_000_000, GENESIS_BITS, 0, [9u8; 32], false);
    assert!(res.is_err(), "claim block at height 2 must be rejected");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vtorrent-node test_height1_bootstrap_claim_block_is_valid -- --nocapture`
Expected: FAIL — "PoS block must begin with a coinstake transaction".

- [ ] **Step 3: Implement the acceptance rule**

In `validate_block_inner` (`consensus.rs:271`), replace the PoS/PoW branch:

```rust
    // vTorrent-NG is a PoS-only chain. The test-only PoW path exists for
    // deterministic chain-state fixtures and is never enabled by production.
    let first_tx = &block.transactions[0];
    if block.header.is_pos() {
        // Height 1 may be a bootstrap block: a single LegacyClaim that seeds
        // the first stakeable UTXO. Genesis has no stakeable output, so no
        // coinstake can exist yet (T3). The claim is validated in the journal.
        let is_bootstrap = prev_height == 0
            && first_tx.tx_type == TxType::LegacyClaim
            && block.transactions.len() == 1;
        if first_tx.tx_type != TxType::Coinstake && !is_bootstrap {
            return Err(NodeError::InvalidBlock(
                "PoS block must begin with a coinstake transaction".into(),
            ));
        }
    } else {
        if !allow_pow {
            return Err(NodeError::InvalidBlock(
                "Proof-of-Work blocks are not permitted".into(),
            ));
        }
        if first_tx.tx_type != TxType::Coinbase {
            return Err(NodeError::InvalidBlock(
                "PoW test block must begin with a coinbase transaction".into(),
            ));
        }
    }
```

- [ ] **Step 4: Run both tests**

Run: `cargo test -p vtorrent-node test_height1_bootstrap_claim_block_is_valid test_height2_claim_block_is_rejected -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the node suite**

Run: `cargo test -p vtorrent-node`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-node/src/consensus.rs
git commit -m "feat(node): accept height-1 bootstrap claim block (T3)"
```

---

### Task 3: T3b — bootstrap output requirement + age exemption

**Files:**
- Modify: `vtorrent-node/src/chain/chain_reorg.rs:312-333` (claim branch) and
  `:380-386` (age check)
- Test: `vtorrent-node/src/chain/chain_tests.rs`

- [ ] **Step 1: Write the failing tests**

Add to `vtorrent-node/src/chain/chain_tests.rs`:

```rust
#[test]
fn test_bootstrap_block_must_create_stakeable_output() {
    // A height-1 claim block whose output is below MIN_STAKE_AMOUNT must be
    // rejected: it would not unblock staking.
    // Build via Chain::apply_bootstrap_claim once implemented; for now assert
    // the journal rule through a direct block apply.
    // (Implemented in Task 4; this test is completed there.)
}

#[test]
fn test_bootstrap_utxo_age_exempt_at_height2() {
    // A coinstake spending a height-1 UTXO at height 2 is accepted with ~0 age.
    // (Implemented in Task 4; this test is completed there.)
}
```

> Note: these two tests are written fully in Task 4, where
> `Chain::apply_bootstrap_claim` exists to construct the fixtures. Task 3
> implements the journal rules they exercise.

- [ ] **Step 2: Implement the bootstrap output requirement**

In `apply_block_journaled` (`chain_reorg.rs`), inside the `if tx.is_legacy_claim()`
branch, after the existing validation and before `Ok`, add:

```rust
            // A height-1 bootstrap block exists solely to seed the first
            // stakeable UTXO. If its claim output is below MIN_STAKE_AMOUNT it
            // cannot unblock staking, so reject it.
            if height == 1 {
                let stakeable = tx
                    .outputs
                    .iter()
                    .any(|o| crate::consensus::is_stakeable(&Utxo {
                        txid,
                        vout: 0,
                        value: o.value,
                        script_pubkey: o.script_pubkey.clone(),
                        height,
                        timestamp,
                    }));
                if !stakeable {
                    return Err(NodeError::InvalidTransaction(
                        "Bootstrap claim must create a stakeable (P2PKH >= MIN_STAKE_AMOUNT) output".into(),
                    ));
                }
            }
```

(Use the correct `vout` per output; iterate with `enumerate()`.)

- [ ] **Step 3: Implement the age exemption**

In the coinstake age check (`chain_reorg.rs:381`), replace:

```rust
        let coin_age = u64::from(coin_age);
        if coin_age < chain.min_stake_age || coin_age > chain.max_stake_age {
```

with:

```rust
        let coin_age = u64::from(coin_age);
        // The height-1 bootstrap UTXO is brand new at height 2, so it is
        // exempt from the minimum age for exactly that one block. The only
        // UTXO created at height 1 is the bootstrap claim's output, and it can
        // only be staked at height 2, so the exemption expires naturally.
        let min_age_ok = staked.height == 1 || coin_age >= chain.min_stake_age;
        if !min_age_ok || coin_age > chain.max_stake_age {
```

- [ ] **Step 4: Build and run the node suite**

Run: `cargo test -p vtorrent-node`
Expected: PASS (no behavior change for existing fixtures; height 1 in regtest is
a faucet PoW block, so the claim branch is not hit).

- [ ] **Step 5: Commit**

```bash
git add vtorrent-node/src/chain/chain_reorg.rs
git commit -m "feat(node): bootstrap output requirement and height-1 age exemption (T3)"
```

---

### Task 4: T3c — `Chain::apply_bootstrap_claim`

**Files:**
- Modify: `vtorrent-node/src/chain.rs` (add method near `mint_to_address:246`)
- Test: `vtorrent-node/src/chain/chain_tests.rs`

- [ ] **Step 1: Write the failing integration test**

Replace the two stubs from Task 3 with real tests:

```rust
#[test]
fn test_bootstrap_claim_unblocks_staking() {
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    let secp = Secp256k1::new();
    let mut key_bytes = [0u8; 32];
    key_bytes[31] = 42;
    let secret = SecretKey::from_slice(&key_bytes).unwrap();
    let pubkey = PublicKey::from_secret_key(&secp, &secret);
    let address = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70).to_string();

    let mut chain = Chain::new().expect("Chain init failed");
    assert_eq!(chain.best_height(), 0);
    assert_eq!(chain.total_staked(), 0);

    let claim = build_test_claim(&address, 100 * crate::consensus::COIN);
    let block_hash = chain
        .apply_bootstrap_claim(claim)
        .expect("bootstrap claim should apply");

    assert_eq!(chain.best_height(), 1);
    assert_eq!(chain.block_hash_at_height(1), Some(block_hash));
    assert_eq!(
        chain.total_staked(),
        100 * crate::consensus::COIN,
        "bootstrap output must seed the staking denominator"
    );
}

#[test]
fn test_bootstrap_claim_rejected_when_not_at_genesis() {
    let mut chain = Chain::new().expect("Chain init failed");
    let claim = build_test_claim("VTest", 100 * crate::consensus::COIN);
    chain.apply_bootstrap_claim(claim.clone()).expect("first ok");
    assert!(
        chain.apply_bootstrap_claim(claim).is_err(),
        "bootstrap is one-shot: rejected once height > 0"
    );
}
```

Add a test helper in `chain_tests.rs`:

```rust
fn build_test_claim(recipient: &str, amount: u64) -> crate::block::Transaction {
    use crate::block::{Transaction, TxOutput, TxType};
    let script = vtorrent_core::address::Address::parse(recipient)
        .map(|a| a.p2pkh_script_pubkey())
        .unwrap_or_else(|_| {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&[0x11u8; 20]);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        });
    Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: vec![TxOutput { value: amount, script_pubkey: script }],
        lock_time: 1,
        claim_address: Some(recipient.into()),
        claim_signature: Some(vec![0u8; 65]),
    }
}
```

> Note: `apply_bootstrap_claim` is a chain-level method that does **not** verify
> the claim signature (the RPC layer builds a signed claim; the journal's
> `validate_legacy_claim` runs on `add_block`). For the test, either use a
> recipient that is a real snapshot address with a valid signature, or add a
> `#[cfg(test)]` bypass. Prefer: use `mint`-style construction and assert the
> journal rules by calling `apply_bootstrap_claim` on a chain whose
> `claimed_addresses` check is satisfied. If signature verification blocks the
> fixture, construct the claim via the same helper the RPC uses
> (`build_legacy_claim_tx`) with a known snapshot WIF from the test fixtures.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vtorrent-node test_bootstrap_claim_unblocks_staking -- --nocapture`
Expected: FAIL — `apply_bootstrap_claim` does not exist.

- [ ] **Step 3: Implement `Chain::apply_bootstrap_claim`**

Add to `vtorrent-node/src/chain.rs` after `mint_to_address`:

```rust
    /// Apply the height-1 bootstrap claim block.
    ///
    /// Genesis has no stakeable UTXO, so no coinstake can be produced (T3).
    /// The first legacy claim is mined directly into a height-1 PoS block whose
    /// only transaction is the claim; its P2PKH output seeds `total_staked` and
    /// unblocks normal PoS from height 2.
    ///
    /// One-shot: only valid while the chain is at genesis.
    pub fn apply_bootstrap_claim(&mut self, claim: Transaction) -> Result<[u8; 32]> {
        if self.best_height() != 0 {
            return Err(NodeError::Chain(
                "Bootstrap claim is only valid at height 1 (chain is not at genesis)".into(),
            ));
        }
        if claim.tx_type != TxType::LegacyClaim {
            return Err(NodeError::InvalidTransaction(
                "Bootstrap transaction must be a LegacyClaim".into(),
            ));
        }

        let genesis = self
            .genesis_block()
            .clone();
        let genesis_hash = genesis.hash();
        let timestamp = now_timestamp_u32().max(genesis.header.timestamp.saturating_add(1));

        let mut block = Block {
            header: BlockHeader {
                version: 2,
                prev_block_hash: genesis_hash,
                merkle_root: [0u8; 32],
                utxo_root: [0u8; 32],
                timestamp,
                bits: crate::genesis::GENESIS_BITS,
                nonce: 0, // PoS
                stake_modifier: compute_stake_modifier(genesis.header.stake_modifier, &genesis_hash),
            },
            transactions: vec![claim.clone()],
        };
        block.header.merkle_root = block.compute_merkle_root();

        // Post-apply UTXO root: genesis UTXO set plus the claim's outputs (the
        // claim has no inputs). Must match the journal or `add_block` rejects
        // the PoS block on the utxo_root check.
        let txid = claim.txid();
        let mut post: Vec<Utxo> = self.utxo_set.values().cloned().collect();
        for (vout, output) in claim.outputs.iter().enumerate() {
            post.push(Utxo {
                txid,
                vout: vout as u32,
                value: output.value,
                script_pubkey: output.script_pubkey.clone(),
                height: 1,
                timestamp,
            });
        }
        block.header.utxo_root = crate::block::compute_utxo_root_sorted(&post);

        self.add_block(block)?;
        Ok(self.best_hash().unwrap_or([0u8; 32]))
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p vtorrent-node test_bootstrap -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the node suite**

Run: `cargo test -p vtorrent-node`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-node/src/chain.rs vtorrent-node/src/chain/chain_tests.rs
git commit -m "feat(node): Chain::apply_bootstrap_claim builds the height-1 block (T3)"
```

---

### Task 5: T3d — extract `build_legacy_claim_tx`

**Files:**
- Modify: `vtorrent-rpc/src/handlers/dex.rs:214-338`
- Test: `vtorrent-rpc` (or a unit test in `dex.rs`)

- [ ] **Step 1: Write the failing test**

Add to `vtorrent-rpc/src/handlers/dex.rs` (or its test module):

```rust
#[test]
fn test_build_legacy_claim_tx_matches_submit_claim_output() {
    // For a known snapshot WIF + recipient, the extracted helper must produce
    // the same transaction (txid) the old inline code produced.
    // Use a fixture WIF whose derived address is in the snapshot.
    let (wif, recipient) = claim_fixture();
    let tx = build_legacy_claim_tx(&wif, &recipient).expect("helper builds");
    assert_eq!(tx.tx_type, vtorrent_node::block::TxType::LegacyClaim);
    assert_eq!(tx.claim_address.as_deref(), Some(/* derived address */));
    assert!(tx.claim_signature.is_some());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vtorrent-rpc test_build_legacy_claim_tx_matches_submit_claim_output -- --nocapture`
Expected: FAIL — helper does not exist.

- [ ] **Step 3: Extract the helper**

Move the body of `submit_claim` from the WIF parse (`dex.rs:237`) through the
transaction construction (`dex.rs:308`) into:

```rust
/// Build and sign a legacy claim transaction.
///
/// Shared by `POST /api/v1/claim/submit` and
/// `POST /api/v1/blockchain/bootstrap` so the v2 signature construction exists
/// in exactly one place.
pub fn build_legacy_claim_tx(
    wif_private_key: &str,
    recipient_address: &str,
) -> Result<Transaction, RpcError> {
    // ... existing WIF parse, address derivation, balance lookup, output
    //     construction, v2 message hash, recoverable signature, Transaction ...
}
```

`submit_claim` becomes: parse request → call `build_legacy_claim_tx` → check
`chain.is_claimed` → `mempool.admit_with_chain_fee` → respond. Keep the
`claimable` value available for the response (return it from the helper or
recompute via `get_legacy_balance`).

- [ ] **Step 4: Run the test**

Run: `cargo test -p vtorrent-rpc test_build_legacy_claim_tx -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the RPC suite**

Run: `cargo test -p vtorrent-rpc`
Expected: PASS — existing claim tests unchanged.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-rpc/src/handlers/dex.rs
git commit -m "refactor(rpc): extract build_legacy_claim_tx from submit_claim (T3)"
```

---

### Task 6: T3e — bootstrap RPC endpoint

**Files:**
- Modify: `vtorrent-rpc/src/models.rs` (add request/response)
- Modify: `vtorrent-rpc/src/handlers/dex.rs` (add `bootstrap_chain`)
- Modify: `vtorrent-rpc/src/server.rs` (route)
- Test: `vtorrent-rpc`

- [ ] **Step 1: Add models**

In `vtorrent-rpc/src/models.rs`, next to `ClaimSubmitRequest`:

```rust
/// Request body for `POST /api/v1/blockchain/bootstrap`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapRequest {
    /// WIF-encoded private key for a legacy snapshot address.
    pub wif_private_key: String,
    /// New-chain address to receive the bootstrap claim.
    pub recipient_address: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BootstrapResponse {
    pub txid: String,
    pub block_hash: String,
    pub block_height: u64,
    pub claimed_satoshis: u64,
    pub recipient_address: String,
}
```

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn test_bootstrap_endpoint_rejects_when_not_at_genesis() {
    // Build an AppState with a chain at height > 0 and assert the handler
    // returns BadRequest.
}
```

- [ ] **Step 3: Implement the handler**

In `vtorrent-rpc/src/handlers/dex.rs`:

```rust
/// POST /api/v1/blockchain/bootstrap
///
/// Mines the height-1 bootstrap claim block. Genesis has no stakeable UTXO, so
/// this is the only way a fresh chain can begin staking (T3). One-shot: valid
/// only while the chain is at genesis.
pub async fn bootstrap_chain(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BootstrapRequest>,
) -> RpcResult<Json<BootstrapResponse>> {
    let claim = build_legacy_claim_tx(&req.wif_private_key, &req.recipient_address)?;
    let claimed = claim.total_output();
    let txid = hex::encode(claim.txid());

    let (block_hash, height) = {
        let mut chain = state.chain.lock().await;
        if chain.best_height() != 0 {
            return Err(RpcError::BadRequest(
                "Bootstrap is only valid while the chain is at genesis".into(),
            ));
        }
        let hash = chain
            .apply_bootstrap_claim(claim)
            .map_err(|e| RpcError::BadRequest(format!("Bootstrap claim rejected: {}", e)))?;
        (hex::encode(hash), chain.best_height() as u64)
    };

    // Announce to peers, mirroring the regtest faucet path.
    if let Some(sender) = &state.block_submit {
        let chain = state.chain.lock().await;
        if let Some(block) = chain.get_block_at_height(height as u32).cloned() {
            let _ = sender.try_send(block);
        }
    }

    Ok(Json(BootstrapResponse {
        txid,
        block_hash,
        block_height: height,
        claimed_satoshis: claimed,
        recipient_address: req.recipient_address,
    }))
}
```

- [ ] **Step 4: Add the route**

In `vtorrent-rpc/src/server.rs`, next to `/api/v1/claim/submit`:

```rust
        .route("/api/v1/blockchain/bootstrap", post(bootstrap_chain))
```

- [ ] **Step 5: Run the RPC suite**

Run: `cargo test -p vtorrent-rpc`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-rpc/src/models.rs vtorrent-rpc/src/handlers/dex.rs vtorrent-rpc/src/server.rs
git commit -m "feat(rpc): POST /api/v1/blockchain/bootstrap (T3)"
```

---

### Task 7: Full gate, docs, and review ledger

**Files:**
- Modify: `docs/code-review-2026-09-20-fix-status.md`
- Modify: `docs/mainnet-readiness.md`
- Modify: `docs/rpc-api.md`

- [ ] **Step 1: Run the full workspace gate**

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo machete
```

Expected: all green. `cargo test --workspace` must show the same pass count plus
the new tests.

- [ ] **Step 2: Update the fix-status ledger**

In `docs/code-review-2026-09-20-fix-status.md`, move T3 and T4 from `## Open`
into the `## Fixed` table with the commit hashes, and delete their `### T3` /
`### T4` sections from `## Open`.

- [ ] **Step 3: Update mainnet-readiness**

In `docs/mainnet-readiness.md`, under "Code & Consensus Verification", add a
checked item:

```markdown
- [x] **Genesis bootstrap path**: height-1 bootstrap claim block defined and
      tested; `POST /api/v1/blockchain/bootstrap` mines it. See
      `docs/superpowers/specs/2026-09-21-genesis-bootstrap-and-stakeable-script-design.md`.
```

- [ ] **Step 4: Document the endpoint**

In `docs/rpc-api.md`, add `POST /api/v1/blockchain/bootstrap` with its request /
response shape and the "genesis-only, one-shot" note.

- [ ] **Step 5: Commit**

```bash
git add docs/
git commit -m "docs: mark T3/T4 fixed; document bootstrap endpoint"
```

---

## Rollout Note

These changes are no-ops on every existing chain, so they can land on `main`
without a fleet redeploy. **Do not deploy them to the soak fleet during the
window** (the fleet stays on `vtorrent/node:13489d4`); they ship with the
post-soak batch and `v2.0.0-beta.3`. The bootstrap endpoint is inert until a
fresh mainnet genesis exists.
