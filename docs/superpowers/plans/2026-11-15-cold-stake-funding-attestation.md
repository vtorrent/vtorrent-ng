# Cold-stake funding attestation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the offline `cold-stake spend` signer refuse unless a pinned node key signs an attestation that the VTR funding outpoint is confirmed, unspent, and free of mempool spends.

**Architecture:** A deterministic byte encoding and secp256k1 signature live in `vtorrent-core`. The RPC crate builds attestations from the chain and mempool and serves them behind the API-key middleware. The CLI verifies the signature, binds every field to the funding transaction, applies expiry and confirmation policy, and then runs the existing fee and confirmation flow.

**Tech Stack:** Rust 2021, `secp256k1` 0.29, `sha2`, `serde`, `axum`, `clap`, `thiserror`.

**Spec:** `docs/superpowers/specs/2026-11-15-cold-stake-funding-attestation-design.md`

## Global Constraints

- Rust edition 2021; shared dependencies stay in the root `Cargo.toml` under `[workspace.dependencies]`.
- Library errors use `thiserror`. Binaries may use `anyhow`.
- Logging uses `tracing`, never `println!`, in library code.
- No comments unless the code cannot explain itself. Public items get `///` docs.
- Do not change consensus rules, genesis, or the active soak.
- Do not stage `checkpoint.md`, `graft/`, `research/`, or `vtorrent-tauri/gen/schemas/`.
- Domain tag is exactly `vtorrent-cold-stake-funding-attestation-v1`.
- Default minimum confirmations is 6. Default attestation lifetime is 3600 seconds.
- Attestor key path is `<data-dir>/cold-stake-attestor.key` with mode 0600.

## Review Focus

- A correct attestation generated seconds ago on a machine whose clock is ahead of the signer must fail with a message that shows both timestamps, not a bare "expired".
- A node behind the chain tip must still produce an attestation; the signer must display the signed `tip_height` so the operator can spot a stale view.
- Attestation JSON with unknown or missing fields must be rejected, not silently ignored.
- A funding transaction whose value or script differs from the attestation must be rejected and name the field.
- A signature with trailing bytes or a non-canonical encoding must be rejected.

---

### Task 1: Attestation type, encoding, signing, verification

**Files:**
- Create: `vtorrent-core/src/attestation.rs`
- Modify: `vtorrent-core/src/lib.rs` (add `pub mod attestation;`)
- Test: unit tests inside `vtorrent-core/src/attestation.rs`

**Interfaces:**
- Produces:
  - `pub const ATTESTATION_DOMAIN: &[u8]`
  - `pub const ATTESTATION_VERSION: u8`
  - `pub struct FundingAttestation` with fields `version: u8`, `network: String`, `txid: String`, `vout: u32`, `value_satoshis: u64`, `script_pubkey: String`, `block_hash: String`, `block_height: u32`, `confirmations: u32`, `tip_hash: String`, `tip_height: u32`, `mempool_spend: bool`, `issued_at: u64`, `expires_at: u64`, `attestor_pubkey: String`, `signature: String`
  - `pub enum AttestationError`
  - `impl FundingAttestation { pub fn signing_bytes(&self) -> Vec<u8>; pub fn sign(&mut self, key: &secp256k1::SecretKey) -> Result<(), AttestationError>; pub fn verify_signature(&self, pinned_pubkey_hex: &str) -> Result<(), AttestationError>; pub fn parse(json: &[u8]) -> Result<Self, AttestationError> }`

- [ ] **Step 1: Write the failing tests**

Fixtures used by every test below: `sample()` returns `FundingAttestation { version: 1, network: "vtorrent-regtest".into(), txid: "11".repeat(32), vout: 0, value_satoshis: 100_000_000, script_pubkey: "22".repeat(8), block_hash: "33".repeat(32), block_height: 100, confirmations: 6, tip_hash: "44".repeat(32), tip_height: 105, mempool_spend: false, issued_at: 1_000, expires_at: 4_600, attestor_pubkey: String::new(), signature: String::new() }`. `pubkey_hex(&SecretKey) -> String` returns the compressed hex public key. `mutations()` returns one closure per signed field, each changing exactly that field.

```rust
#[test]
fn round_trip_signs_and_verifies() {
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let mut attestation = sample();
    attestation.sign(&key).unwrap();
    attestation.verify_signature(&pubkey_hex(&key)).unwrap();
}

#[test]
fn tampering_with_any_signed_field_fails_verification() {
    // for each field: txid, vout, value, script, block_hash, block_height,
    // confirmations, tip_hash, tip_height, mempool_spend, issued_at,
    // expires_at, network, version
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let pinned = pubkey_hex(&key);
    for mutate in mutations() {
        let mut attestation = sample();
        attestation.sign(&key).unwrap();
        mutate(&mut attestation);
        assert!(attestation.verify_signature(&pinned).is_err());
    }
}

#[test]
fn a_different_pinned_key_is_rejected() {
    let signer = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let other = secp256k1::SecretKey::from_slice(&[9u8; 32]).unwrap();
    let mut attestation = sample();
    attestation.sign(&signer).unwrap();
    assert_eq!(
        attestation.verify_signature(&pubkey_hex(&other)),
        Err(AttestationError::WrongKey)
    );
}

#[test]
fn unknown_or_missing_json_fields_are_rejected() {
    assert!(FundingAttestation::parse(br#"{"version":1,"extra":true}"#).is_err());
}

#[test]
fn a_signature_with_trailing_bytes_is_rejected() {
    let key = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let mut attestation = sample();
    attestation.sign(&key).unwrap();
    attestation.signature.push_str("00");
    assert_eq!(
        attestation.verify_signature(&pubkey_hex(&key)),
        Err(AttestationError::BadSignature)
    );
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p vtorrent-core --lib attestation::`
Expected: FAIL because the module and type do not exist.

- [ ] **Step 3: Implement the module**

Encode in this order, each preceded by its length as `u32` little-endian where it is a byte string: the domain tag, `network`, `version` as one byte, `txid` bytes, `vout` as `u32` little-endian, `value_satoshis` as `u64` little-endian, `script_pubkey` bytes, `block_hash` bytes, `block_height` as `u32`, `confirmations` as `u32`, `tip_hash` bytes, `tip_height` as `u32`, `mempool_spend` as one byte, `issued_at` as `u64`, `expires_at` as `u64`.

Hex fields are decoded with `hex::decode` and must decode; a failure returns `AttestationError::Malformed("txid")` and so on. `sign` sets `attestor_pubkey` to the compressed hex public key and `signature` to the DER hex signature over `Sha256(signing_bytes())`. `verify_signature` requires the embedded key to equal the pinned key, parses the DER signature strictly, rejects trailing bytes, and verifies against the digest.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p vtorrent-core --lib attestation::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-core/src/attestation.rs vtorrent-core/src/lib.rs
git commit -m "feat(core): add cold-stake funding attestation encoding and signatures"
```

---

### Task 2: Funding binding and policy checks

**Files:**
- Modify: `vtorrent-core/src/attestation.rs`
- Test: unit tests inside the same file

**Interfaces:**
- Consumes: `FundingAttestation`, `AttestationError`.
- Produces:
  - `pub fn check_funding(&self, txid_hex: &str, vout: u32, value_satoshis: u64, script_pubkey_hex: &str) -> Result<(), AttestationError>`
  - `pub fn check_policy(&self, expected_network: &str, min_confirmations: u32, now: u64) -> Result<(), AttestationError>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn funding_mismatches_name_the_field() {
    let attestation = sample();
    assert_eq!(
        attestation.check_funding(&"11".repeat(32), 0, 100, &"22".repeat(8)),
        Err(AttestationError::Field("txid"))
    );
    assert_eq!(
        attestation.check_funding(&attestation.txid, 1, attestation.value_satoshis, &attestation.script_pubkey),
        Err(AttestationError::Field("vout"))
    );
    assert_eq!(
        attestation.check_funding(&attestation.txid, attestation.vout, attestation.value_satoshis + 1, &attestation.script_pubkey),
        Err(AttestationError::Field("value_satoshis"))
    );
    assert_eq!(
        attestation.check_funding(&attestation.txid, attestation.vout, attestation.value_satoshis, &"33".repeat(8)),
        Err(AttestationError::Field("script_pubkey"))
    );
}

#[test]
fn policy_rejects_mempool_spend_low_confirmations_and_expiry() {
    let mut attestation = sample();
    assert_eq!(
        attestation.check_policy(&attestation.network, 6, attestation.issued_at),
        Ok(())
    );
    attestation.mempool_spend = true;
    assert_eq!(
        attestation.check_policy(&attestation.network, 6, attestation.issued_at),
        Err(AttestationError::MempoolSpend)
    );
    let mut attestation = sample();
    attestation.confirmations = 5;
    assert_eq!(
        attestation.check_policy(&attestation.network, 6, attestation.issued_at),
        Err(AttestationError::TooFewConfirmations { confirmations: 5, required: 6 })
    );
    let mut attestation = sample();
    let after = attestation.expires_at + 1;
    assert_eq!(
        attestation.check_policy(&attestation.network, 6, after),
        Err(AttestationError::Expired { expires_at: attestation.expires_at, now: after })
    );
}

#[test]
fn policy_rejects_a_different_network() {
    let attestation = sample();
    assert_eq!(
        attestation.check_policy("vtorrent-mainnet", 6, attestation.issued_at),
        Err(AttestationError::Network {
            found: attestation.network.clone(),
            expected: "vtorrent-mainnet".into()
        })
    );
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p vtorrent-core --lib attestation::`
Expected: FAIL because the methods do not exist.

- [ ] **Step 3: Implement both checks**

`check_funding` compares `txid`, `vout`, `value_satoshis`, and `script_pubkey` and returns the first mismatch as `Field("<name>")`. `check_policy` compares the network, rejects `mempool_spend`, rejects `confirmations < min_confirmations`, and rejects `now > expires_at`.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p vtorrent-core --lib attestation::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-core/src/attestation.rs
git commit -m "feat(core): bind attestations to funding terms and policy"
```

---

### Task 3: Node-side attestation building

**Files:**
- Modify: `vtorrent-node/src/mempool.rs` (add `spender_of`)
- Create: `vtorrent-rpc/src/cold_stake_attestation.rs`
- Modify: `vtorrent-rpc/src/lib.rs` (add `pub mod cold_stake_attestation;`)
- Test: unit tests in `vtorrent-node/src/mempool.rs` and `vtorrent-rpc/src/cold_stake_attestation.rs`

**Interfaces:**
- Consumes: `vtorrent_core::attestation::{FundingAttestation, ATTESTATION_VERSION}`.
- Produces:
  - `pub fn spender_of(&self, txid: &[u8; 32], vout: u32) -> Option<[u8; 32]>` on `Mempool`
  - `pub enum AttestationRefusal` with variants `UnknownTransaction`, `NotConfirmed`, `MissingOutput`, `SpentInChain`, `MempoolSpend`, `TooFewConfirmations { confirmations: u32, required: u32 }`
  - `pub fn build_attestation(chain: &vtorrent_node::chain::Chain, mempool: &vtorrent_node::mempool::Mempool, txid: [u8; 32], vout: u32, network: &str, now: u64, min_confirmations: u32) -> Result<FundingAttestation, AttestationRefusal>`
  - `pub const DEFAULT_MIN_CONFIRMATIONS: u32 = 6;`
  - `pub const ATTESTATION_TTL_SECONDS: u64 = 3600;`

- [ ] **Step 1: Write the failing tests**

Test `spender_of` returns the txid that spends an input and `None` otherwise. Test `build_attestation` for each refusal and the good case:

```rust
#[test]
fn refuses_an_unknown_transaction() {
    let (chain, mempool) = empty();
    assert_eq!(
        build_attestation(&chain, &mempool, [1; 32], 0, "vtorrent-regtest", 10, 6),
        Err(AttestationRefusal::UnknownTransaction)
    );
}

#[test]
fn signs_a_confirmed_unspent_outpoint() {
    let (chain, mempool, txid, script) = funded_chain();
    let attestation =
        build_attestation(&chain, &mempool, txid, 0, "vtorrent-regtest", 10, 6).unwrap();
    assert_eq!(attestation.version, ATTESTATION_VERSION);
    assert_eq!(attestation.value_satoshis, 100_000_000);
    assert_eq!(attestation.script_pubkey, hex::encode(&script));
    assert_eq!(attestation.tip_height, chain.best_height());
    assert!(!attestation.mempool_spend);
    assert_eq!(attestation.expires_at, 10 + ATTESTATION_TTL_SECONDS);
}

#[test]
fn refuses_a_spent_outpoint() { /* spend it in a later block, expect SpentInChain */ }

#[test]
fn refuses_a_mempool_spend() { /* admit a conflicting tx, expect MempoolSpend */ }

#[test]
fn refuses_low_confirmations_and_missing_output() {
    /* expect TooFewConfirmations { confirmations, required } and MissingOutput */
}
```

Build the funded chain the way `vtorrent-rpc/src/handlers/swap/tests.rs` does: `Chain::new_regtest()`, then `mint_to_address` for a coinbase output at height 1, and use `chain.get_transaction`/`get_utxo` to confirm the fixture. `empty()` returns that chain with an empty `Mempool::new(1_000)`. `funded_chain()` returns the chain at height 7 with the minted transaction's id and its `script_pubkey`.

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p vtorrent-rpc --lib cold_stake_attestation::`
Expected: FAIL because the module does not exist.

- [ ] **Step 3: Implement**

`spender_of` reads the existing `spent_inputs` map. `build_attestation` resolves the transaction, computes `confirmations = best_height - height + 1`, reads the output's value and script from the transaction, requires `chain.get_utxo` to return it, requires `mempool.spender_of` to be `None`, and enforces the minimum. It fills `block_hash`, `tip_hash` from `chain.best_hash()` (erroring as `UnknownTransaction` if absent), `issued_at = now`, and `expires_at = now + ATTESTATION_TTL_SECONDS`.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p vtorrent-rpc --lib cold_stake_attestation::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-node/src/mempool.rs vtorrent-rpc/src/cold_stake_attestation.rs vtorrent-rpc/src/lib.rs
git commit -m "feat(rpc): build cold-stake funding attestations from chain state"
```

---

### Task 4: Attestor key and HTTP endpoints

**Files:**
- Create: `vtorrent-rpc/src/cold_stake_attestor.rs`
- Modify: `vtorrent-rpc/src/lib.rs`
- Modify: `vtorrent-rpc/src/state.rs` (add `pub cold_stake_attestor_key_path: Option<PathBuf>`)
- Modify: `vtorrent-rpc/src/models.rs`
- Modify: `vtorrent-rpc/src/handlers/btc.rs` or a new `vtorrent-rpc/src/handlers/cold_stake.rs`
- Modify: `vtorrent-rpc/src/server.rs`
- Modify: `vtorrent-daemon/src/main.rs`
- Test: unit tests in `cold_stake_attestor.rs` and handler tests

**Interfaces:**
- Consumes: `build_attestation`, `DEFAULT_MIN_CONFIRMATIONS`.
- Produces:
  - `pub fn load_or_create_attestor_key(path: &std::path::Path) -> std::io::Result<secp256k1::SecretKey>`
  - `pub fn attestor_pubkey_hex(key: &secp256k1::SecretKey) -> String`
  - `pub async fn get_funding_attestation(State(state), Json(req)) -> RpcResult<Json<FundingAttestation>>`
  - `pub async fn get_funding_attestation_pubkey(State(state)) -> RpcResult<Json<Value>>`
  - `AppState.cold_stake_attestor_key_path: Option<PathBuf>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn attestor_key_is_created_private_and_reused() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("cold-stake-attestor.key");
    let first = load_or_create_attestor_key(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let second = load_or_create_attestor_key(&path).unwrap();
    assert_eq!(attestor_pubkey_hex(&first), attestor_pubkey_hex(&second));
}

#[tokio::test]
async fn the_endpoint_refuses_when_no_attestor_path_is_configured() { /* BadRequest naming the path */ }

#[tokio::test]
async fn the_endpoint_signs_a_confirmed_outpoint() {
    /* fixture chain, configured path, assert signature verifies against the pubkey endpoint */
}

#[tokio::test]
async fn the_endpoint_reports_a_refusal_reason() { /* unknown txid -> BadRequest with the reason */ }
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p vtorrent-rpc --lib cold_stake_attestor::`
Expected: FAIL because the module does not exist.

- [ ] **Step 3: Implement**

Key creation writes 64 hex characters to `<path>.tmp` with mode 0600, syncs the file, renames, and syncs the parent directory, matching `UtxoSet::save`. The handler loads the key, calls `build_attestation` with `state.chain` and `state.mempool`, signs, and returns the attestation. Refusals map to `RpcError::BadRequest` naming the outpoint and the refusal. Routes go in the `protected` router so the API key and concurrency limiter apply. The daemon sets `rpc_state.cold_stake_attestor_key_path = Some(data_dir.join("cold-stake-attestor.key"))` and logs the public key at startup.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p vtorrent-rpc --lib cold_stake_attestor::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vtorrent-rpc/src/cold_stake_attestor.rs vtorrent-rpc/src/lib.rs vtorrent-rpc/src/state.rs vtorrent-rpc/src/models.rs vtorrent-rpc/src/handlers vtorrent-rpc/src/server.rs vtorrent-daemon/src/main.rs
git commit -m "feat(rpc): serve signed cold-stake funding attestations"
```

---

### Task 5: Offline signer verification and CLI wiring

**Files:**
- Modify: `vtorrent-cli/src/main.rs`
- Modify: `vtorrent-cli/tests/cold_stake_offline.rs`
- Test: unit tests in `vtorrent-cli/src/main.rs`

**Interfaces:**
- Consumes: `FundingAttestation::{parse, verify_signature, check_funding, check_policy}`.
- Produces: `fn check_attestation(attestation: &FundingAttestation, pinned_pubkey_hex: &str, network: &str, funding_txid: &str, vout: u32, value_satoshis: u64, script_pubkey_hex: &str, min_confirmations: u32, now: u64) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_valid_attestation_passes_every_check() { /* ok */ }

#[test]
fn a_tampered_field_is_rejected_before_signing() { /* value mismatch names value_satoshis */ }

#[test]
fn an_expired_or_low_confirmation_attestation_is_rejected() { /* both errors */ }

#[test]
fn the_summary_shows_height_confirmations_and_tip() {
    let summary = attestation_summary(&attestation);
    assert!(summary.contains("height"));
    assert!(summary.contains("confirmations"));
    assert!(summary.contains("tip"));
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p vtorrent-cli --lib cold_stake_review_tests::`
Expected: FAIL because `check_attestation` does not exist.

- [ ] **Step 3: Implement**

Add `--attestation <file>` and `--attestor-pubkey <hex>` to `ColdStakeCommands::Spend`, plus `--min-confirmations` defaulting to 6. Before the existing `YES` prompt, read and parse the file, run `check_attestation`, print the summary (`block height`, `confirmations`, `block hash`, `tip height`, `issued at`, `expires at`), and abort on any error. Keep every existing fee check.

- [ ] **Step 4: Update the pseudo-terminal test**

Generate a key, build and sign an attestation for the fixture funding transaction, write it to a temporary file, pass both new arguments, and assert the run still produces a valid spend. Add a second run with a mismatched value and assert it fails before printing `raw_tx:`.

- [ ] **Step 5: Run the tests and confirm they pass**

Run: `cargo test -p vtorrent-cli --lib cold_stake_review_tests:: && cargo test -p vtorrent-cli --test cold_stake_offline`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add vtorrent-cli/src/main.rs vtorrent-cli/tests/cold_stake_offline.rs
git commit -m "feat(cli): require a signed funding attestation before cold-stake signing"
```

---

### Task 6: Documentation and checklist

**Files:**
- Modify: `docs/rpc-api.md`
- Modify: `docs/cold-staking-p2cs-design.md`
- Modify: `docs/mainnet-readiness.md`

- [ ] **Step 1: Document both endpoints** in `docs/rpc-api.md`: request and response shapes, refusals, auth, and the fact that the attestation proves what one node saw, not that the node is honest.

- [ ] **Step 2: Update the cold-stake flow** in `docs/cold-staking-p2cs-design.md`: the operator runs the attestation command, pins the public key, and passes both files to `cold-stake spend`. State the remaining limits: clock skew, node trust, and reorgs after signing.

- [ ] **Step 3: Update the checklist item** in `docs/mainnet-readiness.md` to say the offline signer now requires a verified attestation, and list the remaining verification (independent review).

- [ ] **Step 4: Run the documentation check**

Run: `git diff --check`
Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add docs/rpc-api.md docs/cold-staking-p2cs-design.md docs/mainnet-readiness.md
git commit -m "docs: describe cold-stake funding attestations"
```"
```

---

## Final verification

- [ ] `cargo test -p vtorrent-core --lib attestation::`
- [ ] `cargo test -p vtorrent-rpc --lib cold_stake_`
- [ ] `cargo test -p vtorrent-cli`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace --all-features`
