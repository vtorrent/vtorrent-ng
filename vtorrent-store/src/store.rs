//! Core block and UTXO store backed by redb.

use crate::error::{Result, StoreError};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::path::Path;
use vtorrent_node::block::Block;
use vtorrent_node::chain::Utxo;

// ─── Table definitions ────────────────────────────────────────────────────────

/// Full blocks indexed by their 32-byte hash (stored as hex string key).
const BLOCKS: TableDefinition<&str, &[u8]> = TableDefinition::new("blocks");

/// Main chain height → block hash (hex string).
const HEIGHT_INDEX: TableDefinition<u32, &str> = TableDefinition::new("height_index");

/// UTXO set: key = "txid_hex:vout", value = JSON-encoded Utxo.
const UTXOS: TableDefinition<&str, &[u8]> = TableDefinition::new("utxos");

/// Claimed legacy addresses (value is always b"1").
const CLAIMED_ADDRS: TableDefinition<&str, u8> = TableDefinition::new("claimed_addrs");

/// Chain metadata: key/value string pairs.
const META: TableDefinition<&str, &str> = TableDefinition::new("meta");

// ─── BlockStore ───────────────────────────────────────────────────────────────

/// Persistent block and UTXO store.
///
/// All writes are atomic redb transactions. Reads are lock-free snapshots.
pub struct BlockStore {
    db: Database,
}

impl BlockStore {
    /// Open or create a block store at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path.as_ref())?;
        // Ensure all tables exist.
        let write_txn = db.begin_write()?;
        {
            write_txn.open_table(BLOCKS)?;
            write_txn.open_table(HEIGHT_INDEX)?;
            write_txn.open_table(UTXOS)?;
            write_txn.open_table(CLAIMED_ADDRS)?;
            write_txn.open_table(META)?;
        }
        write_txn.commit()?;
        tracing::info!("Opened block store at {}", path.as_ref().display());
        Ok(Self { db })
    }

    // ─── Chain metadata ───────────────────────────────────────────────────────

    /// Return the height of the best (main chain tip) block.
    pub fn best_height(&self) -> Result<u32> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META)?;
        match table.get("best_height")? {
            Some(v) => Ok(v.value().parse::<u32>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Return the hash of the best block as a hex string, or None if empty.
    pub fn best_hash(&self) -> Result<Option<[u8; 32]>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(META)?;
        match table.get("best_hash")? {
            Some(v) => {
                let hex_str = v.value().to_string();
                let bytes = hex::decode(&hex_str)
                    .map_err(|_| StoreError::Corrupted("invalid best_hash hex".into()))?;
                if bytes.len() != 32 {
                    return Err(StoreError::Corrupted("best_hash wrong length".into()));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            }
            None => Ok(None),
        }
    }

    // ─── Block storage ────────────────────────────────────────────────────────

    /// Persist a block to the main chain at the given height.
    ///
    /// This is an atomic operation: the block, height index, UTXO changes,
    /// and metadata are all written in a single redb transaction.
    pub fn append_block(
        &self,
        block: &Block,
        height: u32,
        utxos_added: &[Utxo],
        utxos_removed: &[([u8; 32], u32)],
        claimed_addresses: &[String],
    ) -> Result<()> {
        let block_hash = block.hash();
        let hash_hex = hex::encode(block_hash);

        let block_bytes = serde_json::to_vec(block)?;

        let write_txn = self.db.begin_write()?;
        {
            // Store the full block.
            let mut blocks = write_txn.open_table(BLOCKS)?;
            blocks.insert(hash_hex.as_str(), block_bytes.as_slice())?;

            // Update height index.
            let mut height_idx = write_txn.open_table(HEIGHT_INDEX)?;
            height_idx.insert(height, hash_hex.as_str())?;

            // Apply UTXO changes.
            let mut utxos = write_txn.open_table(UTXOS)?;
            for utxo in utxos_added {
                let key = format!("{}:{}", hex::encode(utxo.txid), utxo.vout);
                let val = serde_json::to_vec(utxo)?;
                utxos.insert(key.as_str(), val.as_slice())?;
            }
            for (txid, vout) in utxos_removed {
                let key = format!("{}:{}", hex::encode(txid), vout);
                utxos.remove(key.as_str())?;
            }

            // Mark claimed legacy addresses.
            let mut claimed = write_txn.open_table(CLAIMED_ADDRS)?;
            for addr in claimed_addresses {
                claimed.insert(addr.as_str(), 1u8)?;
            }

            // Update metadata.
            let mut meta = write_txn.open_table(META)?;
            let height_str = height.to_string();
            meta.insert("best_height", height_str.as_str())?;
            meta.insert("best_hash", hash_hex.as_str())?;
        }
        write_txn.commit()?;

        tracing::debug!("Stored block {} at height {}", &hash_hex[..8], height);
        Ok(())
    }

    /// Roll back the tip block (for reorg support).
    pub fn rollback_tip(
        &self,
        utxos_to_restore: &[Utxo],
        utxos_to_remove: &[([u8; 32], u32)],
        claimed_addresses_to_remove: &[String],
    ) -> Result<()> {
        let current_height = self.best_height()?;
        if current_height == 0 {
            return Err(StoreError::Corrupted(
                "Cannot roll back genesis block".into(),
            ));
        }
        let new_height = current_height - 1;

        let write_txn = self.db.begin_write()?;
        {
            // Restore removed UTXOs.
            let mut utxos = write_txn.open_table(UTXOS)?;
            for utxo in utxos_to_restore {
                let key = format!("{}:{}", hex::encode(utxo.txid), utxo.vout);
                let val = serde_json::to_vec(utxo)?;
                utxos.insert(key.as_str(), val.as_slice())?;
            }
            // Remove UTXOs that were created by the rolled-back block.
            for (txid, vout) in utxos_to_remove {
                let key = format!("{}:{}", hex::encode(txid), vout);
                utxos.remove(key.as_str())?;
            }

            // Un-claim legacy addresses.
            let mut claimed = write_txn.open_table(CLAIMED_ADDRS)?;
            for addr in claimed_addresses_to_remove {
                claimed.remove(addr.as_str())?;
            }

            // Update height index and metadata.
            let mut height_idx = write_txn.open_table(HEIGHT_INDEX)?;
            height_idx.remove(current_height)?;

            // Find the new best hash from the height index.
            let new_hash_hex = height_idx
                .get(new_height)?
                .map(|v| v.value().to_string())
                .unwrap_or_else(|| "0".repeat(64));

            let mut meta = write_txn.open_table(META)?;
            let new_height_str = new_height.to_string();
            meta.insert("best_height", new_height_str.as_str())?;
            meta.insert("best_hash", new_hash_hex.as_str())?;
        }
        write_txn.commit()?;

        tracing::debug!("Rolled back to height {}", new_height);
        Ok(())
    }

    // ─── Block queries ────────────────────────────────────────────────────────

    /// Retrieve a block by its hash.
    pub fn get_block(&self, hash: &[u8; 32]) -> Result<Option<Block>> {
        let hash_hex = hex::encode(hash);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOCKS)?;
        match table.get(hash_hex.as_str())? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Retrieve a block by its main chain height.
    pub fn get_block_at_height(&self, height: u32) -> Result<Option<Block>> {
        let read_txn = self.db.begin_read()?;
        let height_idx = read_txn.open_table(HEIGHT_INDEX)?;
        let hash_hex = match height_idx.get(height)? {
            Some(v) => v.value().to_string(),
            None => return Ok(None),
        };
        let blocks = read_txn.open_table(BLOCKS)?;
        match blocks.get(hash_hex.as_str())? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    /// Return all block hashes on the main chain from genesis to tip.
    pub fn main_chain_hashes(&self) -> Result<Vec<[u8; 32]>> {
        let height = self.best_height()?;
        let read_txn = self.db.begin_read()?;
        let height_idx = read_txn.open_table(HEIGHT_INDEX)?;
        let mut hashes = Vec::with_capacity(height as usize + 1);
        for h in 0..=height {
            if let Some(v) = height_idx.get(h)? {
                let bytes = hex::decode(v.value())
                    .map_err(|_| StoreError::Corrupted(format!("bad hash at height {}", h)))?;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                hashes.push(arr);
            }
        }
        Ok(hashes)
    }

    // ─── UTXO queries ─────────────────────────────────────────────────────────

    /// Return all UTXOs in the current set.
    pub fn all_utxos(&self) -> Result<Vec<Utxo>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(UTXOS)?;
        let mut result = Vec::new();
        for entry in table.iter()? {
            let (_, v) = entry?;
            let utxo: Utxo = serde_json::from_slice(v.value())?;
            result.push(utxo);
        }
        Ok(result)
    }

    /// Return all UTXOs whose `script_pubkey` matches the given script.
    pub fn utxos_for_script(&self, script: &[u8]) -> Result<Vec<Utxo>> {
        Ok(self
            .all_utxos()?
            .into_iter()
            .filter(|u| u.script_pubkey == script)
            .collect())
    }

    /// Return the total balance (sum of UTXO values) for a given script.
    pub fn balance_for_script(&self, script: &[u8]) -> Result<u64> {
        Ok(self.utxos_for_script(script)?.iter().map(|u| u.value).sum())
    }

    /// Check whether a specific UTXO exists.
    pub fn has_utxo(&self, txid: &[u8; 32], vout: u32) -> Result<bool> {
        let key = format!("{}:{}", hex::encode(txid), vout);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(UTXOS)?;
        Ok(table.get(key.as_str())?.is_some())
    }

    /// Return a specific UTXO by txid + vout.
    pub fn get_utxo(&self, txid: &[u8; 32], vout: u32) -> Result<Option<Utxo>> {
        let key = format!("{}:{}", hex::encode(txid), vout);
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(UTXOS)?;
        match table.get(key.as_str())? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    }

    // ─── Claimed address queries ───────────────────────────────────────────────

    /// Check whether a legacy address has already been claimed.
    pub fn is_claimed(&self, address: &str) -> Result<bool> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CLAIMED_ADDRS)?;
        Ok(table.get(address)?.is_some())
    }

    /// Return all claimed legacy addresses.
    pub fn all_claimed_addresses(&self) -> Result<Vec<String>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(CLAIMED_ADDRS)?;
        let mut result = Vec::new();
        for entry in table.iter()? {
            let (k, _) = entry?;
            result.push(k.value().to_string());
        }
        Ok(result)
    }

    // ─── Chain loading ────────────────────────────────────────────────────────

    /// Load the persisted chain state into an in-memory `Chain`.
    ///
    /// This replays the stored blocks into a fresh `Chain` instance, which
    /// rebuilds the UTXO set and claimed-address set from the journal.
    /// For large chains this is fast because we skip validation on trusted
    /// stored data.
    pub fn load_into_chain(&self) -> Result<vtorrent_node::chain::Chain> {
        use vtorrent_node::chain::Chain;

        let height = self.best_height()?;
        tracing::info!("Loading chain from store: {} blocks", height + 1);

        let mut chain =
            Chain::new().map_err(|e| StoreError::Corrupted(format!("chain init failed: {}", e)))?;

        // Replay blocks from height 1 (genesis is already in Chain::new()).
        for h in 1..=height {
            if let Some(block) = self.get_block_at_height(h)? {
                chain.add_block(block).map_err(|e| {
                    StoreError::Corrupted(format!("replay failed at height {}: {}", h, e))
                })?;
            } else {
                return Err(StoreError::Corrupted(format!(
                    "missing block at height {} during load",
                    h
                )));
            }
        }

        tracing::info!(
            "Chain loaded successfully at height {}",
            chain.best_height()
        );
        Ok(chain)
    }

    // ─── Statistics ───────────────────────────────────────────────────────────

    /// Return the total number of stored blocks.
    pub fn block_count(&self) -> Result<u64> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(BLOCKS)?;
        Ok(table.len()?)
    }

    /// Return the total number of UTXOs in the current set.
    pub fn utxo_count(&self) -> Result<u64> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(UTXOS)?;
        Ok(table.len()?)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use vtorrent_node::block::{Block, BlockHeader, Transaction, TxOutput, TxType};

    fn make_block(prev_hash: [u8; 32], height: u32) -> Block {
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 5_000_000_000,
                script_pubkey: vec![0x76, 0xa9],
            }],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };
        let merkle = coinbase.txid();
        Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: merkle,
                timestamp: 1_700_000_000 + height,
                bits: 0x1d00ffff,
                nonce: height,
                stake_modifier: 0,
            },
            transactions: vec![coinbase],
        }
    }

    fn make_utxo(txid: [u8; 32], vout: u32, value: u64, height: u32) -> Utxo {
        Utxo {
            txid,
            vout,
            value,
            script_pubkey: vec![0x76, 0xa9],
            height,
            timestamp: 1_700_000_000 + height,
        }
    }

    #[test]
    fn test_open_and_empty_state() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();
        assert_eq!(store.best_height().unwrap(), 0);
        assert!(store.best_hash().unwrap().is_none());
        assert_eq!(store.block_count().unwrap(), 0);
        assert_eq!(store.utxo_count().unwrap(), 0);
    }

    #[test]
    fn test_append_and_retrieve_block() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let genesis_hash = [0u8; 32];
        let block = make_block(genesis_hash, 1);
        let block_hash = block.hash();
        let utxo = make_utxo(block.transactions[0].txid(), 0, 5_000_000_000, 1);

        store
            .append_block(&block, 1, std::slice::from_ref(&utxo), &[], &[])
            .unwrap();

        assert_eq!(store.best_height().unwrap(), 1);
        assert_eq!(store.best_hash().unwrap(), Some(block_hash));
        assert_eq!(store.block_count().unwrap(), 1);
        assert_eq!(store.utxo_count().unwrap(), 1);

        let retrieved = store.get_block(&block_hash).unwrap().unwrap();
        assert_eq!(retrieved.hash(), block_hash);

        let retrieved_at_height = store.get_block_at_height(1).unwrap().unwrap();
        assert_eq!(retrieved_at_height.hash(), block_hash);
    }

    #[test]
    fn test_utxo_has_and_get() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let block = make_block([0u8; 32], 1);
        let txid = block.transactions[0].txid();
        let utxo = make_utxo(txid, 0, 1_000_000, 1);

        store
            .append_block(&block, 1, std::slice::from_ref(&utxo), &[], &[])
            .unwrap();

        assert!(store.has_utxo(&txid, 0).unwrap());
        assert!(!store.has_utxo(&txid, 1).unwrap());

        let fetched = store.get_utxo(&txid, 0).unwrap().unwrap();
        assert_eq!(fetched.value, 1_000_000);
    }

    #[test]
    fn test_utxo_removal() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let block1 = make_block([0u8; 32], 1);
        let txid1 = block1.transactions[0].txid();
        let utxo = make_utxo(txid1, 0, 1_000_000, 1);
        store.append_block(&block1, 1, &[utxo], &[], &[]).unwrap();

        let block2 = make_block(block1.hash(), 2);
        store
            .append_block(&block2, 2, &[], &[(txid1, 0)], &[])
            .unwrap();

        assert!(!store.has_utxo(&txid1, 0).unwrap());
        assert_eq!(store.utxo_count().unwrap(), 0);
    }

    #[test]
    fn test_claimed_addresses() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let block = make_block([0u8; 32], 1);
        let addr = "1A1zP1eP5QGefi2DMPTfTL5SLmv7Divf".to_string();

        store
            .append_block(&block, 1, &[], &[], std::slice::from_ref(&addr))
            .unwrap();

        assert!(store.is_claimed(&addr).unwrap());
        assert!(!store.is_claimed("VUnknownAddress").unwrap());

        let all = store.all_claimed_addresses().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], addr);
    }

    #[test]
    fn test_rollback_tip() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let block1 = make_block([0u8; 32], 1);
        let txid1 = block1.transactions[0].txid();
        let utxo1 = make_utxo(txid1, 0, 5_000_000_000, 1);
        store
            .append_block(&block1, 1, std::slice::from_ref(&utxo1), &[], &[])
            .unwrap();

        let block2 = make_block(block1.hash(), 2);
        let txid2 = block2.transactions[0].txid();
        let utxo2 = make_utxo(txid2, 0, 5_000_000_000, 2);
        store
            .append_block(&block2, 2, &[utxo2], &[(txid1, 0)], &[])
            .unwrap();

        assert_eq!(store.best_height().unwrap(), 2);
        assert!(!store.has_utxo(&txid1, 0).unwrap());

        // Roll back block 2: restore utxo1, remove utxo2.
        store.rollback_tip(&[utxo1], &[(txid2, 0)], &[]).unwrap();

        assert_eq!(store.best_height().unwrap(), 1);
        assert!(store.has_utxo(&txid1, 0).unwrap());
        assert!(!store.has_utxo(&txid2, 0).unwrap());
    }

    #[test]
    fn test_main_chain_hashes() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let b1 = make_block([0u8; 32], 1);
        let h1 = b1.hash();
        store.append_block(&b1, 1, &[], &[], &[]).unwrap();

        let b2 = make_block(h1, 2);
        let h2 = b2.hash();
        store.append_block(&b2, 2, &[], &[], &[]).unwrap();

        let hashes = store.main_chain_hashes().unwrap();
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], h1);
        assert_eq!(hashes[1], h2);
    }

    #[test]
    fn test_balance_for_script() {
        let dir = tempdir().unwrap();
        let store = BlockStore::open(dir.path().join("chain.db")).unwrap();

        let script = vec![0x76, 0xa9, 0x14];
        let other_script = vec![0x00, 0x14];

        let txid1 = [1u8; 32];
        let txid2 = [2u8; 32];
        let u1 = Utxo {
            txid: txid1,
            vout: 0,
            value: 1_000_000,
            script_pubkey: script.clone(),
            height: 1,
            timestamp: 1,
        };
        let u2 = Utxo {
            txid: txid2,
            vout: 0,
            value: 2_000_000,
            script_pubkey: other_script.clone(),
            height: 1,
            timestamp: 1,
        };

        let block = make_block([0u8; 32], 1);
        store.append_block(&block, 1, &[u1, u2], &[], &[]).unwrap();

        assert_eq!(store.balance_for_script(&script).unwrap(), 1_000_000);
        assert_eq!(store.balance_for_script(&other_script).unwrap(), 2_000_000);
    }

    #[test]
    fn test_persistence_across_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.db");

        {
            let store = BlockStore::open(&path).unwrap();
            let block = make_block([0u8; 32], 1);
            let utxo = make_utxo(block.transactions[0].txid(), 0, 5_000_000_000, 1);
            store.append_block(&block, 1, &[utxo], &[], &[]).unwrap();
            assert_eq!(store.best_height().unwrap(), 1);
        }

        // Reopen and verify data persisted.
        {
            let store = BlockStore::open(&path).unwrap();
            assert_eq!(store.best_height().unwrap(), 1);
            assert_eq!(store.block_count().unwrap(), 1);
            assert_eq!(store.utxo_count().unwrap(), 1);
        }
    }
}
