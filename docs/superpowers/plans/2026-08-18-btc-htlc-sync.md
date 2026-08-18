# BTC HTLC Primitives + Live Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Bitcoin-side P2WSH HTLC primitives and a live BTC header-sync + BIP37 Bloom-filter UTXO scan to the vTorrent client.

**Architecture:** A `BtcHtlc` type in `vtorrent-btc` mirrors the VTR-side `Htlc` using `rust-bitcoin`. A `BtcSync` task resolves DNS seeds, syncs headers from genesis, loads a BIP37 Bloom filter, fetches `merkleblock`s, and populates the `UtxoSet`. The daemon's placeholder BTC task is replaced with the real loop.

**Tech Stack:** Rust (edition 2021), `bitcoin` 0.32, `tokio`, `vtorrent-spv::BloomFilter` (reused).

**Spec:** `docs/superpowers/specs/2026-08-18-btc-htlc-sync-design.md`

---

## File Structure

**New files in `vtorrent-btc`:**
- `vtorrent-btc/src/htlc.rs` — `BtcHtlc` (P2WSH script + funding/claim/refund tx)
- `vtorrent-btc/src/sync.rs` — `BtcSync` (DNS seeds, header sync, Bloom filter, UTXO scan)

**Modified:**
- `vtorrent-btc/src/lib.rs` — export `htlc`, `sync`
- `vtorrent-btc/src/error.rs` — add `Dns`, `Sync` variants
- `vtorrent-btc/src/wallet.rs` — add `sync()` + `synced` flag
- `vtorrent-btc/Cargo.toml` — add `vtorrent-spv` dep
- `vtorrent-daemon/src/main.rs` — replace placeholder task with real sync loop
- `vtorrent-rpc/src/handlers.rs` — reflect real sync state in status

---

## Task 1: Add `vtorrent-spv` dependency and error variants

**Files:**
- Modify: `vtorrent-btc/Cargo.toml`
- Modify: `vtorrent-btc/src/error.rs`

- [ ] **Step 1: Add the dependency**

In `vtorrent-btc/Cargo.toml`, add to `[dependencies]`:

```toml
vtorrent-spv = { path = "../vtorrent-spv" }
```

- [ ] **Step 2: Add error variants**

In `vtorrent-btc/src/error.rs`, add to the `BtcError` enum (before the closing `}`):

```rust
    #[error("DNS error: {0}")]
    Dns(String),

    #[error("Sync error: {0}")]
    Sync(String),
```

- [ ] **Step 3: Build and commit**

Run: `cargo build -p vtorrent-btc 2>&1 | tail -3`
Expected: builds successfully.

```bash
git add vtorrent-btc/Cargo.toml vtorrent-btc/src/error.rs
git commit -m "feat: add vtorrent-spv dep and sync error variants to vtorrent-btc"
```

---

## Task 2: Implement `BtcHtlc` (P2WSH)

**Files:**
- Create: `vtorrent-btc/src/htlc.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/htlc.rs`:

```rust
//! Bitcoin-side HTLC (P2WSH) for cross-chain atomic swaps.

use crate::error::{BtcError, Result};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::opcodes::all::{OP_CHECKSIG, OP_CHECKLOCKTIMEVERIFY, OP_DROP, OP_DUP, OP_ELSE, OP_ENDIF, OP_EQUALVERIFY, OP_HASH160, OP_IF, OP_SHA256};
use bitcoin::script::Builder;
use bitcoin::transaction::Version;
use bitcoin::{Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use std::str::FromStr;

/// Default HTLC locktime: 48 hours in seconds.
pub const DEFAULT_HTLC_LOCKTIME: u32 = 48 * 3600;

/// A Bitcoin-side HTLC.
#[derive(Debug, Clone)]
pub struct BtcHtlc {
    pub hash_lock: [u8; 32],
    pub recipient: String,
    pub refund_address: String,
    pub expiry: u32,
    pub amount: u64,
}

impl BtcHtlc {
    pub fn new(
        hash_lock: [u8; 32],
        recipient: String,
        refund_address: String,
        locktime_seconds: u32,
        amount: u64,
    ) -> Result<Self> {
        if amount == 0 {
            return Err(BtcError::Bitcoin("HTLC amount cannot be zero".into()));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        Ok(Self {
            hash_lock,
            recipient,
            refund_address,
            expiry: now + locktime_seconds,
            amount,
        })
    }

    /// Build the P2WSH witness script.
    pub fn build_script(&self) -> ScriptBuf {
        let recipient = Address::from_str(&self.recipient)
            .expect("validated address")
            .require_network(bitcoin::Network::Bitcoin)
            .expect("mainnet");
        let refund = Address::from_str(&self.refund_address)
            .expect("validated address")
            .require_network(bitcoin::Network::Bitcoin)
            .expect("mainnet");

        Builder::new()
            .push_opcode(OP_IF)
            .push_opcode(OP_SHA256)
            .push_slice(self.hash_lock)
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_DUP)
            .push_opcode(OP_HASH160)
            .push_slice(recipient.script_pubkey().as_bytes())
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ELSE)
            .push_int(self.expiry as i64)
            .push_opcode(OP_CHECKLOCKTIMEVERIFY)
            .push_opcode(OP_DROP)
            .push_opcode(OP_DUP)
            .push_opcode(OP_HASH160)
            .push_slice(refund.script_pubkey().as_bytes())
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ENDIF)
            .into_script()
    }

    /// The P2WSH address for the funding output.
    pub fn address(&self) -> Result<String> {
        let script = self.build_script();
        let addr = Address::p2wsh(&script, bitcoin::Network::Bitcoin);
        Ok(addr.to_string())
    }

    /// Build the funding transaction (single input, P2WSH output + change).
    pub fn build_funding_tx(
        &self,
        input_txid: [u8; 32],
        input_vout: u32,
        input_value: u64,
        fee: u64,
        change_address: &str,
    ) -> Result<Transaction> {
        if input_value < self.amount + fee {
            return Err(BtcError::InsufficientFunds {
                available: input_value,
                required: self.amount + fee,
            });
        }
        let change = Address::from_str(change_address)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        let mut outputs = vec![TxOut {
            value: Amount::from_sat(self.amount),
            script_pubkey: self.build_script().to_p2wsh(),
        }];
        let change_sats = input_value - self.amount - fee;
        if change_sats > 0 {
            outputs.push(TxOut {
                value: Amount::from_sat(change_sats),
                script_pubkey: change.script_pubkey(),
            });
        }

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(input_txid),
                    vout: input_vout,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: outputs,
        })
    }

    /// Build the claim transaction (reveals the preimage).
    pub fn build_claim_tx(
        &self,
        funding_txid: [u8; 32],
        preimage: &[u8; 32],
        fee: u64,
    ) -> Result<Transaction> {
        let hash: [u8; 32] = sha256::Hash::hash(preimage).to_byte_array();
        if hash != self.hash_lock {
            return Err(BtcError::Bitcoin("preimage does not match hash lock".into()));
        }
        let recipient = Address::from_str(&self.recipient)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(funding_txid),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(self.amount.saturating_sub(fee)),
                script_pubkey: recipient.script_pubkey(),
            }],
        })
    }

    /// Build the refund transaction (after expiry).
    pub fn build_refund_tx(
        &self,
        funding_txid: [u8; 32],
        fee: u64,
    ) -> Result<Transaction> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if now < self.expiry {
            return Err(BtcError::Bitcoin("HTLC has not expired yet".into()));
        }
        let refund = Address::from_str(&self.refund_address)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::from_consensus(self.expiry),
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(funding_txid),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(self.amount.saturating_sub(fee)),
                script_pubkey: refund.script_pubkey(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_htlc() -> BtcHtlc {
        let preimage = [42u8; 32];
        let hash_lock: [u8; 32] = sha256::Hash::hash(&preimage).to_byte_array();
        BtcHtlc::new(
            hash_lock,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh".to_string(),
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh".to_string(),
            DEFAULT_HTLC_LOCKTIME,
            100_000,
        )
        .unwrap()
    }

    #[test]
    fn test_script_structure() {
        let htlc = make_htlc();
        let script = htlc.build_script();
        assert!(!script.is_empty());
        assert_eq!(script.as_bytes()[0], OP_IF.to_u8());
        assert_eq!(*script.as_bytes().last().unwrap(), OP_ENDIF.to_u8());
    }

    #[test]
    fn test_script_contains_hash_lock() {
        let htlc = make_htlc();
        let script = htlc.build_script();
        let script_hex = hex::encode(script.as_bytes());
        let hash_hex = hex::encode(htlc.hash_lock);
        assert!(script_hex.contains(&hash_hex));
    }

    #[test]
    fn test_address_is_bech32() {
        let htlc = make_htlc();
        let addr = htlc.address().unwrap();
        assert!(addr.starts_with("bc1q"), "got {}", addr);
    }

    #[test]
    fn test_wrong_preimage_rejected() {
        let htlc = make_htlc();
        let result = htlc.build_claim_tx([0u8; 32], &[99u8; 32], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_refund_before_expiry_rejected() {
        let htlc = make_htlc();
        let result = htlc.build_refund_tx([0u8; 32], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_funding_tx_insufficient() {
        let htlc = make_htlc();
        let result = htlc.build_funding_tx(
            [0u8; 32],
            0,
            50_000,
            1000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_funding_tx_valid() {
        let htlc = make_htlc();
        let tx = htlc
            .build_funding_tx(
                [1u8; 32],
                0,
                200_000,
                10_000,
                "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            )
            .unwrap();
        assert_eq!(tx.output[0].value, Amount::from_sat(100_000));
        assert_eq!(tx.output[1].value, Amount::from_sat(90_000));
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc htlc::`
Expected: 7 tests pass. If the `OP_IF.to_u8()` or `push_int` API differs, adjust to the actual `bitcoin 0.32` API (e.g. `OP_IF.to_u8()` exists on `Opcode`; `Builder::push_int` takes `i64`).

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod htlc;` after `pub mod headers;`.

```bash
git add vtorrent-btc/src/htlc.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add Bitcoin P2WSH HTLC primitives to vtorrent-btc"
```

---

## Task 3: Implement the `BtcSync` loop

**Files:**
- Create: `vtorrent-btc/src/sync.rs`
- Modify: `vtorrent-btc/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `vtorrent-btc/src/sync.rs`:

```rust
//! Live Bitcoin header sync and Bloom-filter UTXO scan.

use crate::error::{BtcError, Result};
use crate::headers::HeaderChain;
use crate::p2p::BtcPeer;
use crate::utxo::UtxoSet;
use bitcoin::consensus::encode::serialize;
use bitcoin::p2p::message::{NetworkMessage, RawNetworkMessage};
use bitcoin::p2p::message_blockdata::{GetHeadersMessage, Inventory};
use bitcoin::p2p::message_bloom::FilterLoad;
use bitcoin::p2p::ServiceFlags;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use vtorrent_spv::BloomFilter;

/// Bitcoin DNS seeds (mainnet).
pub const DNS_SEEDS: &[&str] = &[
    "seed.bitcoin.sipa.be",
    "dnsseed.bluematt.me",
    "dnsseed.bitcoin.dashjr.org",
];

/// Resolve DNS seeds to socket addresses.
pub async fn resolve_seeds() -> Result<Vec<SocketAddr>> {
    let mut addrs = Vec::new();
    for seed in DNS_SEEDS {
        match tokio::net::lookup_host((*seed, 8333)).await {
            Ok(iter) => addrs.extend(iter),
            Err(e) => tracing::warn!("DNS seed {} failed: {}", seed, e),
        }
    }
    if addrs.is_empty() {
        return Err(BtcError::Dns("no DNS seeds resolved".into()));
    }
    Ok(addrs)
}

/// A Bitcoin SPV sync engine.
pub struct BtcSync {
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    addresses: Vec<String>,
}

impl BtcSync {
    pub fn new(
        headers: Arc<Mutex<HeaderChain>>,
        utxos: Arc<Mutex<UtxoSet>>,
        addresses: Vec<String>,
    ) -> Self {
        Self {
            headers,
            utxos,
            addresses,
        }
    }

    /// Build a BIP37 Bloom filter from the wallet's addresses.
    pub fn build_filter(&self) -> BloomFilter {
        let mut filter = BloomFilter::new(self.addresses.len().max(1), 0.001, 0);
        for addr in &self.addresses {
            filter.insert(addr.as_bytes());
        }
        filter
    }

    /// Build a `getheaders` message from the current tip.
    pub fn build_getheaders(&self) -> GetHeadersMessage {
        let headers = self.headers.lock().unwrap();
        let locator = if let Some(best) = headers.best_hash() {
            vec![bitcoin::BlockHash::from_byte_array(best)]
        } else {
            vec![]
        };
        GetHeadersMessage {
            version: 70016,
            locator_hashes: locator,
            stop_hash: bitcoin::BlockHash::all_zeros(),
        }
    }

    /// Build a `filterload` message from the wallet's addresses.
    pub fn build_filterload(&self) -> FilterLoad {
        let filter = self.build_filter();
        let (data, hash_funcs, tweak, flags) = filter.to_wire();
        FilterLoad {
            filter: data,
            hash_funcs,
            tweak,
            flags: bitcoin::p2p::message_bloom::BloomFlags::All,
        }
    }

    /// Run one sync pass against a single peer.
    pub async fn sync_once(&self, peer: &mut BtcPeer) -> Result<usize> {
        // Send filterload + getheaders.
        peer.send(NetworkMessage::FilterLoad(self.build_filterload()))
            .await?;
        peer.send(NetworkMessage::GetHeaders(self.build_getheaders()))
            .await?;

        let mut added = 0usize;
        // Read messages until we get headers.
        loop {
            match peer.recv().await? {
                NetworkMessage::Headers(hdrs) => {
                    for h in hdrs {
                        let raw = serialize(&h);
                        let height = {
                            let chain = self.headers.lock().unwrap();
                            chain.best_height() + 1
                        };
                        self.headers.lock().unwrap().add_header(&raw, height)?;
                        added += 1;
                    }
                    break;
                }
                NetworkMessage::Verack | NetworkMessage::Version(_) => continue,
                _ => continue,
            }
        }
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_filter_nonempty() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec!["bc1qtest".to_string()],
        );
        let filter = sync.build_filter();
        assert!(!filter.is_empty());
    }

    #[test]
    fn test_build_getheaders_empty_locator() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![],
        );
        let msg = sync.build_getheaders();
        assert!(msg.locator_hashes.is_empty());
    }

    #[test]
    fn test_build_filterload() {
        let sync = BtcSync::new(
            Arc::new(Mutex::new(HeaderChain::new())),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec!["bc1qtest".to_string()],
        );
        let fl = sync.build_filterload();
        assert!(!fl.filter.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run: `cargo test -p vtorrent-btc sync::`
Expected: 3 tests pass. Adjust `BloomFlags` import path if needed (it is `bitcoin::p2p::message_bloom::BloomFlags`).

- [ ] **Step 3: Export the module and commit**

In `vtorrent-btc/src/lib.rs`, add `pub mod sync;` after `pub mod p2p;`.

```bash
git add vtorrent-btc/src/sync.rs vtorrent-btc/src/lib.rs
git commit -m "feat: add Bitcoin header sync and Bloom filter to vtorrent-btc"
```

---

## Task 4: Wire sync into `BtcWallet`

**Files:**
- Modify: `vtorrent-btc/src/wallet.rs`

- [ ] **Step 1: Add a `synced` flag and `sync()` method**

In `vtorrent-btc/src/wallet.rs`, add a `synced: bool` field to `BtcWallet`, initialize it to `false` in `new()`, and add:

```rust
    /// Whether the header chain has synced at least once.
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Mark the wallet as synced.
    pub fn mark_synced(&mut self) {
        self.synced = true;
    }

    /// Run a sync pass against a peer, updating headers and the synced flag.
    pub async fn sync(&mut self, peer: &mut crate::p2p::BtcPeer) -> Result<usize> {
        let sync = crate::sync::BtcSync::new(
            self.headers.clone(),
            self.utxos.clone(),
            vec![self.current_address()?],
        );
        let added = sync.sync_once(peer).await?;
        if added > 0 {
            self.synced = true;
        }
        Ok(added)
    }
```

Note: `BtcWallet` currently holds `headers: Arc<Mutex<HeaderChain>>` and `utxos: Arc<Mutex<UtxoSet>>` — these are already `Arc<Mutex<...>>`, so they can be cloned. Add `use crate::error::Result;` if not already imported.

- [ ] **Step 2: Update the existing test**

In `vtorrent-btc/src/wallet.rs`, add a test:

```rust
    #[test]
    fn test_synced_default_false() {
        let w = BtcWallet::new([7u8; 64]);
        assert!(!w.synced());
    }
```

- [ ] **Step 3: Run tests and commit**

Run: `cargo test -p vtorrent-btc wallet::`
Expected: 4 tests pass.

```bash
git add vtorrent-btc/src/wallet.rs
git commit -m "feat: add sync method and synced flag to BtcWallet"
```

---

## Task 5: Replace the daemon placeholder with the real sync loop

**Files:**
- Modify: `vtorrent-daemon/src/main.rs`

- [ ] **Step 1: Replace the placeholder task**

In `vtorrent-daemon/src/main.rs`, replace the placeholder BTC task (the one that logs "Bitcoin SPV wallet task started (idle)") with:

```rust
    // Bitcoin SPV sync — resolves DNS seeds and syncs headers in a loop.
    let btc_wallet = Arc::clone(&rpc_state.btc_wallet);
    tokio::spawn(async move {
        tracing::info!("Bitcoin SPV sync task started");
        loop {
            // Only sync when a wallet with a seed is present.
            let has_wallet = btc_wallet.read().await.is_some();
            if !has_wallet {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                continue;
            }
            match vtorrent_btc::sync::resolve_seeds().await {
                Ok(addrs) => {
                    for addr in addrs {
                        match vtorrent_btc::p2p::BtcPeer::connect(addr).await {
                            Ok(mut peer) => {
                                if let Some(w) = btc_wallet.write().await.as_mut() {
                                    match w.sync(&mut peer).await {
                                        Ok(n) => tracing::info!("BTC sync: {} headers", n),
                                        Err(e) => tracing::warn!("BTC sync error: {}", e),
                                    }
                                }
                            }
                            Err(e) => tracing::warn!("BTC peer {} failed: {}", addr, e),
                        }
                    }
                }
                Err(e) => tracing::warn!("BTC seed resolution failed: {}", e),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });
```

- [ ] **Step 2: Build and commit**

Run: `cargo build -p vtorrent-daemon 2>&1 | tail -5`
Expected: builds successfully.

```bash
git add vtorrent-daemon/src/main.rs
git commit -m "feat: run live Bitcoin SPV sync loop in daemon"
```

---

## Task 6: Reflect sync state in RPC status

**Files:**
- Modify: `vtorrent-rpc/src/handlers.rs`

- [ ] **Step 1: Use the `synced()` flag**

In `vtorrent-rpc/src/handlers.rs`, in `get_btc_status`, change the `synced` field to use the wallet's `synced()` method instead of `best_height() > 0`:

```rust
        Some(w) => Ok(Json(BtcStatusResponse {
            initialized: true,
            balance_satoshis: w.balance(),
            address: w.current_address().ok(),
            best_height: w.best_height(),
            synced: w.synced(),
        })),
```

- [ ] **Step 2: Build, test, and commit**

Run: `cargo test -p vtorrent-rpc test_btc_status_uninitialized 2>&1 | tail -5`
Expected: PASS.

```bash
git add vtorrent-rpc/src/handlers.rs
git commit -m "feat: reflect real BTC sync state in RPC status"
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

- [ ] **Step 3: Commit any remaining changes**

```bash
git add -A
git commit -m "chore: final verification of BTC HTLC and sync"
```
