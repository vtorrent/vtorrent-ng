use crate::{
    block::{Block, Transaction},
    consensus::{compute_pos_reward, validate_block, validate_legacy_claim},
    error::{NodeError, Result},
    genesis::{create_genesis_block, get_legacy_balance},
};
/// Blockchain state manager.
///
/// Manages the chain of blocks, UTXO set, and processes new blocks.
/// Supports chain reorganization (reorg) when a competing fork accumulates
/// more cumulative work than the current main chain.
use std::collections::{HashMap, HashSet, VecDeque};
use vtorrent_script::{Engine, Script, ScriptEnv};

/// A UTXO (unspent transaction output).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Utxo {
    pub txid: [u8; 32],
    pub vout: u32,
    pub value: u64,
    pub script_pubkey: Vec<u8>,
    pub height: u32,
    pub timestamp: u32,
}

/// A snapshot of a UTXO change for reorg rollback.
#[derive(Debug, Clone)]
enum UtxoChange {
    /// A UTXO was added (created by an output).
    Added { key: ([u8; 32], u32) },
    /// A UTXO was removed (spent by an input). Store the full UTXO for restoration.
    Removed { key: ([u8; 32], u32), utxo: Utxo },
}

/// A journal of UTXO changes for a single block, used for rollback.
#[derive(Debug, Clone)]
struct BlockJournal {
    block_hash: [u8; 32],
    height: u32,
    changes: Vec<UtxoChange>,
    claimed_addresses: Vec<String>,
}

/// The result of processing a new block.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockAcceptance {
    /// Block was appended to the main chain.
    MainChain {
        height: u32,
        /// UTXOs created by this block (for persistence).
        utxos_added: Vec<Utxo>,
        /// UTXOs spent by this block (for persistence).
        utxos_removed: Vec<([u8; 32], u32)>,
        /// Legacy addresses claimed by this block (for persistence).
        claimed_addresses: Vec<String>,
    },
    /// Block extended a fork, which then became the new main chain (reorg occurred).
    Reorg {
        old_tip: [u8; 32],
        new_tip: [u8; 32],
        depth: u32,
    },
    /// Block extended a fork that is still shorter than the main chain.
    Fork { fork_tip: [u8; 32] },
    /// Block was already known.
    Duplicate,
}

/// The blockchain state.
pub struct Chain {
    /// All blocks indexed by hash (main chain + all known forks).
    blocks: HashMap<[u8; 32], Block>,
    /// Block hash at each height on the main chain.
    height_index: Vec<[u8; 32]>,
    /// Main-chain transaction index: txid → (containing block hash, transaction offset).
    /// Fork-only transactions are intentionally excluded until their branch becomes active.
    tx_index: HashMap<[u8; 32], ([u8; 32], usize)>,
    /// UTXO set: (txid, vout) → Utxo.
    utxo_set: HashMap<([u8; 32], u32), Utxo>,
    /// Set of legacy addresses that have already been claimed.
    claimed_addresses: HashSet<String>,
    /// Per-block UTXO journals for rollback (main chain only, in order).
    journals: VecDeque<BlockJournal>,
    /// Maximum number of journals to keep (limits reorg depth).
    max_reorg_depth: u32,
    /// Cumulative work for each known block hash.
    /// Work = sum of 2^256 / target for each block in the chain.
    /// We approximate with block height for PoS (equal work per block).
    cumulative_work: HashMap<[u8; 32], u64>,
    /// Parent map: child_hash → parent_hash (all known blocks).
    parent_map: HashMap<[u8; 32], [u8; 32]>,
    /// Height of every known block (main chain + forks).
    block_heights: HashMap<[u8; 32], u32>,
}

impl Chain {
    /// Initialize a new chain with the genesis block.
    pub fn new() -> Result<Self> {
        let genesis = create_genesis_block();
        let genesis_hash = genesis.hash();

        let mut chain = Self {
            blocks: HashMap::new(),
            height_index: Vec::new(),
            tx_index: HashMap::new(),
            utxo_set: HashMap::new(),
            claimed_addresses: HashSet::new(),
            journals: VecDeque::new(),
            max_reorg_depth: 100,
            cumulative_work: HashMap::new(),
            parent_map: HashMap::new(),
            block_heights: HashMap::new(),
        };

        chain.blocks.insert(genesis_hash, genesis.clone());
        chain.height_index.push(genesis_hash);
        chain.cumulative_work.insert(genesis_hash, 1);
        chain.block_heights.insert(genesis_hash, 0);

        // Process genesis block outputs into UTXO set
        let journal = chain.apply_block_journaled(&genesis, 0)?;
        chain.index_block_transactions(genesis_hash, &genesis);
        chain.journals.push_back(journal);

        tracing::info!(
            "Chain initialized with genesis block: {}",
            hex::encode(genesis_hash)
        );
        Ok(chain)
    }

    /// Get the current best block height.
    pub fn best_height(&self) -> u32 {
        self.height_index.len().saturating_sub(1) as u32
    }

    /// Get the best block hash.
    pub fn best_hash(&self) -> Option<[u8; 32]> {
        self.height_index.last().copied()
    }

    /// Get a block by hash.
    pub fn get_block(&self, hash: &[u8; 32]) -> Option<&Block> {
        self.blocks.get(hash)
    }

    /// Get a block by height (main chain only).
    pub fn get_block_at_height(&self, height: u32) -> Option<&Block> {
        self.height_index
            .get(height as usize)
            .and_then(|hash| self.blocks.get(hash))
    }

    /// Get the active main-chain block hash at a given height.
    pub fn block_hash_at_height(&self, height: u32) -> Option<[u8; 32]> {
        self.height_index.get(height as usize).copied()
    }

    /// Look up a transaction that is currently part of the active main chain.
    ///
    /// Returns the transaction, its containing block hash, and its main-chain height.
    pub fn get_transaction(&self, txid: &[u8; 32]) -> Option<(&Transaction, [u8; 32], u32)> {
        let (block_hash, tx_offset) = self.tx_index.get(txid).copied()?;
        let height = self.block_height(&block_hash)?;
        let block = self.blocks.get(&block_hash)?;
        let tx = block.transactions.get(tx_offset)?;
        Some((tx, block_hash, height))
    }

    /// Get the UTXO for a specific output.
    pub fn get_utxo(&self, txid: &[u8; 32], vout: u32) -> Option<&Utxo> {
        self.utxo_set.get(&(*txid, vout))
    }

    /// Get all UTXOs for a specific scriptPubKey.
    pub fn get_utxos_for_script(&self, script: &[u8]) -> Vec<&Utxo> {
        self.utxo_set
            .values()
            .filter(|u| u.script_pubkey == script)
            .collect()
    }

    /// Get all UTXOs belonging to a specific address.
    pub fn get_utxos_for_address(&self, address: &str) -> Vec<Utxo> {
        let script = self.address_to_p2pkh_script(address);
        self.utxo_set
            .values()
            .filter(|u| u.script_pubkey == script)
            .cloned()
            .collect()
    }

    /// Convert a vTorrent address to a P2PKH scriptPubKey for UTXO matching.
    fn address_to_p2pkh_script(&self, address: &str) -> Vec<u8> {
        let Ok(addr) = vtorrent_core::address::Address::parse(address) else {
            return Vec::new();
        };
        let mut script = Vec::with_capacity(25);
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(&addr.hash);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG
        script
    }

    /// Get the full UTXO set.
    pub fn get_utxo_set(&self) -> &HashMap<([u8; 32], u32), Utxo> {
        &self.utxo_set
    }

    /// Get the genesis block.
    pub fn genesis_block(&self) -> &Block {
        let genesis_hash = self.height_index[0];
        self.blocks
            .get(&genesis_hash)
            .expect("genesis block always present")
    }

    pub fn is_claimed(&self, address: &str) -> bool {
        self.claimed_addresses.contains(address)
    }

    /// Get the most recent `limit` transactions from the main chain (newest first).
    ///
    /// Returns a vector of `(txid_hex, block_height, block_timestamp, tx_type_str, total_output_sats)`.
    pub fn get_recent_transactions(&self, limit: usize) -> Vec<(String, u32, u32, String, u64)> {
        let mut result = Vec::new();
        let height = self.best_height();
        let mut h = height;
        loop {
            if result.len() >= limit {
                break;
            }
            let block = match self.get_block_at_height(h) {
                Some(b) => b,
                None => break,
            };
            let ts = block.header.timestamp;
            for tx in block.transactions.iter().rev() {
                if result.len() >= limit {
                    break;
                }
                let txid = hex::encode(tx.txid());
                let tx_type = format!("{:?}", tx.tx_type);
                let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
                result.push((txid, h, ts, tx_type, total_out));
            }
            if h == 0 {
                break;
            }
            h -= 1;
        }
        result
    }

    /// Add a new block to the chain, handling forks and reorgs automatically.
    ///
    /// Returns a `BlockAcceptance` value describing what happened:
    /// - `MainChain` — block was appended to the main chain
    /// - `Reorg` — block triggered a reorganization to a longer fork
    /// - `Fork` — block extended a shorter fork (stored but not applied)
    /// - `Duplicate` — block was already known
    pub fn add_block(&mut self, block: Block) -> Result<BlockAcceptance> {
        let block_hash = block.hash();

        // Duplicate check
        if self.blocks.contains_key(&block_hash) {
            return Ok(BlockAcceptance::Duplicate);
        }

        let prev_hash = block.header.prev_block_hash;

        // Determine if this block extends the main chain or a fork
        let main_tip = self.best_hash().unwrap_or([0u8; 32]);

        if prev_hash == main_tip {
            // ── Happy path: extends the main chain ───────────────────────
            let height = self.best_height() + 1;
            let prev_block = self
                .get_block_at_height(height - 1)
                .ok_or_else(|| NodeError::Chain("Previous block not found".into()))?;

            validate_block(
                &block,
                height - 1,
                prev_block.header.timestamp,
                prev_block.header.bits,
            )?;

            let parent_work = self.cumulative_work.get(&prev_hash).copied().unwrap_or(0);
            self.cumulative_work.insert(block_hash, parent_work + 1);
            self.parent_map.insert(block_hash, prev_hash);
            self.block_heights.insert(block_hash, height);

            let journal = self.apply_block_journaled(&block, height)?;

            // Extract UTXO diff from journal for persistence
            let mut utxos_added: Vec<Utxo> = Vec::new();
            let mut utxos_removed: Vec<([u8; 32], u32)> = Vec::new();
            for change in &journal.changes {
                match change {
                    UtxoChange::Added { key } => {
                        if let Some(utxo) = self.utxo_set.get(key) {
                            utxos_added.push(utxo.clone());
                        }
                    }
                    UtxoChange::Removed { key, .. } => {
                        utxos_removed.push(*key);
                    }
                }
            }
            let claimed_addresses = journal.claimed_addresses.clone();

            self.journals.push_back(journal);

            // Trim old journals beyond max_reorg_depth
            while self.journals.len() > self.max_reorg_depth as usize {
                self.journals.pop_front();
            }

            self.index_block_transactions(block_hash, &block);
            self.blocks.insert(block_hash, block);
            self.height_index.push(block_hash);

            tracing::info!(
                "Main chain extended to height {} ({})",
                height,
                hex::encode(block_hash)
            );

            Ok(BlockAcceptance::MainChain {
                height,
                utxos_added,
                utxos_removed,
                claimed_addresses,
            })
        } else if self.blocks.contains_key(&prev_hash) {
            // ── Fork: block's parent is known but not the main tip ────────
            // Use block_heights (covers all blocks, not just main chain)
            let parent_height = self
                .block_heights
                .get(&prev_hash)
                .copied()
                .or_else(|| self.block_height(&prev_hash))
                .unwrap_or(0);
            let fork_height = parent_height + 1;
            let parent_timestamp = self.blocks.get(&prev_hash).unwrap().header.timestamp;
            let parent_bits = self.blocks.get(&prev_hash).unwrap().header.bits;

            // Validate against the fork parent
            validate_block(&block, parent_height, parent_timestamp, parent_bits)?;

            let parent_work = self.cumulative_work.get(&prev_hash).copied().unwrap_or(0);
            let fork_work = parent_work + 1;
            self.cumulative_work.insert(block_hash, fork_work);
            self.parent_map.insert(block_hash, prev_hash);
            self.block_heights.insert(block_hash, fork_height);
            self.blocks.insert(block_hash, block);

            let main_work = self.cumulative_work.get(&main_tip).copied().unwrap_or(0);

            if fork_work > main_work {
                // ── Reorg: fork is now longer than main chain ─────────────
                let old_tip = main_tip;
                self.reorganize_to(block_hash, fork_height)?;

                let depth =
                    (self.best_height() as i64 - fork_height as i64).unsigned_abs() as u32 + 1;
                tracing::warn!(
                    "Chain reorg: old tip {} → new tip {} (depth {})",
                    hex::encode(old_tip),
                    hex::encode(block_hash),
                    depth
                );

                Ok(BlockAcceptance::Reorg {
                    old_tip,
                    new_tip: block_hash,
                    depth,
                })
            } else {
                tracing::debug!(
                    "Fork block {} at height {} (work {} < main {})",
                    hex::encode(block_hash),
                    fork_height,
                    fork_work,
                    main_work
                );
                Ok(BlockAcceptance::Fork {
                    fork_tip: block_hash,
                })
            }
        } else {
            // Parent not known — orphan block, reject for now
            Err(NodeError::InvalidBlock(format!(
                "Orphan block {}: parent {} not found",
                hex::encode(block_hash),
                hex::encode(prev_hash)
            )))
        }
    }

    /// Find the height of a block on the main chain by its hash.
    pub fn block_height(&self, hash: &[u8; 32]) -> Option<u32> {
        self.height_index
            .iter()
            .position(|h| h == hash)
            .map(|i| i as u32)
    }

    /// Reorganize the main chain to make `new_tip` the best tip.
    ///
    /// Algorithm:
    /// 1. Walk back from both tips to find the common ancestor.
    /// 2. Roll back the main chain to the fork point.
    /// 3. Apply the fork chain forward to the new tip.
    fn reorganize_to(&mut self, new_tip: [u8; 32], new_tip_height: u32) -> Result<()> {
        let old_tip = self.best_hash().unwrap_or([0u8; 32]);

        // Build the path from new_tip back to genesis
        let new_chain = self.ancestors(new_tip);
        // Build the path from old_tip back to genesis
        let old_chain = self.ancestors(old_tip);

        // Find the common ancestor (first hash in both chains)
        let new_set: HashSet<[u8; 32]> = new_chain.iter().copied().collect();
        let mut fork_point = [0u8; 32];
        for hash in &old_chain {
            if new_set.contains(hash) {
                fork_point = *hash;
                break;
            }
        }

        if fork_point == [0u8; 32] {
            return Err(NodeError::Chain(
                "No common ancestor found during reorg".into(),
            ));
        }

        let fork_height = self
            .block_height(&fork_point)
            .ok_or_else(|| NodeError::Chain("Fork point not on main chain".into()))?;

        tracing::info!("Reorg: fork point at height {}", fork_height);

        // ── Step 1: Roll back main chain to fork point ────────────────────
        while self.best_height() > fork_height {
            self.rollback_one_block()?;
        }

        // ── Step 2: Apply new fork chain from fork_point+1 to new_tip ────
        // Collect blocks to apply in order (fork_point+1 ... new_tip)
        let mut to_apply: Vec<[u8; 32]> = Vec::new();
        let mut cursor = new_tip;
        while cursor != fork_point {
            to_apply.push(cursor);
            cursor = self
                .parent_map
                .get(&cursor)
                .copied()
                .ok_or_else(|| NodeError::Chain("Missing parent during reorg apply".into()))?;
        }
        to_apply.reverse(); // now in ascending order

        for (i, hash) in to_apply.iter().enumerate() {
            let height = fork_height + 1 + i as u32;
            let block = self
                .blocks
                .get(hash)
                .ok_or_else(|| {
                    NodeError::Chain(format!("Missing block {} during reorg", hex::encode(hash)))
                })?
                .clone();

            let journal = self.apply_block_journaled(&block, height)?;
            self.index_block_transactions(*hash, &block);
            self.journals.push_back(journal);
            self.height_index.push(*hash);
        }

        // Sanity check
        assert_eq!(self.best_hash(), Some(new_tip));
        assert_eq!(self.best_height(), new_tip_height);

        Ok(())
    }

    /// Walk the parent_map from `tip` back to genesis, returning hashes in
    /// descending order (tip first).
    fn ancestors(&self, mut tip: [u8; 32]) -> Vec<[u8; 32]> {
        let mut chain = Vec::new();
        let genesis = self.height_index[0];
        loop {
            chain.push(tip);
            if tip == genesis {
                break;
            }
            match self.parent_map.get(&tip) {
                Some(&parent) => tip = parent,
                None => break,
            }
        }
        chain
    }

    /// Roll back the most recent main chain block, restoring the UTXO set.
    fn rollback_one_block(&mut self) -> Result<()> {
        let journal = self
            .journals
            .pop_back()
            .ok_or_else(|| NodeError::Chain("No journal to roll back".into()))?;

        // Apply changes in reverse
        for change in journal.changes.iter().rev() {
            match change {
                UtxoChange::Added { key } => {
                    self.utxo_set.remove(key);
                }
                UtxoChange::Removed { key, utxo } => {
                    self.utxo_set.insert(*key, utxo.clone());
                }
            }
        }

        // Restore claimed addresses
        for addr in &journal.claimed_addresses {
            self.claimed_addresses.remove(addr);
        }

        self.remove_block_transactions(journal.block_hash);

        // Remove from height index
        self.height_index.pop();

        tracing::debug!(
            "Rolled back block {} at height {}",
            hex::encode(journal.block_hash),
            journal.height
        );

        Ok(())
    }

    /// Add all transactions from an active main-chain block to the transaction index.
    fn index_block_transactions(&mut self, block_hash: [u8; 32], block: &Block) {
        for (tx_offset, tx) in block.transactions.iter().enumerate() {
            self.tx_index.insert(tx.txid(), (block_hash, tx_offset));
        }
    }

    /// Remove all transactions belonging to a disconnected main-chain block.
    fn remove_block_transactions(&mut self, block_hash: [u8; 32]) {
        let txids: Vec<[u8; 32]> = self
            .blocks
            .get(&block_hash)
            .map(|block| block.transactions.iter().map(Transaction::txid).collect())
            .unwrap_or_default();
        for txid in txids {
            self.tx_index.remove(&txid);
        }
    }

    /// Apply a block's transactions to the UTXO set, recording a journal for rollback.
    fn apply_block_journaled(&mut self, block: &Block, height: u32) -> Result<BlockJournal> {
        let mut journal = BlockJournal {
            block_hash: block.hash(),
            height,
            changes: Vec::new(),
            claimed_addresses: Vec::new(),
        };

        for tx in &block.transactions {
            self.apply_transaction_journaled(tx, height, block.header.timestamp, &mut journal)?;
        }

        Ok(journal)
    }

    /// Apply a transaction to the UTXO set, recording changes in the journal.
    fn apply_transaction_journaled(
        &mut self,
        tx: &Transaction,
        height: u32,
        timestamp: u32,
        journal: &mut BlockJournal,
    ) -> Result<()> {
        let txid = tx.txid();

        // Spend inputs (except for coinbase)
        let mut total_input: u64 = 0;
        // For coinstake: the staked UTXO that satisfies the kernel check.
        let mut stake_input: Option<Utxo> = None;
        if !tx.is_coinbase() {
            // Build the sighash for script verification (SHA256d of serialised tx)
            let tx_hash = tx.txid();
            for (input_index, input) in tx.inputs.iter().enumerate() {
                let key = (input.prev_txid, input.prev_vout);
                if let Some(utxo) = self.utxo_set.remove(&key) {
                    total_input = total_input.saturating_add(utxo.value);
                    if tx.is_coinstake() && input_index == 0 {
                        stake_input = Some(utxo.clone());
                    }
                    // ── Script verification ──────────────────────────────────
                    // Skip for legacy-claim inputs (they use ECDSA message sig,
                    // not script-sig, verified separately in validate_legacy_claim).
                    if !tx.is_legacy_claim() {
                        let env = ScriptEnv {
                            tx_hash,
                            block_height: height,
                            block_time: timestamp,
                            tx_lock_time: tx.lock_time,
                        };
                        let mut engine = Engine::new(env);
                        let script_sig =
                            Script::from_bytes(input.script_sig.clone()).map_err(|e| {
                                NodeError::InvalidTransaction(format!("Invalid scriptSig: {}", e))
                            })?;
                        let script_pubkey = Script::from_bytes(utxo.script_pubkey.clone())
                            .map_err(|e| {
                                NodeError::InvalidTransaction(format!(
                                    "Invalid scriptPubKey: {}",
                                    e
                                ))
                            })?;
                        engine.execute(&script_sig, &script_pubkey).map_err(|e| {
                            NodeError::InvalidTransaction(format!(
                                "Script verification failed for input {}:{}: {}",
                                hex::encode(input.prev_txid),
                                input.prev_vout,
                                e
                            ))
                        })?;
                    }
                    journal.changes.push(UtxoChange::Removed { key, utxo });
                } else if !tx.is_legacy_claim() {
                    return Err(NodeError::InvalidTransaction(format!(
                        "Input {}:{} not found in UTXO set",
                        hex::encode(input.prev_txid),
                        input.prev_vout
                    )));
                }
            }
        }

        // Track claimed legacy addresses. The genesis distribution tx is also a
        // LegacyClaim but carries no per-address signature (`claim_address` is
        // None), so only signed, address-bearing claims are validated here.
        if tx.is_legacy_claim() {
            if let Some(addr) = &tx.claim_address {
                if self.claimed_addresses.contains(addr) {
                    return Err(NodeError::ClaimAlreadyProcessed(addr.clone()));
                }
                let snapshot_balance = get_legacy_balance(addr);
                validate_legacy_claim(tx, snapshot_balance).map_err(|e| {
                    NodeError::InvalidTransaction(format!("Invalid legacy claim: {}", e))
                })?;
                self.claimed_addresses.insert(addr.clone());
                journal.claimed_addresses.push(addr.clone());
            }
        }

        // Add outputs to UTXO set
        for (vout, output) in tx.outputs.iter().enumerate() {
            let key = (txid, vout as u32);
            self.utxo_set.insert(
                key,
                Utxo {
                    txid,
                    vout: vout as u32,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    height,
                    timestamp,
                },
            );
            journal.changes.push(UtxoChange::Added { key });
        }

        // Value conservation: a standard transaction must not create value.
        // Coinbase/coinstake mint the block reward (no inputs), and legacy
        // claims are funded by the snapshot (validated separately), so both
        // are exempt here.
        if !tx.is_coinbase() && !tx.is_coinstake() && !tx.is_legacy_claim() {
            let total_output = tx.total_output();
            if total_output > total_input {
                return Err(NodeError::InvalidTransaction(format!(
                    "Transaction creates value: inputs {} < outputs {}",
                    total_input, total_output
                )));
            }
        }

        // Coinstake reward cap: the block reward minted by a coinstake is
        // bounded by the PoS formula for the staked amount and coin age.
        // Without this, a block could mint an unbounded reward.
        if tx.is_coinstake() {
            let staked = stake_input.ok_or_else(|| {
                NodeError::InvalidTransaction("Coinstake must spend a stake input".into())
            })?;
            let coin_age = timestamp.saturating_sub(staked.timestamp);
            let max_reward = compute_pos_reward(staked.value, coin_age as u64);
            let minted = tx.total_output().saturating_sub(staked.value);
            if minted > max_reward {
                return Err(NodeError::InvalidTransaction(format!(
                    "Coinstake mints {} above the allowed reward {}",
                    minted, max_reward
                )));
            }
        }

        Ok(())
    }

    /// Legacy `add_block` that returns `Result<()>` for compatibility.
    /// Internally calls the new `add_block` and discards the acceptance type.
    pub fn add_block_simple(&mut self, block: Block) -> Result<()> {
        self.add_block(block).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType};

    fn make_block(prev_hash: [u8; 32], height: u32) -> Block {
        let transactions = vec![Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![height as u8], // unique per height
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 1_000_000,
                script_pubkey: vec![
                    0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0x88, 0xac,
                ],
            }],
            lock_time: height, // height is encoded in the first tx's lock_time
            claim_address: None,
            claim_signature: None,
        }];
        // Build a partial block to compute the merkle root
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: [0u8; 32],
                timestamp: 1_700_000_000 + height,
                bits: crate::genesis::GENESIS_BITS,
                nonce: height, // PoW-style: non-zero nonce for a coinbase block
                stake_modifier: 0u64,
            },
            transactions,
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    }

    #[test]
    fn test_chain_initialization() {
        let chain = Chain::new().expect("Chain init failed");
        assert_eq!(chain.best_height(), 0);
        assert!(chain.best_hash().is_some());
    }

    #[test]
    fn test_add_block_main_chain() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let block = make_block(genesis_hash, 1);
        let result = chain.add_block(block).unwrap();
        assert!(matches!(
            result,
            BlockAcceptance::MainChain { height: 1, .. }
        ));
        assert_eq!(chain.best_height(), 1);
    }

    #[test]
    fn test_duplicate_block_ignored() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let block = make_block(genesis_hash, 1);
        chain.add_block(block.clone()).unwrap();
        let result = chain.add_block(block).unwrap();
        assert_eq!(result, BlockAcceptance::Duplicate);
    }

    #[test]
    fn test_fork_block_stored() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // Add block 1 to main chain
        let block1 = make_block(genesis_hash, 1);
        chain.add_block(block1).unwrap();

        // Add a competing block 1 (fork)
        let mut fork_block = make_block(genesis_hash, 1);
        fork_block.header.nonce = 999; // make it different
        let result = chain.add_block(fork_block).unwrap();
        assert!(matches!(result, BlockAcceptance::Fork { .. }));
        assert_eq!(chain.best_height(), 1); // main chain unchanged
    }

    #[test]
    fn test_rollback_restores_utxo() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let utxo_count_before = chain.utxo_set.len();

        let block = make_block(genesis_hash, 1);
        chain.add_block(block).unwrap();
        assert!(chain.utxo_set.len() > utxo_count_before);

        // Roll back
        chain.rollback_one_block().unwrap();
        assert_eq!(chain.best_height(), 0);
        assert_eq!(chain.utxo_set.len(), utxo_count_before);
    }

    #[test]
    fn test_transaction_index_tracks_main_chain_and_reorgs() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let genesis_txid = chain.genesis_block().transactions[0].txid();
        assert_eq!(chain.get_transaction(&genesis_txid).unwrap().2, 0);

        // Main chain: genesis → A.
        let block_a = make_block(genesis_hash, 1);
        let txid_a = block_a.transactions[0].txid();
        chain.add_block(block_a).unwrap();
        assert_eq!(chain.get_transaction(&txid_a).unwrap().2, 1);

        // Longer fork: genesis → B → C, with B using a distinct coinbase txid.
        let mut block_b = make_block(genesis_hash, 1);
        block_b.header.nonce = 777;
        block_b.transactions[0].inputs[0].script_sig = vec![1, 42];
        block_b.header.merkle_root = block_b.compute_merkle_root();
        let txid_b = block_b.transactions[0].txid();
        let hash_b = block_b.hash();
        chain.add_block(block_b).unwrap();
        assert!(chain.get_transaction(&txid_b).is_none());

        let block_c = make_block(hash_b, 2);
        let txid_c = block_c.transactions[0].txid();
        chain.add_block(block_c).unwrap();

        assert!(chain.get_transaction(&txid_a).is_none());
        assert_eq!(chain.get_transaction(&txid_b).unwrap().2, 1);
        assert_eq!(chain.get_transaction(&txid_c).unwrap().2, 2);
    }

    #[test]
    fn test_reorg_to_longer_fork() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // Main chain: genesis → A
        let block_a = make_block(genesis_hash, 1);
        chain.add_block(block_a.clone()).unwrap();
        let tip_a = chain.best_hash().unwrap();

        // Fork: genesis → B (different nonce)
        let mut block_b = make_block(genesis_hash, 1);
        block_b.header.nonce = 999;
        chain.add_block(block_b.clone()).unwrap();
        let hash_b = block_b.hash();

        // Fork extension: B → C (makes fork longer)
        let block_c = make_block(hash_b, 2);
        let result = chain.add_block(block_c).unwrap();

        assert!(matches!(result, BlockAcceptance::Reorg { .. }));
        assert_eq!(chain.best_height(), 2);
        assert_ne!(chain.best_hash(), Some(tip_a));
    }

    #[test]
    fn test_address_to_p2pkh_script() {
        let chain = Chain::new().expect("Chain init failed");
        let script = chain.address_to_p2pkh_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
    }
}
