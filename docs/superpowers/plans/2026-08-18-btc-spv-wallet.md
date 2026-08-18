# BTC SPV Wallet Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a built-in Bitcoin SPV wallet (BIP39 mnemonic + BIP84 SegWit addresses + header sync + tx build/sign/broadcast) to the vTorrent client, sharing a single HD seed with the VTR wallet.

**Architecture:** A new `vtorrent-btc` crate owns all Bitcoin logic (keys, headers, merkle, utxo, tx, p2p, wallet facade). The existing `vtorrent-wallet` gains an optional HD layer (`hd: Option<HdAccount>`) holding a BIP39 mnemonic that serves as the shared seed. RPC, Tauri, and UI are wired to expose the BTC wallet.

**Tech Stack:** Rust (edition 2021), `bitcoin` 0.32 (matches existing `secp256k1` 0.29), `bip39` 2.x, `tokio`, `axum`, `tauri` 2, React + TypeScript.

**Spec:** `docs/superpowers/specs/2026-08-18-btc-spv-wallet-design.md`

---

## File Structure

**New crate `vtorrent-btc`:**
- `vtorrent-btc/Cargo.toml` — crate manifest
- `vtorrent-btc/src/lib.rs` — module map
- `vtorrent-btc/src/error.rs` — `BtcError` thiserror enum
- `vtorrent-btc/src/keys.rs` — BIP32/BIP84 derivation + address
- `vtorrent-btc/src/headers.rs` — BTC header-chain store
- `vtorrent-btc/src/merkle.rs` — merkle proof verification
- `vtorrent-btc/src/utxo.rs` — UTXO set tracking
- `vtorrent-btc/src/tx.rs` — tx build/sign/broadcast
- `vtorrent-btc/src/p2p.rs` — minimal Bitcoin P2P client
- `vtorrent-btc/src/wallet.rs` — `BtcWallet` facade

**Modified:**
- `vtorrent-wallet/src/hd.rs` (new) — mnemonic + HdAccount
- `vtorrent-wallet/src/wallet.rs` — add `hd` field + `enable_hd()`
- `vtorrent-wallet/src/lib.rs` — export `hd`
- `vtorrent-wallet/src/error.rs` — add HD error variants
- `Cargo.toml` — add `bitcoin`, `bip39` to workspace deps; add `vtorrent-btc` member
- `vtorrent-rpc/src/state.rs` — add `btc_wallet` field
- `vtorrent-rpc/src/handlers.rs` — BTC endpoints
- `vtorrent-rpc/src/models.rs` — BTC response types
- `vtorrent-rpc/src/server.rs` — BTC routes
- `vtorrent-rpc/src/error.rs` — `From<BtcError>`
- `vtorrent-rpc/Cargo.toml` — add `vtorrent-btc` dep
- `vtorrent-tauri/src/commands.rs` — BTC commands
- `vtorrent-tauri/src/main.rs` — register BTC commands
- `vtorrent-tauri/Cargo.toml` — add `vtorrent-btc` dep
- `vtorrent-ui/src/hooks/useBtc.tsx` (new) — BTC hooks
- `vtorrent-ui/src/pages/BtcWalletPage.tsx` (new) — BTC wallet page
- `vtorrent-ui/src/App.tsx` — add `/btc` route
- `vtorrent-ui/src/components/Layout.tsx` — add nav item
- `vtorrent-daemon/src/main.rs` — spawn BTC sync task
- `vtorrent-daemon/Cargo.toml` — add `vtorrent-btc` dep

---

## Phase 1 — HD layer in `vtorrent-wallet`

### Task 1: Add `bitcoin` and `bip39` to workspace dependencies

**Files:**
- Modify: `Cargo.toml:29-44`

- [ ] **Step 1: Add the dependencies**

In `Cargo.toml`, under `[workspace.dependencies]`, add after the `# Cryptography` block:

```toml
# Bitcoin
bitcoin = { version = "0.32", features = ["rand"] }
bip39 = { version = "2", features = ["rand"] }
```

- [ ] **Step 2: Verify the workspace still resolves**

Run: `cargo build --workspace 2>&1 | tail -5`
Expected: builds successfully (new deps are unused but resolvable).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add bitcoin and bip39 workspace dependencies"
```

### Task 2: Add `hd.rs` with mnemonic and HdAccount types

**Files:**
- Create: `vtorrent-wallet/src/hd.rs`
- Modify: `vtorrent-wallet/src/lib.rs:1-13`
- Modify: `vtorrent-wallet/src/error.rs:3-61`
- Modify: `vtorrent-wallet/Cargo.toml`

- [ ] **Step 1: Add `bitcoin` and `bip39` to vtorrent-wallet deps**

In `vtorrent-wallet/Cargo.toml`, add to `[dependencies]`:

```toml
bitcoin = { workspace = true }
bip39 = { workspace = true }
```

- [ ] **Step 2: Add error variants**

In `vtorrent-wallet/src/error.rs`, add to the `WalletError` enum (before the closing `}`):

```rust
    #[error("HD derivation error: {0}")]
    HdError(String),

    #[error("Mnemonic error: {0}")]
    MnemonicError(String),
```

- [ ] **Step 3: Write the failing test**

Create `vtorrent-wallet/src/hd.rs` with a test module first:

```rust
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A BIP39 mnemonic phrase, zeroized on drop.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Mnemonic {
    words: String,
}

/// HD account metadata stored in the wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdAccount {
    /// The BIP39 mnemonic phrase (space-separated words).
    pub mnemonic: String,
    /// Word count (12 or 24).
    pub word_count: usize,
    /// Unix timestamp when HD was enabled.
    pub created_at: u64,
}

impl Mnemonic {
    /// Generate a new 24-word mnemonic.
    pub fn generate() -> crate::error::Result<Self> {
        use bip39::{Language, Mnemonic as Bip39Mnemonic};
        let m = Bip39Mnemonic::generate_in(Language::English, 24)
            .map_err(|e| crate::error::WalletError::MnemonicError(e.to_string()))?;
        Ok(Self {
            words: m.to_string(),
        })
    }

    /// Parse a mnemonic from a phrase string.
    pub fn from_phrase(phrase: &str) -> crate::error::Result<Self> {
        use bip39::Mnemonic as Bip39Mnemonic;
        Bip39Mnemonic::parse_in_normalized(bip39::Language::English, phrase)
            .map_err(|e| crate::error::WalletError::MnemonicError(e.to_string()))?;
        Ok(Self {
            words: phrase.to_string(),
        })
    }

    /// The mnemonic phrase as a string.
    pub fn phrase(&self) -> &str {
        &self.words
    }

    /// Derive the 64-byte BIP39 seed (empty passphrase).
    pub fn to_seed(&self) -> [u8; 64] {
        use bip39::Mnemonic as Bip39Mnemonic;
        let m = Bip39Mnemonic::parse_in_normalized(bip39::Language::English, &self.words)
            .expect("mnemonic already validated");
        m.to_seed("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_24_words() {
        let m = Mnemonic::generate().unwrap();
        assert_eq!(m.phrase().split_whitespace().count(), 24);
    }

    #[test]
    fn test_seed_is_64_bytes() {
        let m = Mnemonic::generate().unwrap();
        assert_eq!(m.to_seed().len(), 64);
    }

    #[test]
    fn test_roundtrip_phrase() {
        let m = Mnemonic::generate().unwrap();
        let parsed = Mnemonic::from_phrase(m.phrase()).unwrap();
        assert_eq!(parsed.phrase(), m.phrase());
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p vtorrent-wallet hd::`
Expected: 3 tests pass.

- [ ] **Step 5: Export the module**

In `vtorrent-wallet/src/lib.rs`, add `pub mod hd;` after `pub mod error;`:

```rust
pub mod encryption;
pub mod error;
pub mod hd;
pub mod otp;
pub mod tx_builder;
pub mod wallet;
```

- [ ] **Step 6: Run tests and commit**

Run: `cargo test -p vtorrent-wallet 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-wallet/src/hd.rs vtorrent-wallet/src/lib.rs vtorrent-wallet/src/error.rs vtorrent-wallet/Cargo.toml
git commit -m "feat: add BIP39 mnemonic and HD account types to wallet"
```

### Task 3: Wire `hd` field into `WalletData` and add `enable_hd()`

**Files:**
- Modify: `vtorrent-wallet/src/wallet.rs`

- [ ] **Step 1: Add the `hd` field to `WalletData`**

In `vtorrent-wallet/src/wallet.rs`, add the field to `WalletData` (after `otp_config`):

```rust
    pub otp_config: Option<OtpConfig>,
    /// Optional HD account (BIP39 mnemonic) used as the shared seed.
    pub hd: Option<crate::hd::HdAccount>,
    pub created_at: u64,
```

- [ ] **Step 2: Initialize `hd: None` in `create()`**

In `Wallet::create`, in the `WalletData { ... }` literal, add `hd: None,` after `otp_config: None,`.

- [ ] **Step 3: Initialize `hd: None` in `load()` and `from_json_file()`**

Both `load()` and `from_json_file()` construct `Self { data, passphrase }` from deserialized `data`, so no change is needed there — but the `WalletData` deserialization must tolerate a missing `hd` field for old wallet files. Add `#[serde(default)]` to the field:

```rust
    /// Optional HD account (BIP39 mnemonic) used as the shared seed.
    #[serde(default)]
    pub hd: Option<crate::hd::HdAccount>,
```

- [ ] **Step 4: Write the failing test for `enable_hd`**

Add to the `mod tests` block in `wallet.rs`:

```rust
    #[test]
    fn test_enable_hd() {
        let mut wallet = Wallet::create("test-pass").expect("Create failed");
        assert!(wallet.data.hd.is_none());

        let mnemonic = wallet.enable_hd().expect("enable_hd failed");
        assert_eq!(mnemonic.split_whitespace().count(), 24);
        assert!(wallet.data.hd.is_some());
        assert!(wallet.has_hd());
    }
```

- [ ] **Step 5: Run the test to verify it fails**

Run: `cargo test -p vtorrent-wallet test_enable_hd`
Expected: FAIL (compile error — `enable_hd` and `has_hd` not defined).

- [ ] **Step 6: Implement `enable_hd` and `has_hd`**

Add to `impl Wallet` (after the 2FA section, before `// ─── Serialization helpers`):

```rust
    // ─── HD / seed ────────────────────────────────────────────────────────────

    /// Enable HD on this wallet by generating a BIP39 mnemonic.
    /// Returns the mnemonic phrase so the caller can display it for backup.
    pub fn enable_hd(&mut self) -> Result<String> {
        if self.data.hd.is_some() {
            return Ok(self.data.hd.as_ref().unwrap().mnemonic.clone());
        }
        let mnemonic = crate::hd::Mnemonic::generate()?;
        let phrase = mnemonic.phrase().to_string();
        self.data.hd = Some(crate::hd::HdAccount {
            mnemonic: phrase.clone(),
            word_count: 24,
            created_at: unix_now(),
        });
        self.data.last_modified = unix_now();
        Ok(phrase)
    }

    /// Whether this wallet has an HD account (mnemonic) set.
    pub fn has_hd(&self) -> bool {
        self.data.hd.is_some()
    }

    /// Get the mnemonic phrase, if HD is enabled.
    pub fn mnemonic(&self) -> Option<&str> {
        self.data.hd.as_ref().map(|h| h.mnemonic.as_str())
    }
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p vtorrent-wallet test_enable_hd`
Expected: PASS.

- [ ] **Step 8: Run full wallet tests and commit**

Run: `cargo test -p vtorrent-wallet 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-wallet/src/wallet.rs
git commit -m "feat: add optional HD account to wallet with enable_hd"
```

---

## Phase 2 — `vtorrent-btc` crate: keys

### Task 4: Create the `vtorrent-btc` crate skeleton

**Files:**
- Create: `vtorrent-btc/Cargo.toml`
- Create: `vtorrent-btc/src/lib.rs`
- Create: `vtorrent-btc/src/error.rs`
- Modify: `Cargo.toml:3-20`

- [ ] **Step 1: Add the crate to the workspace members**

In `Cargo.toml`, add `"vtorrent-btc",` to the `members` array (after `"vtorrent-store",`).

- [ ] **Step 2: Write the crate manifest**

Create `vtorrent-btc/Cargo.toml`:

```toml
[package]
name = "vtorrent-btc"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "Bitcoin SPV wallet for vTorrent: BIP84 keys, header sync, and transaction building"

[dependencies]
vtorrent-wallet = { path = "../vtorrent-wallet" }

bitcoin = { workspace = true }
bip39 = { workspace = true }
secp256k1 = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 3: Write the error module**

Create `vtorrent-btc/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BtcError {
    #[error("Bitcoin error: {0}")]
    Bitcoin(String),

    #[error("Key derivation error: {0}")]
    KeyDerivation(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Insufficient funds: available {available} sats, required {required} sats")]
    InsufficientFunds { available: u64, required: u64 },

    #[error("Not synced")]
    NotSynced,

    #[error("P2P error: {0}")]
    P2p(String),

    #[error("Wallet error: {0}")]
    Wallet(#[from] vtorrent_wallet::error::WalletError),
}

pub type Result<T> = std::result::Result<T, BtcError>;
```

- [ ] **Step 4: Write the module map**

Create `vtorrent-btc/src/lib.rs`:

```rust
//! Bitcoin SPV wallet for vTorrent.
//!
//! Provides BIP84 native SegWit key derivation, a header-chain store,
//! merkle-proof verification, UTXO tracking, transaction building/signing,
//! and a minimal Bitcoin P2P client.

pub mod error;
```

- [ ] **Step 5: Build to verify the skeleton compiles**

Run: `cargo build -p vtorrent-btc 2>&1 | tail -5`
Expected: builds successfully (only the `error` module is declared so far).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml vtorrent-btc/
git commit -m "feat: scaffold vtorrent-btc crate"
```

### Task 5: Implement BIP84 key derivation and address

**Files:**
- Create: `vtorrent-btc/src/keys.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/keys.rs`:

```rust
//! BIP32/BIP84 key derivation and native SegWit address generation.

use crate::error::{BtcError, Result};
use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, Network, PublicKey};
use std::str::FromStr;

/// Derive the BIP84 native SegWit address for the given account/change/index.
pub fn derive_address(seed: &[u8; 64], index: u32) -> Result<String> {
    let secp = Secp256k1::new();
    let xpriv = Xpriv::new_master(Network::Bitcoin, seed)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let path = DerivationPath::from_str(&format!("m/84'/0'/0'/0/{}", index))
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let derived = xpriv
        .derive_priv(&secp, &path)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let pubkey = PublicKey::new(derived.private_key.public_key(&secp));
    let address = Address::p2wpkh(&pubkey, Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;
    Ok(address.to_string())
}

/// Derive the private key (WIF) for the given index.
pub fn derive_wif(seed: &[u8; 64], index: u32) -> Result<String> {
    let secp = Secp256k1::new();
    let xpriv = Xpriv::new_master(Network::Bitcoin, seed)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let path = DerivationPath::from_str(&format!("m/84'/0'/0'/0/{}", index))
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let derived = xpriv
        .derive_priv(&secp, &path)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    Ok(derived.private_key.to_wif())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_seed() -> [u8; 64] {
        [7u8; 64]
    }

    #[test]
    fn test_derive_address_is_bech32() {
        let addr = derive_address(&test_seed(), 0).unwrap();
        assert!(addr.starts_with("bc1q"), "got {}", addr);
    }

    #[test]
    fn test_derive_address_deterministic() {
        let a = derive_address(&test_seed(), 3).unwrap();
        let b = derive_address(&test_seed(), 3).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_derive_address_distinct_indices() {
        let a = derive_address(&test_seed(), 0).unwrap();
        let b = derive_address(&test_seed(), 1).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn test_derive_wif_roundtrip() {
        let wif = derive_wif(&test_seed(), 0).unwrap();
        let key = bitcoin::PrivateKey::from_wif(&wif).unwrap();
        assert!(key.compressed);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc keys::`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add vtorrent-btc/src/keys.rs
git commit -m "feat: add BIP84 key derivation and SegWit address to vtorrent-btc"
```

---

## Phase 3 — `vtorrent-btc`: headers and merkle

### Task 6: Implement the BTC header-chain store

**Files:**
- Create: `vtorrent-btc/src/headers.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/headers.rs`:

```rust
//! Bitcoin block-header chain store for SPV.

use crate::error::{BtcError, Result};
use bitcoin::blockdata::block::Header;
use bitcoin::consensus::encode::deserialize;
use bitcoin::hashes::Hash;
use std::collections::HashMap;

/// A stored header with its height.
#[derive(Debug, Clone)]
pub struct StoredHeader {
    pub header: Header,
    pub height: u32,
}

/// A lightweight Bitcoin header chain.
#[derive(Debug, Default)]
pub struct HeaderChain {
    headers: HashMap<[u8; 32], StoredHeader>,
    best_hash: Option<[u8; 32]>,
    best_height: u32,
}

impl HeaderChain {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a header (raw 80-byte serialization) at the given height.
    pub fn add_header(&mut self, raw: &[u8], height: u32) -> Result<()> {
        let header: Header = deserialize(raw)
            .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        let hash: [u8; 32] = header.block_hash().to_byte_array();

        if self.headers.contains_key(&hash) {
            return Ok(());
        }

        if height > 0 {
            let prev: [u8; 32] = header.prev_blockhash.to_byte_array();
            if !self.headers.contains_key(&prev) {
                return Err(BtcError::Bitcoin(format!(
                    "unknown parent {}",
                    hex::encode(prev)
                )));
            }
        }

        self.headers.insert(hash, StoredHeader { header, height });
        if height >= self.best_height || self.best_hash.is_none() {
            self.best_height = height;
            self.best_hash = Some(hash);
        }
        Ok(())
    }

    pub fn best_height(&self) -> u32 {
        self.best_height
    }

    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.best_hash
    }

    pub fn len(&self) -> usize {
        self.headers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    pub fn get(&self, hash: &[u8; 32]) -> Option<&StoredHeader> {
        self.headers.get(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::blockdata::block::Header;
    use bitcoin::consensus::encode::serialize;

    fn make_header(prev: [u8; 32], nonce: u32) -> Header {
        Header {
            version: bitcoin::blockdata::block::Version::ONE,
            prev_blockhash: bitcoin::BlockHash::from_byte_array(prev),
            merkle_root: bitcoin::TxMerkleNode::all_zeros(),
            time: 1_700_000_000 + nonce,
            bits: bitcoin::CompactTarget::from_consensus(0x1d00ffff),
            nonce,
        }
    }

    #[test]
    fn test_add_genesis() {
        let mut chain = HeaderChain::new();
        let h = make_header([0u8; 32], 0);
        chain.add_header(&serialize(&h), 0).unwrap();
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }

    #[test]
    fn test_chain_of_headers() {
        let mut chain = HeaderChain::new();
        let h0 = make_header([0u8; 32], 0);
        let h0_hash: [u8; 32] = h0.block_hash().to_byte_array();
        chain.add_header(&serialize(&h0), 0).unwrap();

        let h1 = make_header(h0_hash, 1);
        chain.add_header(&serialize(&h1), 1).unwrap();
        assert_eq!(chain.best_height(), 1);
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_unknown_parent_rejected() {
        let mut chain = HeaderChain::new();
        let orphan = make_header([0xffu8; 32], 1);
        assert!(chain.add_header(&serialize(&orphan), 1).is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc headers::`
Expected: 3 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod headers;` after `pub mod keys;`.

```bash
git add vtorrent-btc/src/headers.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add Bitcoin header-chain store to vtorrent-btc"
```

### Task 7: Implement merkle proof verification

**Files:**
- Create: `vtorrent-btc/src/merkle.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/merkle.rs`:

```rust
//! Merkle inclusion proof verification for Bitcoin transactions.

use crate::error::{BtcError, Result};
use bitcoin::hashes::{sha256d, Hash};

fn hash256(data: &[u8]) -> [u8; 32] {
    sha256d::Hash::hash(data).to_byte_array()
}

fn combine(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(left);
    buf[32..].copy_from_slice(right);
    hash256(&buf)
}

/// Verify that `txid` is included in the block with the given merkle root.
///
/// `siblings` is the ordered list of sibling hashes from the leaf up to the
/// root; `index` is the leaf's position in the tree.
pub fn verify_inclusion(
    txid: &[u8; 32],
    merkle_root: &[u8; 32],
    siblings: &[[u8; 32]],
    index: u32,
) -> Result<()> {
    let mut current = *txid;
    let mut idx = index;
    for sibling in siblings {
        current = if idx % 2 == 0 {
            combine(&current, sibling)
        } else {
            combine(sibling, &current)
        };
        idx /= 2;
    }
    if current == *merkle_root {
        Ok(())
    } else {
        Err(BtcError::Bitcoin("merkle proof mismatch".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_leaf() {
        let txid = [1u8; 32];
        assert!(verify_inclusion(&txid, &txid, &[], 0).is_ok());
    }

    #[test]
    fn test_two_leaves() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let root = combine(&a, &b);
        assert!(verify_inclusion(&a, &root, &[b], 0).is_ok());
        assert!(verify_inclusion(&b, &root, &[a], 1).is_ok());
    }

    #[test]
    fn test_wrong_sibling_fails() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let root = combine(&a, &b);
        assert!(verify_inclusion(&a, &root, &[a], 0).is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc merkle::`
Expected: 3 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod merkle;`.

```bash
git add vtorrent-btc/src/merkle.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add merkle proof verification to vtorrent-btc"
```

---

## Phase 4 — `vtorrent-btc`: UTXO and transactions

### Task 8: Implement UTXO set tracking

**Files:**
- Create: `vtorrent-btc/src/utxo.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/utxo.rs`:

```rust
//! UTXO set tracking for the wallet's addresses.

use serde::{Deserialize, Serialize};

/// A spendable output owned by the wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    /// Transaction id (hex).
    pub txid: String,
    /// Output index.
    pub vout: u32,
    /// Value in satoshis.
    pub value: u64,
    /// The address this output pays to.
    pub address: String,
    /// Block height where this output was confirmed (0 = mempool).
    pub height: u32,
}

/// In-memory UTXO set.
#[derive(Debug, Default)]
pub struct UtxoSet {
    utxos: Vec<Utxo>,
}

impl UtxoSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, utxo: Utxo) {
        if !self.utxos.iter().any(|u| u.txid == utxo.txid && u.vout == utxo.vout) {
            self.utxos.push(utxo);
        }
    }

    pub fn remove(&mut self, txid: &str, vout: u32) {
        self.utxos.retain(|u| !(u.txid == txid && u.vout == vout));
    }

    pub fn total(&self) -> u64 {
        self.utxos.iter().map(|u| u.value).sum()
    }

    pub fn list(&self) -> &[Utxo] {
        &self.utxos
    }

    /// Select UTXOs to cover `amount` (plus `fee`), largest-first.
    pub fn select(&self, amount: u64, fee: u64) -> Option<Vec<Utxo>> {
        let mut sorted: Vec<Utxo> = self.utxos.clone();
        sorted.sort_by(|a, b| b.value.cmp(&a.value));
        let mut selected = Vec::new();
        let mut sum = 0u64;
        for u in sorted {
            selected.push(u);
            sum += u.value;
            if sum >= amount + fee {
                return Some(selected);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utxo(txid: &str, vout: u32, value: u64) -> Utxo {
        Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: "bc1qtest".to_string(),
            height: 100,
        }
    }

    #[test]
    fn test_add_and_total() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("b", 0, 2000));
        assert_eq!(set.total(), 3000);
    }

    #[test]
    fn test_dedup() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("a", 0, 1000));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn test_select_covers_amount() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 500));
        set.add(utxo("b", 0, 1000));
        set.add(utxo("c", 0, 2000));
        let selected = set.select(1500, 100).unwrap();
        assert!(selected.iter().map(|u| u.value).sum::<u64>() >= 1600);
    }

    #[test]
    fn test_select_insufficient() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 100));
        assert!(set.select(1000, 0).is_none());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc utxo::`
Expected: 4 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod utxo;`.

```bash
git add vtorrent-btc/src/utxo.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add UTXO set tracking to vtorrent-btc"
```

### Task 9: Implement transaction building and signing

**Files:**
- Create: `vtorrent-btc/src/tx.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/tx.rs`:

```rust
//! Bitcoin transaction building, signing, and serialization.

use crate::error::{BtcError, Result};
use crate::utxo::Utxo;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::transaction::Version;
use bitcoin::{Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use std::str::FromStr;

/// Build and sign a P2WPKH transaction spending `inputs` to `destination`,
/// returning the change to `change_address`.
pub fn build_and_sign(
    inputs: &[Utxo],
    destination: &str,
    amount_sats: u64,
    fee_sats: u64,
    change_address: &str,
    wif: &str,
) -> Result<Vec<u8>> {
    let secp = Secp256k1::new();
    let key = bitcoin::PrivateKey::from_wif(wif)
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    let dest = Address::from_str(destination)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(bitcoin::Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let change = Address::from_str(change_address)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(bitcoin::Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let total_in: u64 = inputs.iter().map(|u| u.value).sum();
    let change_sats = total_in
        .checked_sub(amount_sats)
        .and_then(|v| v.checked_sub(fee_sats))
        .ok_or(BtcError::InsufficientFunds {
            available: total_in,
            required: amount_sats + fee_sats,
        })?;

    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs
            .iter()
            .map(|u| TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_str(&u.txid)
                        .map_err(|e| BtcError::Bitcoin(e.to_string()))?,
                    vout: u.vout,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            })
            .collect(),
        output: vec![
            TxOut {
                value: Amount::from_sat(amount_sats),
                script_pubkey: dest.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(change_sats),
                script_pubkey: change.script_pubkey(),
            },
        ],
    };

    // Sign each input (P2WPKH).
    for (i, u) in inputs.iter().enumerate() {
        let sighash = tx
            .segwit_signature_hash(i, &dest.script_pubkey(), Amount::from_sat(u.value), bitcoin::EcdsaSighashType::All)
            .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        let msg = bitcoin::secp256k1::Message::from_digest_slice(sighash.as_ref())
            .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        let sig = secp.sign_ecdsa(&msg, &key.inner);
        let mut sig_bytes = sig.serialize_der().to_vec();
        sig_bytes.push(bitcoin::EcdsaSighashType::All as u8);
        tx.input[i].witness = Witness::from_slice(&[sig_bytes, key.public_key(&secp).to_bytes()]);
    }

    Ok(serialize(&tx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_sign_serializes() {
        // A real testnet-style key; we only assert the tx serializes and has
        // the expected structure, not that it spends a real UTXO.
        let wif = "cVt4o7BGAig1UXywgGSmARhxM85PzKPvS8BKfRRXwEfiyJYQVurM";
        let inputs = vec![Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 100_000,
            address: "bc1qtest".to_string(),
            height: 100,
        }];
        let raw = build_and_sign(
            &inputs,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            50_000,
            1_000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            wif,
        )
        .unwrap();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_insufficient_funds() {
        let wif = "cVt4o7BGAig1UXywgGSmARhxM85PzKPvS8BKfRRXwEfiyJYQVurM";
        let inputs = vec![Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 10_000,
            address: "bc1qtest".to_string(),
            height: 100,
        }];
        let result = build_and_sign(
            &inputs,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            50_000,
            1_000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            wif,
        );
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc tx::`
Expected: 2 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod tx;`.

```bash
git add vtorrent-btc/src/tx.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add transaction building and signing to vtorrent-btc"
```

---

## Phase 5 — `vtorrent-btc`: P2P client and wallet facade

### Task 10: Implement the minimal Bitcoin P2P client

**Files:**
- Create: `vtorrent-btc/src/p2p.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/p2p.rs`:

```rust
//! Minimal Bitcoin P2P client for header sync and transaction broadcast.

use crate::error::{BtcError, Result};
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::network::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::network::message_network::VersionMessage;
use bitcoin::network::Address as BtcAddress;
use bitcoin::p2p::ServiceFlags;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A single connection to a Bitcoin peer.
pub struct BtcPeer {
    stream: TcpStream,
}

impl BtcPeer {
    /// Connect to a peer and perform the version handshake.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        let version = VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            receiver: BtcAddress::new(addr, ServiceFlags::NONE),
            sender: BtcAddress::new(addr, ServiceFlags::NONE),
            nonce: 0,
            user_agent: "/vtorrent-btc:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };

        let msg = RawNetworkMessage::new(
            bitcoin::Network::Bitcoin.magic(),
            NetworkMessage::Version(version),
        );
        let payload = serialize(&msg);
        stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        // Read the verack (ignore the peer's version for now).
        let mut buf = [0u8; 24];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;

        Ok(Self { stream })
    }

    /// Send a raw network message.
    pub async fn send(&mut self, msg: NetworkMessage) -> Result<()> {
        let raw = RawNetworkMessage::new(bitcoin::Network::Bitcoin.magic(), msg);
        let payload = serialize(&raw);
        self.stream
            .write_all(&payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        Ok(())
    }

    /// Read one raw network message.
    pub async fn recv(&mut self) -> Result<NetworkMessage> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let len = u32::from_le_bytes([header[20], header[21], header[22], header[23]]) as usize;
        let mut payload = vec![0u8; len];
        self.stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        let mut full = header.to_vec();
        full.extend_from_slice(&payload);
        let raw: RawNetworkMessage = deserialize(&full)
            .map_err(|e| BtcError::P2p(e.to_string()))?;
        Ok(raw.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_message_serializes() {
        let addr: SocketAddr = "127.0.0.1:8333".parse().unwrap();
        let version = VersionMessage {
            version: 70016,
            services: ServiceFlags::WITNESS,
            timestamp: 0,
            receiver: BtcAddress::new(addr, ServiceFlags::NONE),
            sender: BtcAddress::new(addr, ServiceFlags::NONE),
            nonce: 0,
            user_agent: "/vtorrent-btc:0.1.0/".to_string(),
            start_height: 0,
            relay: true,
        };
        let msg = RawNetworkMessage::new(
            bitcoin::Network::Bitcoin.magic(),
            NetworkMessage::Version(version),
        );
        let bytes = serialize(&msg);
        assert!(!bytes.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc p2p::`
Expected: 1 test passes.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod p2p;`.

```bash
git add vtorrent-btc/src/p2p.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add minimal Bitcoin P2P client to vtorrent-btc"
```

### Task 11: Implement the `BtcWallet` facade

**Files:**
- Create: `vtorrent-btc/src/wallet.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/wallet.rs`:

```rust
//! Top-level Bitcoin wallet facade.

use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::keys::derive_address;
use crate::utxo::{Utxo, UtxoSet};
use std::sync::{Arc, Mutex};

/// A Bitcoin SPV wallet.
pub struct BtcWallet {
    seed: [u8; 64],
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    next_index: u32,
}

impl BtcWallet {
    /// Create a wallet from a 64-byte BIP39 seed.
    pub fn new(seed: [u8; 64]) -> Self {
        Self {
            seed,
            headers: Arc::new(Mutex::new(HeaderChain::new())),
            utxos: Arc::new(Mutex::new(UtxoSet::new())),
            next_index: 0,
        }
    }

    /// Derive the next unused receiving address.
    pub fn next_address(&mut self) -> Result<String> {
        let addr = derive_address(&self.seed, self.next_index)?;
        self.next_index += 1;
        Ok(addr)
    }

    /// The current receiving address (without advancing).
    pub fn current_address(&self) -> Result<String> {
        derive_address(&self.seed, self.next_index)
    }

    /// Total confirmed balance in satoshis.
    pub fn balance(&self) -> u64 {
        self.utxos.lock().unwrap().total()
    }

    /// List all UTXOs.
    pub fn list_utxos(&self) -> Vec<Utxo> {
        self.utxos.lock().unwrap().list().to_vec()
    }

    /// Best known header height.
    pub fn best_height(&self) -> u32 {
        self.headers.lock().unwrap().best_height()
    }

    /// Add a header to the chain.
    pub fn add_header(&self, raw: &[u8], height: u32) -> Result<()> {
        self.headers.lock().unwrap().add_header(raw, height)
    }

    /// Add a UTXO.
    pub fn add_utxo(&self, utxo: Utxo) {
        self.utxos.lock().unwrap().add(utxo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_address_advances() {
        let mut w = BtcWallet::new([7u8; 64]);
        let a = w.next_address().unwrap();
        let b = w.next_address().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("bc1q"));
    }

    #[test]
    fn test_balance_tracks_utxos() {
        let w = BtcWallet::new([7u8; 64]);
        w.add_utxo(Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 5000,
            address: "bc1qtest".to_string(),
            height: 1,
        });
        assert_eq!(w.balance(), 5000);
    }

    #[test]
    fn test_best_height_default_zero() {
        let w = BtcWallet::new([7u8; 64]);
        assert_eq!(w.best_height(), 0);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc wallet::`
Expected: 3 tests pass.

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod wallet;`.

```bash
git add vtorrent-btc/src/wallet.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add BtcWallet facade to vtorrent-btc"
```

---

## Phase 6 — Integration

### Task 12: Add BTC RPC endpoints

**Files:**
- Modify: `vtorrent-rpc/Cargo.toml`
- Modify: `vtorrent-rpc/src/state.rs`
- Modify: `vtorrent-rpc/src/models.rs`
- Modify: `vtorrent-rpc/src/handlers.rs`
- Modify: `vtorrent-rpc/src/server.rs`
- Modify: `vtorrent-rpc/src/error.rs`

- [ ] **Step 1: Add the dependency**

In `vtorrent-rpc/Cargo.toml`, add to `[dependencies]`:

```toml
vtorrent-btc = { path = "../vtorrent-btc" }
```

- [ ] **Step 2: Add `btc_wallet` to `AppState`**

In `vtorrent-rpc/src/state.rs`, add the import and field. Add to imports:

```rust
use vtorrent_btc::wallet::BtcWallet;
```

Add the field to `AppState` (after `spv_chain`):

```rust
    /// Bitcoin SPV wallet (optional — created when a seed is available).
    pub btc_wallet: Arc<RwLock<Option<BtcWallet>>>,
```

Initialize it in both constructors (`new_with_shared` and `new`) with:

```rust
            btc_wallet: Arc::new(RwLock::new(None)),
```

- [ ] **Step 3: Add response models**

In `vtorrent-rpc/src/models.rs`, append:

```rust
// ─── Bitcoin wallet ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcStatusResponse {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
    pub synced: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcSendRequest {
    pub to_address: String,
    pub amount_satoshis: u64,
    pub fee_satoshis: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BtcSendResponse {
    pub txid: String,
    pub raw_tx: String,
}
```

- [ ] **Step 4: Add the handlers**

In `vtorrent-rpc/src/handlers.rs`, append:

```rust
// ─── Bitcoin wallet ────────────────────────────────────────────────────────────

pub async fn get_btc_status(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<BtcStatusResponse>> {
    let btc = state.btc_wallet.read().await;
    match &*btc {
        None => Ok(Json(BtcStatusResponse {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
            synced: false,
        })),
        Some(w) => Ok(Json(BtcStatusResponse {
            initialized: true,
            balance_satoshis: w.balance(),
            address: w.current_address().ok(),
            best_height: w.best_height(),
            synced: w.best_height() > 0,
        })),
    }
}

pub async fn get_btc_address(
    State(state): State<Arc<AppState>>,
) -> RpcResult<Json<Value>> {
    let mut btc = state.btc_wallet.write().await;
    match &mut *btc {
        None => Err(RpcError::BadRequest("BTC wallet not initialized".into())),
        Some(w) => Ok(Json(json!({ "address": w.next_address().map_err(|e| RpcError::Internal(e.to_string()))? }))),
    }
}

pub async fn send_btc(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BtcSendRequest>,
) -> RpcResult<Json<BtcSendResponse>> {
    let btc = state.btc_wallet.read().await;
    let w = btc.as_ref().ok_or_else(|| RpcError::BadRequest("BTC wallet not initialized".into()))?;
    let fee = req.fee_satoshis.unwrap_or(1_000);
    let utxos = w.list_utxos();
    let selected = crate::utxo_select(&utxos, req.amount_satoshis, fee)
        .ok_or_else(|| RpcError::BadRequest("Insufficient BTC funds".into()))?;
    let change = w.current_address().map_err(|e| RpcError::Internal(e.to_string()))?;
    let wif = w.derive_wif(0).map_err(|e| RpcError::Internal(e.to_string()))?;
    let raw = vtorrent_btc::tx::build_and_sign(
        &selected,
        &req.to_address,
        req.amount_satoshis,
        fee,
        &change,
        &wif,
    )
    .map_err(|e| RpcError::BadRequest(e.to_string()))?;
    let txid = hex::encode(vtorrent_btc::tx::txid_of(&raw));
    Ok(Json(BtcSendResponse {
        txid,
        raw_tx: hex::encode(raw),
    }))
}
```

- [ ] **Step 5: Add the helper functions to `vtorrent-btc`**

The handlers reference `crate::utxo_select` and `vtorrent_btc::tx::txid_of` and `BtcWallet::derive_wif`. Add these:

In `vtorrent-btc/src/tx.rs`, add:

```rust
/// Compute the txid (double-SHA256 of the serialized tx) as hex.
pub fn txid_of(raw: &[u8]) -> [u8; 32] {
    use bitcoin::hashes::{sha256d, Hash};
    sha256d::Hash::hash(raw).to_byte_array()
}
```

In `vtorrent-btc/src/wallet.rs`, add a `derive_wif` method:

```rust
    /// Derive the WIF private key for the given index.
    pub fn derive_wif(&self, index: u32) -> Result<String> {
        crate::keys::derive_wif(&self.seed, index)
    }
```

In `vtorrent-rpc/src/handlers.rs`, add a local helper (top of file, near `parse_hash32`):

```rust
fn utxo_select(
    utxos: &[vtorrent_btc::utxo::Utxo],
    amount: u64,
    fee: u64,
) -> Option<Vec<vtorrent_btc::utxo::Utxo>> {
    let mut sorted: Vec<vtorrent_btc::utxo::Utxo> = utxos.to_vec();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let mut selected = Vec::new();
    let mut sum = 0u64;
    for u in sorted {
        selected.push(u);
        sum += u.value;
        if sum >= amount + fee {
            return Some(selected);
        }
    }
    None
}
```

- [ ] **Step 6: Add the routes**

In `vtorrent-rpc/src/server.rs`, add to the read-only router (after the DEX route):

```rust
        // Bitcoin wallet
        .route("/api/v1/btc/status", get(get_btc_status))
        .route("/api/v1/btc/address", get(get_btc_address))
```

And to the `protected` router (after the spv headers route):

```rust
        .route("/api/v1/btc/send", post(send_btc))
```

- [ ] **Step 7: Add the error conversion**

In `vtorrent-rpc/src/error.rs`, add:

```rust
impl From<vtorrent_btc::error::BtcError> for RpcError {
    fn from(e: vtorrent_btc::error::BtcError) -> Self {
        RpcError::Internal(e.to_string())
    }
}
```

- [ ] **Step 8: Write the failing test**

In `vtorrent-rpc/src/server.rs`, add to `mod tests`:

```rust
    #[tokio::test]
    async fn test_btc_status_uninitialized() {
        let app = build_router(AppState::new());
        let (status, body) = get(app, "/api/v1/btc/status").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["initialized"], false);
        assert_eq!(body["balance_satoshis"], 0);
    }
```

- [ ] **Step 9: Run the test to verify it passes**

Run: `cargo test -p vtorrent-rpc test_btc_status_uninitialized`
Expected: PASS.

- [ ] **Step 10: Run full RPC tests and commit**

Run: `cargo test -p vtorrent-rpc 2>&1 | rg "test result"`
Expected: all pass.

```bash
git add vtorrent-rpc/ vtorrent-btc/src/tx.rs vtorrent-btc/src/wallet.rs
git commit -m "feat: add BTC wallet RPC endpoints"
```

### Task 13: Add Tauri BTC commands

**Files:**
- Modify: `vtorrent-tauri/Cargo.toml`
- Modify: `vtorrent-tauri/src/commands.rs`
- Modify: `vtorrent-tauri/src/main.rs`

- [ ] **Step 1: Add the dependency**

In `vtorrent-tauri/Cargo.toml`, add to `[dependencies]`:

```toml
vtorrent-btc = { path = "../vtorrent-btc" }
```

- [ ] **Step 2: Add the commands**

In `vtorrent-tauri/src/commands.rs`, append:

```rust
// ─── Bitcoin wallet commands ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct BtcStatus {
    pub initialized: bool,
    pub balance_satoshis: u64,
    pub address: Option<String>,
    pub best_height: u32,
}

/// Get the BTC wallet status.
#[tauri::command]
pub fn get_btc_status(state: State<AppState>) -> Result<BtcStatus> {
    let wallet = state.wallet.lock().map_err(|_| TauriError::WalletLocked)?;
    let wallet = wallet.as_ref().ok_or(TauriError::WalletNotInitialized)?;
    if !wallet.has_hd() {
        return Ok(BtcStatus {
            initialized: false,
            balance_satoshis: 0,
            address: None,
            best_height: 0,
        });
    }
    let mnemonic = wallet.mnemonic().ok_or(TauriError::WalletNotInitialized)?;
    let seed = vtorrent_wallet::hd::Mnemonic::from_phrase(mnemonic)
        .map_err(TauriError::from)?
        .to_seed();
    let mut btc = vtorrent_btc::wallet::BtcWallet::new(seed);
    let address = btc.next_address().map_err(|e| TauriError::Wallet(e.to_string()))?;
    Ok(BtcStatus {
        initialized: true,
        balance_satoshis: btc.balance(),
        address: Some(address),
        best_height: btc.best_height(),
    })
}
```

- [ ] **Step 3: Register the command**

In `vtorrent-tauri/src/main.rs`, add to the `generate_handler!` list (after `commands::get_staking_status,`):

```rust
            commands::get_btc_status,
```

- [ ] **Step 4: Build and commit**

Run: `cargo build -p vtorrent-tauri 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-tauri/
git commit -m "feat: add BTC wallet Tauri command"
```

### Task 14: Add the BTC wallet UI page

**Files:**
- Create: `vtorrent-ui/src/hooks/useBtc.tsx`
- Create: `vtorrent-ui/src/pages/BtcWalletPage.tsx`
- Modify: `vtorrent-ui/src/App.tsx`
- Modify: `vtorrent-ui/src/components/Layout.tsx`

- [ ] **Step 1: Write the hook**

Create `vtorrent-ui/src/hooks/useBtc.tsx`:

```tsx
import { useState, useEffect, useCallback } from 'react'

const RPC_BASE = 'http://127.0.0.1:22525'

export interface BtcStatus {
  initialized: boolean
  balanceSatoshis: number
  address: string | null
  bestHeight: number
  synced: boolean
}

function camel<T>(obj: unknown): T {
  if (Array.isArray(obj)) return obj.map(camel) as unknown as T
  if (obj && typeof obj === 'object') {
    const out: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
      const key = k.replace(/_([a-z])/g, (_, c) => c.toUpperCase())
      out[key] = camel(v)
    }
    return out as T
  }
  return obj as T
}

export function useBtcStatus(intervalMs = 10_000) {
  const [status, setStatus] = useState<BtcStatus | null>(null)

  useEffect(() => {
    let active = true
    const fetchStatus = async () => {
      try {
        const res = await fetch(`${RPC_BASE}/api/v1/btc/status`)
        if (!res.ok) return
        const data = await res.json()
        if (active) setStatus(camel<BtcStatus>(data))
      } catch {
        /* ignore */
      }
    }
    fetchStatus()
    const id = setInterval(fetchStatus, intervalMs)
    return () => {
      active = false
      clearInterval(id)
    }
  }, [intervalMs])

  return status
}

export function useBtcAddress() {
  const [address, setAddress] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const generate = useCallback(async () => {
    setLoading(true)
    try {
      const res = await fetch(`${RPC_BASE}/api/v1/btc/address`)
      if (!res.ok) return
      const data = await res.json()
      setAddress(data.address)
    } finally {
      setLoading(false)
    }
  }, [])

  return { address, generate, loading }
}
```

- [ ] **Step 2: Write the page**

Create `vtorrent-ui/src/pages/BtcWalletPage.tsx`:

```tsx
import { useState } from 'react'
import { Bitcoin, Copy, RefreshCw, Send } from 'lucide-react'
import { useBtcStatus, useBtcAddress } from '../hooks/useBtc'

export default function BtcWalletPage() {
  const status = useBtcStatus()
  const { address, generate, loading } = useBtcAddress()
  const [toAddress, setToAddress] = useState('')
  const [amount, setAmount] = useState('')
  const [sent, setSent] = useState<string | null>(null)

  const send = async () => {
    const res = await fetch('http://127.0.0.1:22525/api/v1/btc/send', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ to_address: toAddress, amount_satoshis: Number(amount) }),
    })
    if (res.ok) {
      const data = await res.json()
      setSent(data.txid)
    }
  }

  return (
    <div className="p-6 space-y-6">
      <h1 className="text-2xl font-bold flex items-center gap-2">
        <Bitcoin className="text-amber-400" /> Bitcoin Wallet
      </h1>

      <div className="grid grid-cols-3 gap-4">
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Balance</p>
          <p className="text-2xl font-bold">
            {(status?.balanceSatoshis ?? 0) / 100_000_000} BTC
          </p>
        </div>
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Sync Height</p>
          <p className="text-2xl font-bold">{status?.bestHeight ?? 0}</p>
        </div>
        <div className="bg-navy-800 rounded-lg p-4">
          <p className="text-sm text-gray-400">Status</p>
          <p className="text-2xl font-bold">
            {status?.initialized ? (status.synced ? 'Synced' : 'Syncing') : 'Not set up'}
          </p>
        </div>
      </div>

      <div className="bg-navy-800 rounded-lg p-4 space-y-3">
        <h2 className="font-semibold">Receive</h2>
        <div className="flex items-center gap-2">
          <code className="flex-1 bg-navy-900 p-2 rounded text-sm break-all">
            {address ?? 'Generate an address'}
          </code>
          <button
            onClick={generate}
            disabled={loading}
            className="p-2 bg-vtorrent-600 rounded hover:bg-vtorrent-500 disabled:opacity-50"
          >
            <RefreshCw size={16} />
          </button>
        </div>
      </div>

      <div className="bg-navy-800 rounded-lg p-4 space-y-3">
        <h2 className="font-semibold">Send</h2>
        <input
          value={toAddress}
          onChange={(e) => setToAddress(e.target.value)}
          placeholder="bc1q destination"
          className="w-full bg-navy-900 p-2 rounded text-sm"
        />
        <input
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
          placeholder="amount in satoshis"
          className="w-full bg-navy-900 p-2 rounded text-sm"
        />
        <button
          onClick={send}
          className="flex items-center gap-2 px-4 py-2 bg-vtorrent-600 rounded hover:bg-vtorrent-500"
        >
          <Send size={16} /> Send
        </button>
        {sent && <p className="text-sm text-green-400">Sent! TXID: {sent}</p>}
      </div>
    </div>
  )
}
```

- [ ] **Step 3: Add the route**

In `vtorrent-ui/src/App.tsx`, add the import and route. Add to imports:

```tsx
import BtcWalletPage from './pages/BtcWalletPage'
```

Add inside the `<Route element={<Layout />}>` block (after the `/claim` route):

```tsx
        <Route
          path="/btc"
          element={<BtcWalletPage />}
        />
```

- [ ] **Step 4: Add the nav item**

In `vtorrent-ui/src/components/Layout.tsx`, add `Bitcoin` to the lucide import and a nav item. Change the import line:

```tsx
import {
  LayoutDashboard, Shield, Download, ArrowLeftRight,
  Lock, Wifi, WifiOff, RefreshCw, Cpu, Zap, Gift, Bitcoin,
} from 'lucide-react'
```

Add to `navItems` (after the `/claim` entry):

```tsx
  { to: '/btc',       icon: Bitcoin,        label: 'Bitcoin'  },
```

- [ ] **Step 5: Lint, typecheck, and commit**

Run: `cd vtorrent-ui && pnpm lint && npx tsc --noEmit`
Expected: both pass.

```bash
git add vtorrent-ui/
git commit -m "feat: add Bitcoin wallet UI page"
```

### Task 15: Spawn the BTC sync task in the daemon

**Files:**
- Modify: `vtorrent-daemon/Cargo.toml`
- Modify: `vtorrent-daemon/src/main.rs`

> **Scope note:** This plan delivers the full `vtorrent-btc` crate (keys, header store, merkle, UTXO, tx, P2P primitives, wallet facade) and wires it through RPC/Tauri/UI. The **live header-sync loop** — connecting to real BTC peers via DNS seeds, running `getheaders` from genesis, and scanning blocks to populate the UTXO set — is deferred to the cross-chain swap sub-project, which needs the same peer-discovery infrastructure. The P2P primitives (`BtcPeer::connect/send/recv`) and `HeaderChain`/`UtxoSet` are in place so that loop is a thin orchestration layer on top.

- [ ] **Step 1: Add the dependency**

In `vtorrent-daemon/Cargo.toml`, add to `[dependencies]`:

```toml
vtorrent-btc = { path = "../vtorrent-btc" }
```

- [ ] **Step 2: Spawn a placeholder sync task**

In `vtorrent-daemon/src/main.rs`, after the DEX maintenance task (around line 437), add:

```rust
    // Bitcoin SPV sync — placeholder that logs readiness. The full header
    // sync loop is wired in a later sub-project (cross-chain swap).
    let btc_wallet = Arc::clone(&rpc_state.btc_wallet);
    tokio::spawn(async move {
        tracing::info!("Bitcoin SPV wallet task started (idle)");
        let _ = btc_wallet;
    });
```

- [ ] **Step 3: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/
git commit -m "feat: spawn Bitcoin SPV wallet task in daemon"
```

---

## Final Verification

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace --all-features 2>&1 | rg "test result: FAILED|error\["`
Expected: no failures.

- [ ] **Step 2: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | rg "warning:|error:"`
Expected: no output.

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 3: Run the frontend checks**

Run: `cd vtorrent-ui && pnpm lint && npx tsc --noEmit`
Expected: both pass.

- [ ] **Step 4: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of BTC SPV wallet"
```
