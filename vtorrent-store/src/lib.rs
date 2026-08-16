//! # vtorrent-store
//!
//! Persistent block and UTXO storage for vTorrent using [redb](https://github.com/cberner/redb),
//! a pure-Rust embedded database with ACID transactions and no C dependencies.
//!
//! ## Tables
//!
//! | Table | Key | Value | Purpose |
//! |---|---|---|---|
//! | `blocks` | `[u8; 32]` block hash | JSON-encoded `Block` | Full block storage |
//! | `height_index` | `u32` height | `[u8; 32]` block hash | Main chain height → hash |
//! | `utxos` | `(txid_hex, vout)` string | JSON-encoded `Utxo` | UTXO set |
//! | `claimed_addrs` | address string | `1u8` | Claimed legacy addresses |
//! | `meta` | string key | string value | Chain metadata (best_height, etc.) |
//!
//! ## Usage
//!
//! ```no_run
//! use vtorrent_store::BlockStore;
//!
//! let store = BlockStore::open("~/.vtorrent/chain.db").unwrap();
//! let height = store.best_height().unwrap();
//! println!("Loaded chain at height {}", height);
//! ```

pub mod error;
pub mod store;

pub use error::StoreError;
pub use store::BlockStore;
