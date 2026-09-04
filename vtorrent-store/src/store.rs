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
const STORE_PROTOCOL_VERSION: &str = "3";

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

            let has_blocks = write_txn.open_table(HEIGHT_INDEX)?.len()? > 0;
            let stored_protocol = write_txn
                .open_table(META)?
                .get("protocol_version")?
                .map(|value| value.value().to_string());
            match stored_protocol.as_deref() {
                Some(STORE_PROTOCOL_VERSION) => {}
                Some(other) => {
                    return Err(StoreError::Corrupted(format!(
                        "block store protocol {} is incompatible with required protocol {}; start with a new data directory",
                        other, STORE_PROTOCOL_VERSION
                    )));
                }
                None if has_blocks => {
                    return Err(StoreError::Corrupted(
                        "legacy block store predates protocol 3 UTXO commitments; start with a new data directory".into(),
                    ));
                }
                None => {
                    write_txn
                        .open_table(META)?
                        .insert("protocol_version", STORE_PROTOCOL_VERSION)?;
                }
            }

            // Backfill genesis on first open (and on stores created before
            // this convention): reorgs that roll back to height 0 need the
            // genesis entry present in HEIGHT_INDEX/BLOCKS.
            let needs_genesis = write_txn.open_table(HEIGHT_INDEX)?.get(0)?.is_none();
            let mut backfilled_genesis = false;
            if needs_genesis {
                let genesis = vtorrent_node::genesis::create_genesis_block();
                let hash_hex = hex::encode(genesis.hash());
                let block_bytes = serde_json::to_vec(&genesis)?;
                write_txn
                    .open_table(BLOCKS)?
                    .insert(hash_hex.as_str(), block_bytes.as_slice())?;
                write_txn
                    .open_table(HEIGHT_INDEX)?
                    .insert(0, hash_hex.as_str())?;
                backfilled_genesis = true;
            }

            // Repair META when it lags the height index (e.g. after genesis
            // backfill on a pre-existing store, or a failed mid-commit).
            {
                let max_indexed = write_txn
                    .open_table(HEIGHT_INDEX)?
                    .iter()?
                    .filter_map(|r| r.ok().map(|(k, _)| k.value()))
                    .max()
                    .unwrap_or(0);
                let stored = write_txn
                    .open_table(META)?
                    .get("best_height")?
                    .and_then(|v| v.value().parse::<u32>().ok())
                    .unwrap_or(0);
                if backfilled_genesis || stored < max_indexed {
                    let hash_hex = write_txn
                        .open_table(HEIGHT_INDEX)?
                        .get(&max_indexed)?
                        .map(|v| v.value().to_string())
                        .ok_or_else(|| {
                            StoreError::Corrupted(format!(
                                "height index missing block {}",
                                max_indexed
                            ))
                        })?;
                    let mut meta = write_txn.open_table(META)?;
                    meta.insert("best_height", max_indexed.to_string().as_str())?;
                    meta.insert("best_hash", hash_hex.as_str())?;
                    tracing::warn!(
                        "Repaired block store META: best_height {} -> {}",
                        stored,
                        max_indexed
                    );
                }
            }
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
            Some(v) => v
                .value()
                .parse::<u32>()
                .map_err(|_| StoreError::Corrupted("invalid best_height".into())),
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
                .ok_or_else(|| {
                    StoreError::Corrupted(format!("no block at height {}", new_height))
                })?;

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
        // Cap the allocation: a corrupted best_height must not drive a huge
        // allocation or a multi-billion-iteration loop.
        const MAX_CHAIN_HEIGHT: u32 = 10_000_000;
        if height > MAX_CHAIN_HEIGHT {
            return Err(StoreError::Corrupted(format!(
                "best_height {} exceeds maximum {}",
                height, MAX_CHAIN_HEIGHT
            )));
        }
        let read_txn = self.db.begin_read()?;
        let height_idx = read_txn.open_table(HEIGHT_INDEX)?;
        let mut hashes = Vec::with_capacity(height as usize + 1);
        for h in 0..=height {
            if let Some(v) = height_idx.get(h)? {
                let bytes = hex::decode(v.value())
                    .map_err(|_| StoreError::Corrupted(format!("bad hash at height {}", h)))?;
                if bytes.len() != 32 {
                    return Err(StoreError::Corrupted(format!(
                        "bad hash length {} at height {}",
                        bytes.len(),
                        h
                    )));
                }
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
        self.load_into_chain_with_mode(false, false)
    }

    pub fn load_into_regtest_chain(&self) -> Result<vtorrent_node::chain::Chain> {
        self.load_into_chain_with_mode(true, false)
    }

    pub fn load_into_fast_regtest_chain(&self) -> Result<vtorrent_node::chain::Chain> {
        self.load_into_chain_with_mode(true, true)
    }

    fn load_into_chain_with_mode(
        &self,
        regtest: bool,
        fast_stake: bool,
    ) -> Result<vtorrent_node::chain::Chain> {
        use vtorrent_node::chain::Chain;

        let height = self.best_height()?;
        tracing::info!("Loading chain from store: {} blocks", height + 1);

        let make_chain = || {
            if regtest && fast_stake {
                Chain::new_regtest_fast()
            } else if regtest {
                Chain::new_regtest()
            } else {
                Chain::new()
            }
            .map_err(|e| StoreError::Corrupted(format!("chain init failed: {}", e)))
        };
        let mut chain = make_chain()?;

        // Replay blocks from height 1 (genesis is already in Chain::new()).
        // On failure the store is truncated to the last good height and
        // rebuilt — a diverged/corrupted tail must never brick the node.
        let mut replay_height = height;
        // Bounded self-heal: each round truncates at least one block, so this
        // terminates. Multi-site corruption converges over successive rounds.
        for _round in 0..=height {
            match self.replay_range(&mut chain, 1, replay_height) {
                Ok(()) => break,
                Err(first_err) => {
                    // Find the exact failing height by scanning from genesis.
                    let bad = self.first_failing_height(&mut chain, replay_height);
                    let keep = bad.saturating_sub(1);
                    tracing::error!(
                        "Replay failed at height {} ({}); truncating store to height {} and rebuilding",
                        bad,
                        first_err,
                        keep
                    );
                    self.truncate_above(keep)?;
                    self.clear_derived_state()?;
                    chain = make_chain()?;
                    self.replay_and_repersist(&mut chain, 1, keep)?;
                    replay_height = keep;
                }
            }
        }

        tracing::info!(
            "Chain loaded successfully at height {}",
            chain.best_height()
        );
        Ok(chain)
    }

    /// Rebuild ALL derived state (UTXOS, CLAIMED_ADDRS, HEIGHT_INDEX) from a
    /// complete in-memory block list. Used when event loss may have left the
    /// store behind the chain.
    pub fn rebuild_from_blocks(&self, blocks: &[vtorrent_node::block::Block]) -> Result<()> {
        self.rebuild_from_blocks_with_mode(blocks, false, false)
    }

    pub fn rebuild_from_regtest_blocks(
        &self,
        blocks: &[vtorrent_node::block::Block],
    ) -> Result<()> {
        self.rebuild_from_blocks_with_mode(blocks, true, false)
    }

    pub fn rebuild_from_fast_regtest_blocks(
        &self,
        blocks: &[vtorrent_node::block::Block],
    ) -> Result<()> {
        self.rebuild_from_blocks_with_mode(blocks, true, true)
    }

    fn rebuild_from_blocks_with_mode(
        &self,
        blocks: &[vtorrent_node::block::Block],
        regtest: bool,
        fast_stake: bool,
    ) -> Result<()> {
        let chain_result = if regtest && fast_stake {
            vtorrent_node::chain::Chain::new_regtest_fast()
        } else if regtest {
            vtorrent_node::chain::Chain::new_regtest()
        } else {
            vtorrent_node::chain::Chain::new()
        };
        let mut chain = chain_result.map_err(|e| {
            StoreError::Corrupted(format!("chain init failed during rebuild: {}", e))
        })?;
        self.truncate_above(0)?;
        self.clear_derived_state()?;
        for (i, block) in blocks.iter().enumerate() {
            let h = i as u32;
            let acceptance = chain.add_block(block.clone()).map_err(|e| {
                StoreError::Corrupted(format!("rebuild failed at height {}: {}", h, e))
            })?;
            match acceptance {
                vtorrent_node::chain::BlockAcceptance::MainChain {
                    utxos_added,
                    utxos_removed,
                    claimed_addresses,
                    ..
                } => {
                    self.append_block(block, h, &utxos_added, &utxos_removed, &claimed_addresses)?;
                }
                // Genesis is already present in a fresh Chain::new(); skip it.
                vtorrent_node::chain::BlockAcceptance::Duplicate if h == 0 => {}
                _ => {
                    return Err(StoreError::Corrupted(format!(
                        "unexpected acceptance during rebuild at height {}",
                        h
                    )));
                }
            }
        }
        Ok(())
    }

    /// Replay `from..=to` through the in-memory chain only.
    fn replay_range(
        &self,
        chain: &mut vtorrent_node::chain::Chain,
        from: u32,
        to: u32,
    ) -> Result<()> {
        for h in from..=to {
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
        Ok(())
    }

    /// Find the lowest height whose block fails replay, starting fresh.
    fn first_failing_height(&self, chain: &mut vtorrent_node::chain::Chain, max: u32) -> u32 {
        for h in 1..=max {
            match self.get_block_at_height(h) {
                Ok(Some(block)) => {
                    if chain.add_block(block).is_err() {
                        return h;
                    }
                }
                _ => return h,
            }
        }
        max
    }

    /// Remove all blocks above `keep` from BLOCKS + HEIGHT_INDEX.
    fn truncate_above(&self, keep: u32) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut height_idx = write_txn.open_table(HEIGHT_INDEX)?;
            let mut blocks = write_txn.open_table(BLOCKS)?;
            let doomed: Vec<(u32, String)> = height_idx
                .range(keep + 1..)?
                .filter_map(|r| r.ok().map(|(k, v)| (k.value(), v.value().to_string())))
                .collect();
            for (h, hash_hex) in doomed {
                blocks.remove(hash_hex.as_str())?;
                height_idx.remove(h)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Wipe UTXOS + CLAIMED tables (rebuilt by replay_and_repersist).
    fn clear_derived_state(&self) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut utxos = write_txn.open_table(UTXOS)?;
            let keys: Vec<String> = utxos
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for k in keys {
                utxos.remove(k.as_str())?;
            }
            let mut claimed = write_txn.open_table(CLAIMED_ADDRS)?;
            let ckeys: Vec<String> = claimed
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
                .collect();
            for k in ckeys {
                claimed.remove(k.as_str())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Replay `from..=to` into the chain AND repersist derived state (UTXO
    /// diffs + claims) after each block. Used after a truncate+clear.
    fn replay_and_repersist(
        &self,
        chain: &mut vtorrent_node::chain::Chain,
        from: u32,
        to: u32,
    ) -> Result<()> {
        for h in from..=to {
            let block = self.get_block_at_height(h)?.ok_or_else(|| {
                StoreError::Corrupted(format!("missing block at height {} during rebuild", h))
            })?;
            let acceptance = chain.add_block(block.clone()).map_err(|e| {
                StoreError::Corrupted(format!("rebuild replay failed at height {}: {}", h, e))
            })?;
            match acceptance {
                vtorrent_node::chain::BlockAcceptance::MainChain {
                    utxos_added,
                    utxos_removed,
                    claimed_addresses,
                    ..
                } => {
                    self.append_block(&block, h, &utxos_added, &utxos_removed, &claimed_addresses)?;
                }
                // Genesis is already present in a fresh Chain::new(); skip it.
                vtorrent_node::chain::BlockAcceptance::Duplicate if h == 0 => {}
                _ => {
                    return Err(StoreError::Corrupted(format!(
                        "unexpected acceptance during rebuild at height {}",
                        h
                    )));
                }
            }
        }
        Ok(())
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
                utxo_root: [0u8; 32],
                timestamp: 1_700_000_000 + height,
                bits: vtorrent_node::genesis::GENESIS_BITS,
                nonce: height, // PoW-style: non-zero nonce for a coinbase block
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
        // Fresh stores are seeded with the deterministic genesis block.
        assert_eq!(store.best_height().unwrap(), 0);
        assert!(store.best_hash().unwrap().is_some());
        assert_eq!(store.block_count().unwrap(), 1);
        assert_eq!(store.utxo_count().unwrap(), 0);
    }

    #[test]
    fn test_open_rejects_legacy_protocol_store() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let db = Database::create(&path).unwrap();
            let write = db.begin_write().unwrap();
            write.open_table(BLOCKS).unwrap();
            write
                .open_table(HEIGHT_INDEX)
                .unwrap()
                .insert(0, "legacy-hash")
                .unwrap();
            write.open_table(UTXOS).unwrap();
            write.open_table(CLAIMED_ADDRS).unwrap();
            write.open_table(META).unwrap();
            write.commit().unwrap();
        }
        let error = BlockStore::open(path)
            .err()
            .expect("legacy store must fail");
        assert!(error.to_string().contains("protocol 3"));
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
        assert_eq!(store.block_count().unwrap(), 2); // genesis + appended
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
        assert_eq!(hashes.len(), 3); // genesis + b1 + b2
        assert_eq!(hashes[1], h1);
        assert_eq!(hashes[2], h2);
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

        // Reopen and verify data persisted (genesis backfill is idempotent).
        {
            let store = BlockStore::open(&path).unwrap();
            assert_eq!(store.best_height().unwrap(), 1);
            assert_eq!(store.block_count().unwrap(), 2); // genesis + block
            assert_eq!(store.utxo_count().unwrap(), 1);
        }
    }

    /// Regression test for the faucet-persistence bug: faucet-minted blocks
    /// must persist contiguously so a restart replays cleanly instead of
    /// truncating to genesis. Mirrors the daemon event bridge: mint into a
    /// Chain, append each block with its UTXO diff, reopen, verify replay.
    #[test]
    fn test_faucet_blocks_persist_and_replay() {
        use vtorrent_node::chain::Chain;

        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.db");

        {
            let store = BlockStore::open(&path).unwrap();
            let mut chain = Chain::new_regtest().unwrap();

            // Mint two faucet blocks (regtest faucet path).
            chain
                .mint_to_address("VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k", 1_000_000)
                .unwrap();
            let h1 = chain.best_height();
            let b1 = chain.get_block_at_height(h1).cloned().unwrap();

            chain
                .mint_to_address("VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k", 2_000_000)
                .unwrap();
            let h2 = chain.best_height();
            let b2 = chain.get_block_at_height(h2).cloned().unwrap();

            // Event-bridge behavior: append each NewBlock with its UTXO diff.
            for (height, block) in [(h1, &b1), (h2, &b2)] {
                let utxos_added: Vec<Utxo> = block
                    .transactions
                    .iter()
                    .flat_map(|tx| {
                        let txid = tx.txid();
                        tx.outputs
                            .iter()
                            .enumerate()
                            .filter_map(|(vout, _)| {
                                let vout = vout as u32;
                                chain.get_utxo(&txid, vout).map(|u| Utxo {
                                    txid,
                                    vout,
                                    value: u.value,
                                    script_pubkey: u.script_pubkey.clone(),
                                    height,
                                    timestamp: u.timestamp,
                                })
                            })
                            .collect::<Vec<Utxo>>()
                    })
                    .collect();
                let utxos_removed: Vec<([u8; 32], u32)> = block
                    .transactions
                    .iter()
                    .flat_map(|tx| tx.inputs.iter().map(|i| (i.prev_txid, i.prev_vout)))
                    .collect();
                let claimed: Vec<String> = block
                    .transactions
                    .iter()
                    .filter_map(|tx| tx.claim_address.clone())
                    .collect();
                store
                    .append_block(block, height, &utxos_added, &utxos_removed, &claimed)
                    .unwrap();
            }
            assert_eq!(store.best_height().unwrap(), 2);
        }

        // Reopen: replay must reach height 2 (pre-fix, faucet blocks were
        // never appended, replay hit a height gap and truncated to 0).
        let store = BlockStore::open(&path).unwrap();
        assert_eq!(store.best_height().unwrap(), 2);
        assert!(store.get_block_at_height(1).unwrap().is_some());
        assert!(store.get_block_at_height(2).unwrap().is_some());
    }
}
