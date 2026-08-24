use crate::{
    block::{Block, Transaction},
    consensus::{
        check_stake_kernel, compute_pos_reward, compute_stake_modifier, validate_block,
        validate_legacy_claim,
    },
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

/// Compute the net supply change a transaction contributes to the chain.
///
/// Standard transactions are value-conserving (outputs <= inputs), so they
/// contribute nothing. Coinbase/coinstake mint the block reward. Legacy
/// claims are funded by the snapshot already counted in the genesis supply,
/// so a user claim (which carries a claim_address) contributes nothing; the
/// genesis distribution tx (claim_address = None) establishes the supply.
fn compute_supply_delta(tx: &Transaction, total_input: u64, total_output: u64) -> u64 {
    if tx.is_legacy_claim() && tx.claim_address.is_some() {
        0
    } else {
        total_output.saturating_sub(total_input)
    }
}

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
    /// Net value minted by this block (outputs created minus UTXO-set inputs
    /// spent). Used to track total supply and enforce the MAX_SUPPLY cap.
    supply_delta: u64,
}

/// The result of processing a new block.
#[derive(Debug, Clone)]
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
        /// Transactions from the abandoned chain — their inputs are spendable
        /// again and callers should consider them for mempool re-admission.
        rolled_back_txs: Vec<Transaction>,
    },
    /// Block extended a fork that is still shorter than the main chain.
    Fork { fork_tip: [u8; 32] },
    /// Block was already known.
    Duplicate,
}

impl PartialEq for BlockAcceptance {
    fn eq(&self, other: &Self) -> bool {
        use BlockAcceptance::*;
        match (self, other) {
            (MainChain { height: h1, .. }, MainChain { height: h2, .. }) => h1 == h2,
            (
                Reorg {
                    old_tip: o1,
                    new_tip: n1,
                    depth: d1,
                    ..
                },
                Reorg {
                    old_tip: o2,
                    new_tip: n2,
                    depth: d2,
                    ..
                },
            ) => o1 == o2 && n1 == n2 && d1 == d2,
            (Fork { fork_tip: f1 }, Fork { fork_tip: f2 }) => f1 == f2,
            (Duplicate, Duplicate) => true,
            _ => false,
        }
    }
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
    /// Total coin supply on the main chain (satoshis), tracked incrementally
    /// as blocks are applied and rolled back. Bounded by `MAX_SUPPLY`.
    total_supply: u64,
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
            total_supply: 0,
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

    /// Get the total coin supply on the main chain (satoshis).
    pub fn total_supply(&self) -> u64 {
        self.total_supply
    }

    /// Mint coins to an address by appending a coinbase block (regtest only).
    ///
    /// This is a development/testing primitive: it creates a PoW coinbase block
    /// paying `amount` to `address` and appends it to the main chain. It must
    /// never be reachable on mainnet — callers gate it behind a regtest flag.
    pub fn mint_to_address(&mut self, address: &str, amount: u64) -> Result<[u8; 32]> {
        use crate::block::{BlockHeader, TxInput, TxOutput, TxType};

        if amount == 0 {
            return Err(NodeError::InvalidTransaction(
                "Mint amount must be non-zero".into(),
            ));
        }
        let script = self.address_to_p2pkh_script(address);
        if script.is_empty() {
            return Err(NodeError::InvalidTransaction(format!(
                "Invalid address: {}",
                address
            )));
        }

        let prev_hash = self
            .best_hash()
            .ok_or_else(|| NodeError::Chain("Cannot mint: chain has no tip".into()))?;
        let height = self.best_height() + 1;
        let prev_timestamp = self
            .get_block_at_height(self.best_height())
            .map(|b| b.header.timestamp)
            .unwrap_or(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        let timestamp = timestamp.max(prev_timestamp + 1);

        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![height as u8],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: amount,
                script_pubkey: script,
            }],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        };
        let txid = coinbase.txid();

        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: [0u8; 32],
                timestamp,
                bits: crate::genesis::GENESIS_BITS,
                nonce: height,
                stake_modifier: compute_stake_modifier(
                    self.get_block_at_height(self.best_height())
                        .map(|b| b.header.stake_modifier)
                        .unwrap_or(0),
                    &prev_hash,
                ),
            },
            transactions: vec![coinbase],
        };
        block.header.merkle_root = block.compute_merkle_root();

        self.add_block(block)?;
        Ok(txid)
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

    /// Compute the real fee paid by a transaction using the current UTXO set.
    ///
    /// Returns `None` if any input is not in the UTXO set (unspendable or
    /// already spent). Coinbase/coinstake/legacy claims pay no fee.
    pub fn compute_tx_fee(&self, tx: &Transaction) -> Option<u64> {
        if tx.is_coinbase() || tx.is_coinstake() || tx.is_legacy_claim() {
            return Some(0);
        }
        let mut total_input: u64 = 0;
        for input in &tx.inputs {
            let utxo = self.utxo_set.get(&(input.prev_txid, input.prev_vout))?;
            total_input = total_input.saturating_add(utxo.value);
        }
        let total_output = tx.total_output();
        total_input.checked_sub(total_output)
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
    pub fn get_recent_transactions(
        &self,
        limit: usize,
    ) -> Vec<(String, u32, u32, String, u64, u64)> {
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
                let fee = self.tx_fee(tx, total_out);
                result.push((txid, h, ts, tx_type, total_out, fee));
            }
            if h == 0 {
                break;
            }
            h -= 1;
        }
        result
    }

    /// Compute the fee for a transaction as (sum of spent input values) - (sum
    /// of outputs). Returns 0 for coinbase/legacy-claim transactions (no inputs)
    /// or when an input's previous output can no longer be resolved.
    fn tx_fee(&self, tx: &Transaction, total_out: u64) -> u64 {
        if tx.is_coinbase() || tx.is_legacy_claim() {
            return 0;
        }
        let input_sum: u64 = tx
            .inputs
            .iter()
            .filter_map(|inp| self.resolve_output_value(&inp.prev_txid, inp.prev_vout))
            .sum();
        input_sum.saturating_sub(total_out)
    }

    /// Resolve the value of a previous output (txid, vout) from the main chain.
    fn resolve_output_value(&self, txid: &[u8; 32], vout: u32) -> Option<u64> {
        let (block_hash, offset) = self.tx_index.get(txid)?;
        let block = self.blocks.get(block_hash)?;
        block
            .transactions
            .get(*offset)?
            .outputs
            .get(vout as usize)
            .map(|o| o.value)
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
                prev_block.header.stake_modifier,
                prev_hash,
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
            let parent = self.blocks.get(&prev_hash).ok_or_else(|| {
                NodeError::Chain(format!("missing parent block {}", hex::encode(prev_hash)))
            })?;
            let parent_timestamp = parent.header.timestamp;
            let parent_bits = parent.header.bits;
            let parent_modifier = parent.header.stake_modifier;

            // Validate against the fork parent
            validate_block(
                &block,
                parent_height,
                parent_timestamp,
                parent_bits,
                parent_modifier,
                prev_hash,
            )?;

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
                let rolled_back_txs = self.reorganize_to(block_hash, fork_height)?;

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
                    rolled_back_txs,
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
    fn reorganize_to(
        &mut self,
        new_tip: [u8; 32],
        new_tip_height: u32,
    ) -> Result<Vec<Transaction>> {
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
        let mut rolled_back: Vec<Transaction> = Vec::new();
        while self.best_height() > fork_height {
            rolled_back.extend(self.rollback_one_block()?);
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
        if self.best_hash() != Some(new_tip) || self.best_height() != new_tip_height {
            return Err(NodeError::Chain(format!(
                "reorg verification failed: expected tip {:?} at height {}, got {:?} at height {}",
                new_tip,
                new_tip_height,
                self.best_hash(),
                self.best_height()
            )));
        }

        Ok(rolled_back)
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
    fn rollback_one_block(&mut self) -> Result<Vec<Transaction>> {
        let journal = self
            .journals
            .pop_back()
            .ok_or_else(|| NodeError::Chain("No journal to roll back".into()))?;

        // Capture the block's transactions so callers can re-inject them into
        // the mempool (their inputs become spendable again after rollback).
        let rolled_back_txs: Vec<Transaction> = self
            .blocks
            .get(&journal.block_hash)
            .map(|b| b.transactions.clone())
            .unwrap_or_default();

        // Apply changes in reverse.  The journal is consumed here — use
        // into_iter() to move UTXOs instead of cloning them.
        for change in journal.changes.into_iter().rev() {
            match change {
                UtxoChange::Added { key } => {
                    self.utxo_set.remove(&key);
                }
                UtxoChange::Removed { key, utxo } => {
                    self.utxo_set.insert(key, utxo);
                }
            }
        }

        // Restore claimed addresses (move out of the consumed journal).
        for addr in journal.claimed_addresses {
            self.claimed_addresses.remove(&addr);
        }

        // Restore total supply.
        self.total_supply = self.total_supply.saturating_sub(journal.supply_delta);

        self.remove_block_transactions(journal.block_hash);

        // Remove from height index
        self.height_index.pop();

        tracing::debug!(
            "Rolled back block {} at height {}",
            hex::encode(journal.block_hash),
            journal.height
        );

        Ok(rolled_back_txs)
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
    ///
    /// If any transaction fails validation, the partial changes already applied
    /// are rolled back so the UTXO set is left unchanged. Without this, a
    /// rejected block would permanently delete the inputs it spent.
    fn apply_block_journaled(&mut self, block: &Block, height: u32) -> Result<BlockJournal> {
        let mut journal = BlockJournal {
            block_hash: block.hash(),
            height,
            changes: Vec::new(),
            claimed_addresses: Vec::new(),
            supply_delta: 0,
        };

        // The coinstake kernel is validated against the *parent* block's stake
        // modifier (the tip at stake time). Genesis (height 0) has no parent.
        let parent_modifier = if height == 0 {
            0
        } else {
            self.blocks
                .get(&block.header.prev_block_hash)
                .map(|b| b.header.stake_modifier)
                .ok_or_else(|| {
                    NodeError::Chain("Parent block not found for stake modifier".into())
                })?
        };

        for tx in &block.transactions {
            if let Err(e) = self.apply_transaction_journaled(
                tx,
                height,
                block.header.timestamp,
                parent_modifier,
                &mut journal,
            ) {
                self.rollback_journal(&journal);
                return Err(e);
            }
        }

        // Enforce the maximum supply cap: a block may not mint value that would
        // push the total coin supply past MAX_SUPPLY.
        let new_supply = self.total_supply.saturating_add(journal.supply_delta);
        if new_supply > crate::consensus::MAX_SUPPLY {
            self.rollback_journal(&journal);
            return Err(NodeError::InvalidBlock(format!(
                "Block would exceed maximum supply: {} + {} > {}",
                self.total_supply,
                journal.supply_delta,
                crate::consensus::MAX_SUPPLY
            )));
        }
        self.total_supply = new_supply;

        Ok(journal)
    }

    /// Reverse the UTXO-set and claimed-address changes recorded in a journal.
    ///
    /// Used to undo a partially-applied block when a later transaction (or the
    /// supply cap) fails, so a rejected block leaves no trace in the UTXO set.
    fn rollback_journal(&mut self, journal: &BlockJournal) {
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
        for addr in journal.claimed_addresses.iter().rev() {
            self.claimed_addresses.remove(addr);
        }
    }

    /// Apply a transaction to the UTXO set, recording changes in the journal.
    fn apply_transaction_journaled(
        &mut self,
        tx: &Transaction,
        height: u32,
        timestamp: u32,
        parent_stake_modifier: u64,
        journal: &mut BlockJournal,
    ) -> Result<()> {
        let txid = tx.txid();

        // Spend inputs (except for coinbase)
        let mut total_input: u64 = 0;
        // For coinstake: the staked UTXO that satisfies the kernel check.
        let mut stake_input: Option<Utxo> = None;
        if !tx.is_coinbase() {
            for (input_index, input) in tx.inputs.iter().enumerate() {
                let key = (input.prev_txid, input.prev_vout);
                if let Some(utxo) = self.utxo_set.remove(&key) {
                    total_input = total_input.saturating_add(utxo.value);

                    // Extract data before moving utxo into the journal.
                    let script_bytes = utxo.script_pubkey.clone();
                    let stake_value = utxo.value;
                    let stake_height = utxo.height;
                    let stake_timestamp = utxo.timestamp;

                    // ── Script verification ──────────────────────────────────
                    if !tx.is_legacy_claim() {
                        let tx_hash = tx.sighash(input_index, &utxo.script_pubkey);
                        let env = ScriptEnv {
                            tx_hash,
                            block_height: height,
                            block_time: timestamp,
                            tx_lock_time: tx.lock_time,
                            input_sequence: input.sequence,
                        };
                        let mut engine = Engine::new(env);
                        let script_sig =
                            Script::from_bytes(input.script_sig.clone()).map_err(|e| {
                                NodeError::InvalidTransaction(format!("Invalid scriptSig: {}", e))
                            })?;
                        let script_pubkey =
                            Script::from_bytes(script_bytes.clone()).map_err(|e| {
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

                    // Save coinstake kernel data before the move.
                    if tx.is_coinstake() && input_index == 0 {
                        stake_input = Some(Utxo {
                            txid: utxo.txid,
                            vout: utxo.vout,
                            value: stake_value,
                            script_pubkey: script_bytes,
                            height: stake_height,
                            timestamp: stake_timestamp,
                        });
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
            // The stake kernel must satisfy the PoS difficulty requirement.
            // Without this check an attacker could forge a coinstake block
            // without meeting the stake target.
            if !check_stake_kernel(parent_stake_modifier, &staked, timestamp) {
                return Err(NodeError::InvalidTransaction(
                    "Coinstake kernel hash does not meet the stake target".into(),
                ));
            }
            let max_reward = compute_pos_reward(staked.value, coin_age as u64);
            let minted = tx.total_output().saturating_sub(staked.value);
            if minted > max_reward {
                return Err(NodeError::InvalidTransaction(format!(
                    "Coinstake mints {} above the allowed reward {}",
                    minted, max_reward
                )));
            }
        }

        // Net supply change: value created by this transaction (outputs minus
        // UTXO-set inputs spent). Standard transactions are value-conserving
        // (checked above), so only coinbase/coinstake rewards contribute.
        //
        // Legacy claims are funded by the snapshot embedded in the genesis
        // block, whose full value is already counted in total_supply. A user
        // claim (which carries a claim_address) mints new coins without
        // spending inputs, so excluding it here prevents the snapshot value
        // from being double-counted. The genesis distribution tx itself
        // (claim_address = None) establishes the initial supply and is kept.
        let total_output = tx.total_output();
        let supply_delta = compute_supply_delta(tx, total_input, total_output);
        journal.supply_delta = journal.supply_delta.saturating_add(supply_delta);

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

    fn make_block(prev_hash: [u8; 32], prev_stake_modifier: u64, height: u32) -> Block {
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
                stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
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
        // Genesis embeds the legacy snapshot (~11.59M VTR).
        assert_eq!(
            chain.total_supply(),
            crate::genesis::LEGACY_TOTAL_SUPPLY_SATOSHIS
        );
    }

    #[test]
    fn test_total_supply_tracks_minted_value() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let base = chain.total_supply();

        // A 1M-satoshi coinbase block mints value into the supply.
        let block = make_block(genesis_hash, 0, 1);
        chain.add_block(block).unwrap();
        assert_eq!(chain.total_supply(), base + 1_000_000);

        // Rolling the block back restores the supply.
        chain.rollback_one_block().unwrap();
        assert_eq!(chain.total_supply(), base);
    }

    #[test]
    fn test_block_exceeding_max_supply_rejected() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // A block minting 10M VTR would push total supply over the 20M cap
        // (genesis already embeds ~11.59M VTR).
        let mut block = make_block(genesis_hash, 0, 1);
        block.transactions[0].outputs[0].value = 10_000_000 * crate::consensus::COIN;
        block.header.merkle_root = block.compute_merkle_root();
        let result = chain.add_block(block);
        assert!(
            result.is_err(),
            "block exceeding MAX_SUPPLY must be rejected"
        );
        assert_eq!(chain.best_height(), 0);

        // A block minting a small amount is still accepted.
        let block = make_block(genesis_hash, 0, 1);
        chain.add_block(block).unwrap();
        assert_eq!(chain.best_height(), 1);
    }

    #[test]
    fn test_rejected_block_does_not_corrupt_utxo_set() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // Mint a spendable UTXO to a known address.
        let addr = "VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT";
        chain
            .mint_to_address(addr, 100 * crate::consensus::COIN)
            .unwrap();
        let utxos_before = chain.get_utxos_for_address(addr);
        assert_eq!(utxos_before.len(), 1);
        let utxo = utxos_before[0].clone();

        // Build a block that spends the UTXO with an invalid scriptSig, so
        // script verification fails *after* the input is removed.
        let spend = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: vec![0x00], // invalid: empty signature
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: utxo.value,
                script_pubkey: utxo.script_pubkey.clone(),
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let mut block = make_block(genesis_hash, 0, 2);
        block.transactions.push(spend);
        block.header.merkle_root = block.compute_merkle_root();

        let result = chain.add_block(block);
        assert!(
            result.is_err(),
            "block with invalid scriptSig must be rejected"
        );

        // The UTXO must still be present and unspent.
        let utxos_after = chain.get_utxos_for_address(addr);
        assert_eq!(
            utxos_after.len(),
            1,
            "rejected block must not delete the UTXO"
        );
        assert_eq!(utxos_after[0].txid, utxo.txid);
    }

    #[test]
    fn test_add_block_main_chain() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let block = make_block(genesis_hash, 0, 1);
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
        let block = make_block(genesis_hash, 0, 1);
        chain.add_block(block.clone()).unwrap();
        let result = chain.add_block(block).unwrap();
        assert_eq!(result, BlockAcceptance::Duplicate);
    }

    #[test]
    fn test_fork_block_stored() {
        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // Add block 1 to main chain
        let block1 = make_block(genesis_hash, 0, 1);
        chain.add_block(block1).unwrap();

        // Add a competing block 1 (fork)
        let mut fork_block = make_block(genesis_hash, 0, 1);
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

        let block = make_block(genesis_hash, 0, 1);
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
        let block_a = make_block(genesis_hash, 0, 1);
        let txid_a = block_a.transactions[0].txid();
        chain.add_block(block_a).unwrap();
        assert_eq!(chain.get_transaction(&txid_a).unwrap().2, 1);

        // Longer fork: genesis → B → C, with B using a distinct coinbase txid.
        let mut block_b = make_block(genesis_hash, 0, 1);
        block_b.header.nonce = 777;
        block_b.transactions[0].inputs[0].script_sig = vec![1, 42];
        block_b.header.merkle_root = block_b.compute_merkle_root();
        let txid_b = block_b.transactions[0].txid();
        let hash_b = block_b.hash();
        let b_modifier = block_b.header.stake_modifier;
        chain.add_block(block_b).unwrap();
        assert!(chain.get_transaction(&txid_b).is_none());

        let block_c = make_block(hash_b, b_modifier, 2);
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
        let block_a = make_block(genesis_hash, 0, 1);
        chain.add_block(block_a.clone()).unwrap();
        let tip_a = chain.best_hash().unwrap();

        // Fork: genesis → B (different nonce)
        let mut block_b = make_block(genesis_hash, 0, 1);
        block_b.header.nonce = 999;
        chain.add_block(block_b.clone()).unwrap();
        let hash_b = block_b.hash();

        // Fork extension: B → C (makes fork longer)
        let block_c = make_block(hash_b, block_b.header.stake_modifier, 2);
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

    #[test]
    fn test_mint_to_address() {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};

        let secp = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 9;
        let secret = SecretKey::from_slice(&key_bytes).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_height = chain.best_height();

        let txid = chain
            .mint_to_address(&address, 100 * crate::consensus::COIN)
            .expect("mint should succeed");

        assert_eq!(chain.best_height(), genesis_height + 1);
        let utxos = chain.get_utxos_for_address(&address);
        assert_eq!(utxos.len(), 1);
        assert_eq!(utxos[0].value, 100 * crate::consensus::COIN);
        assert_eq!(utxos[0].txid, txid);
    }

    #[test]
    fn test_mint_to_address_rejects_invalid() {
        let mut chain = Chain::new().expect("Chain init failed");
        assert!(chain.mint_to_address("not-an-address", 1000).is_err());
        assert!(chain
            .mint_to_address("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT", 0)
            .is_err());
    }

    /// Build a coinbase block paying to a specific P2PKH script.
    fn make_coinbase_to_script(
        prev_hash: [u8; 32],
        prev_stake_modifier: u64,
        height: u32,
        timestamp: u32,
        script_pubkey: Vec<u8>,
        value: u64,
    ) -> Block {
        let transactions = vec![Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![height as u8],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value,
                script_pubkey,
            }],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        }];
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: prev_hash,
                merkle_root: [0u8; 32],
                timestamp,
                bits: crate::genesis::GENESIS_BITS,
                nonce: height,
                stake_modifier: compute_stake_modifier(prev_stake_modifier, &prev_hash),
            },
            transactions,
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    }

    #[test]
    fn test_pos_block_with_signed_coinstake_accepted() {
        use crate::staking::StakingEngine;
        use secp256k1::{PublicKey, Secp256k1, SecretKey};

        // Generate a staking key pair.
        let secp = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 42;
        let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
        let wif = key.to_wif(198);
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();

        // Fund the staking address with a large coinbase UTXO at an old
        // timestamp so the coin is mature (age >= MIN_STAKE_AGE).
        let funding_ts = 1_700_000_001u32;
        let script = chain.address_to_p2pkh_script(&address);
        let funding_block = make_coinbase_to_script(
            genesis_hash,
            0,
            1,
            funding_ts,
            script.clone(),
            100 * crate::consensus::COIN,
        );
        chain.add_block(funding_block).unwrap();
        assert_eq!(chain.best_height(), 1);

        let utxos = chain.get_utxos_for_address(&address);
        assert!(!utxos.is_empty(), "staking address must have a UTXO");

        // Search for a timestamp whose kernel hash satisfies the target.
        let engine = StakingEngine::with_wif(address.clone(), wif);
        let prev_stake_modifier = chain
            .get_block_at_height(1)
            .map(|b| b.header.stake_modifier)
            .unwrap_or(0);
        let mut stake_block = None;
        let mut ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
        for _ in 0..10_000 {
            if let Some(block) = engine.build_stake_block(
                chain.best_hash().unwrap(),
                prev_stake_modifier,
                2,
                ts,
                utxos.clone(),
                vec![],
            ) {
                stake_block = Some(block);
                break;
            }
            ts += 1;
        }
        let stake_block = stake_block.expect("should find a valid stake kernel");

        // The coinstake input must be signed (scriptSig is a real P2PKH sig).
        let coinstake = &stake_block.transactions[0];
        assert_eq!(coinstake.tx_type, TxType::Coinstake);
        assert!(
            coinstake.inputs[0].script_sig.len() > 2,
            "coinstake input must carry a signature"
        );

        // The chain must accept the block: kernel check + script verification.
        let result = chain.add_block(stake_block).unwrap();
        assert!(matches!(
            result,
            BlockAcceptance::MainChain { height: 2, .. }
        ));
        assert_eq!(chain.best_height(), 2);
    }

    #[test]
    fn test_pos_block_with_bad_kernel_rejected() {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};

        let secp = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 7;
        let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
        let secret = SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().expect("Chain init failed");
        let genesis_hash = chain.best_hash().unwrap();
        let funding_ts = 1_700_000_001u32;
        let script = chain.address_to_p2pkh_script(&address);
        let funding_block = make_coinbase_to_script(
            genesis_hash,
            0,
            1,
            funding_ts,
            script.clone(),
            100 * crate::consensus::COIN,
        );
        chain.add_block(funding_block).unwrap();
        let utxos = chain.get_utxos_for_address(&address);

        // Build a coinstake whose kernel does NOT satisfy the target by
        // forging it directly (bypassing the engine's kernel search).
        let utxo = utxos[0].clone();
        let prev_stake_modifier = chain
            .get_block_at_height(1)
            .map(|b| b.header.stake_modifier)
            .unwrap_or(0);
        let mut bad_ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
        while check_stake_kernel(prev_stake_modifier, &utxo, bad_ts) {
            bad_ts += 1;
        }
        let reward = compute_pos_reward(utxo.value, (bad_ts - utxo.timestamp) as u64);
        let coinstake = Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: vec![0x51], // OP_TRUE — no real signature
                sequence: u32::MAX,
            }],
            outputs: vec![
                TxOutput {
                    value: 0,
                    script_pubkey: Vec::new(),
                },
                TxOutput {
                    value: utxo.value + reward,
                    script_pubkey: script.clone(),
                },
            ],
            lock_time: 2,
            claim_address: None,
            claim_signature: None,
        };

        let parent_hash = chain.best_hash().unwrap();
        let mut block = Block {
            header: BlockHeader {
                version: 2,
                prev_block_hash: parent_hash,
                merkle_root: [0u8; 32],
                timestamp: bad_ts,
                bits: crate::genesis::GENESIS_BITS,
                nonce: 0, // PoS block
                stake_modifier: compute_stake_modifier(prev_stake_modifier, &parent_hash),
            },
            transactions: vec![coinstake],
        };
        block.header.merkle_root = block.compute_merkle_root();

        // The block must be rejected: the kernel check fails.
        let result = chain.add_block(block);
        assert!(result.is_err(), "block with bad kernel must be rejected");
        assert_eq!(chain.best_height(), 1);
    }

    #[test]
    fn test_legacy_claim_does_not_double_count_supply() {
        // A user legacy claim (claim_address = Some) is funded by the snapshot
        // already counted in genesis, so it must not add to the supply.
        let claim_tx = Transaction {
            version: 1,
            tx_type: TxType::LegacyClaim,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 500 * crate::consensus::COIN,
                script_pubkey: vec![0x76, 0xa9, 0x14],
            }],
            lock_time: 1,
            claim_address: Some("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string()),
            claim_signature: Some(vec![0u8; 65]),
        };
        assert_eq!(
            compute_supply_delta(&claim_tx, 0, claim_tx.total_output()),
            0
        );

        // The genesis distribution tx (claim_address = None) establishes the
        // initial supply and must still count.
        let genesis_dist = Transaction {
            claim_address: None,
            ..claim_tx.clone()
        };
        assert_eq!(
            compute_supply_delta(&genesis_dist, 0, genesis_dist.total_output()),
            genesis_dist.total_output()
        );

        // A coinbase mints its reward into the supply.
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 1_000_000,
                script_pubkey: vec![0x76, 0xa9, 0x14],
            }],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        assert_eq!(
            compute_supply_delta(&coinbase, 0, coinbase.total_output()),
            1_000_000
        );
    }

    /// Full end-to-end test: coinbase → P2PKH spend → chain acceptance.
    ///
    /// Exercises the complete pipeline:
    ///   keypair generation → address derivation → scriptPubKey construction
    ///   → coinbase minting → manual signing → sighash →
    ///   script engine verification → UTXO set update.
    #[test]
    fn test_p2pkh_spend_through_chain() {
        use secp256k1::{PublicKey, Secp256k1, SecretKey};

        // 1. Generate a keypair and derive the vTorrent address.
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[0xAB; 32]).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        // 2. Mint a coinbase UTXO to the address.
        let mut chain = Chain::new().expect("Chain init failed");
        chain
            .mint_to_address(&address, 10 * crate::consensus::COIN)
            .unwrap();
        let funded_hash = chain.best_hash().unwrap();
        assert_eq!(
            chain.best_height(),
            1,
            "mint must advance chain to height 1"
        );
        let utxos = chain.get_utxos_for_address(&address);
        assert_eq!(utxos.len(), 1, "must have exactly one funded UTXO");
        let utxo = &utxos[0];
        let spend_value = 5 * crate::consensus::COIN;

        // 3. Build the recipient address.
        let (_, recipient_addr) = {
            let s = Secp256k1::new();
            let sk = SecretKey::from_slice(&[0xCD; 32]).unwrap();
            let pk = PublicKey::from_secret_key(&s, &sk);
            let a = vtorrent_core::address::Address::from_pubkey(&pk, true, 70);
            (sk, a.to_string())
        };

        let script_pubkey = chain.address_to_p2pkh_script(&address);

        // 4. Build the spending transaction manually.
        //    Height is encoded in the first tx's lock_time (chain is at height 1 after mint).
        let spend_height = 2u32;
        let mut tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: utxo.txid,
                prev_vout: utxo.vout,
                script_sig: Vec::new(), // filled below
                sequence: 0xffff_fffe,
            }],
            outputs: vec![TxOutput {
                value: spend_value,
                script_pubkey: chain.address_to_p2pkh_script(&recipient_addr),
            }],
            lock_time: spend_height,
            claim_address: None,
            claim_signature: None,
        };

        // 5. Sign the input over the UTXO's scriptPubKey.
        let sighash = tx.sighash(0, &script_pubkey);
        let msg = secp256k1::Message::from_digest(sighash);
        let sig = secp.sign_ecdsa(&msg, &secret);
        let mut der = sig.serialize_der().to_vec();
        der.push(0x01); // SIGHASH_ALL
        let pubkey_bytes = pubkey.serialize();

        // Build scriptSig: <len><sig><len><pubkey>
        let mut script_sig = Vec::with_capacity(1 + der.len() + 1 + pubkey_bytes.len());
        script_sig.push(der.len() as u8);
        script_sig.extend_from_slice(&der);
        script_sig.push(pubkey_bytes.len() as u8);
        script_sig.extend_from_slice(&pubkey_bytes);
        tx.inputs[0].script_sig = script_sig;

        // 6. Verify the scriptSig through the script engine directly.
        let env = vtorrent_script::ScriptEnv {
            tx_hash: tx.sighash(0, &script_pubkey),
            block_height: 2,
            block_time: 1_700_000_002,
            tx_lock_time: tx.lock_time,
            input_sequence: 0xffff_fffe,
        };
        let mut engine = vtorrent_script::Engine::new(env);
        let sig_script =
            vtorrent_script::Script::from_bytes(tx.inputs[0].script_sig.clone()).unwrap();
        let pk_script = vtorrent_script::Script::from_bytes(script_pubkey.clone()).unwrap();
        engine.execute(&sig_script, &pk_script).unwrap();

        // 7. Wrap in a block and add to the chain — exercises full validation.
        //    Every block must start with a coinbase transaction.
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![TxInput {
                prev_txid: [0u8; 32],
                prev_vout: 0xffffffff,
                script_sig: vec![2u8], // height = 2
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: crate::consensus::COIN, // block reward
                script_pubkey: vec![
                    0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0x88, 0xac,
                ],
            }],
            lock_time: spend_height,
            claim_address: None,
            claim_signature: None,
        };

        let funded_stake_modifier = chain
            .get_block_at_height(1)
            .map(|b| b.header.stake_modifier)
            .unwrap_or(0);
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                prev_block_hash: funded_hash,
                merkle_root: [0u8; 32],
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as u32
                    + 1,
                bits: crate::genesis::GENESIS_BITS,
                nonce: 42,
                stake_modifier: compute_stake_modifier(funded_stake_modifier, &funded_hash),
            },
            transactions: vec![coinbase, tx],
        };
        block.header.merkle_root = block.compute_merkle_root();

        let acceptance = chain.add_block(block).unwrap();
        assert!(
            matches!(acceptance, super::BlockAcceptance::MainChain { .. }),
            "P2PKH spend block must be accepted on main chain"
        );

        // 8. Verify UTXO set: old UTXO consumed, new UTXO created for recipient.
        let sender_utxos = chain.get_utxos_for_address(&address);
        assert_eq!(sender_utxos.len(), 0, "spent UTXO must be consumed");

        let recipient_utxos = chain.get_utxos_for_address(&recipient_addr);
        assert_eq!(recipient_utxos.len(), 1, "recipient must have one UTXO");
        assert_eq!(recipient_utxos[0].value, spend_value);
    }

    /// Multi-block staking: produce 3 consecutive PoS blocks and verify
    /// chain state, UTXO set, and staking reward accumulation.
    #[test]
    fn test_multi_block_staking() {
        use crate::staking::StakingEngine;
        use secp256k1::{PublicKey, Secp256k1};

        let secp = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 99;
        let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
        let wif = key.to_wif(198);
        let secret = secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().unwrap();
        let genesis_hash = chain.best_hash().unwrap();

        // Fund the staking address with a coinbase at an old timestamp.
        let funding_ts = 1_700_000_001u32;
        let script = chain.address_to_p2pkh_script(&address);
        let funding_block = make_coinbase_to_script(
            genesis_hash,
            0,
            1,
            funding_ts,
            script,
            100 * crate::consensus::COIN,
        );
        chain.add_block(funding_block).unwrap();

        let engine = StakingEngine::with_wif(address.clone(), wif);
        let mut prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;

        for expected_height in 2..=4 {
            let utxos = chain.get_utxos_for_address(&address);
            assert!(
                !utxos.is_empty(),
                "must have UTXOs at height {}",
                expected_height
            );

            let mut stake_block = None;
            let mut ts = chain
                .get_block_at_height(expected_height - 1)
                .unwrap()
                .header
                .timestamp
                + crate::consensus::MIN_STAKE_AGE as u32;
            for _ in 0..100_000 {
                if let Some(block) = engine.build_stake_block(
                    chain.best_hash().unwrap(),
                    prev_modifier,
                    expected_height,
                    ts,
                    utxos.clone(),
                    vec![],
                ) {
                    stake_block = Some(block);
                    break;
                }
                ts += 1;
            }
            let block = stake_block.expect("should find stake kernel");
            prev_modifier = block.header.stake_modifier;
            let result = chain.add_block(block).unwrap();
            assert!(
                matches!(result, super::BlockAcceptance::MainChain { height, .. } if height == expected_height)
            );
        }

        assert_eq!(chain.best_height(), 4);
        // Staking address should still have UTXOs (stake return + rewards).
        let final_utxos = chain.get_utxos_for_address(&address);
        assert!(!final_utxos.is_empty());
    }

    /// PoS block with mempool transactions: verify pending txs are included
    /// in the block assembled by the staking engine.
    #[test]
    fn test_pos_block_includes_mempool_txs() {
        use crate::staking::StakingEngine;
        use secp256k1::{PublicKey, Secp256k1};

        let secp = Secp256k1::new();
        let mut key_bytes = [0u8; 32];
        key_bytes[31] = 55;
        let key = vtorrent_core::keys::PrivateKey::from_bytes(key_bytes, true).unwrap();
        let wif = key.to_wif(198);
        let secret = secp256k1::SecretKey::from_slice(key.as_bytes()).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);
        let addr = vtorrent_core::address::Address::from_pubkey(&pubkey, true, 70);
        let address = addr.to_string();

        let mut chain = Chain::new().unwrap();
        let genesis_hash = chain.best_hash().unwrap();

        let funding_ts = 1_700_000_001u32;
        let script = chain.address_to_p2pkh_script(&address);
        let funding_block = make_coinbase_to_script(
            genesis_hash,
            0,
            1,
            funding_ts,
            script,
            100 * crate::consensus::COIN,
        );
        chain.add_block(funding_block).unwrap();
        assert_eq!(chain.best_height(), 1);

        let utxos = chain.get_utxos_for_address(&address);
        let engine = StakingEngine::with_wif(address.clone(), wif);
        let prev_modifier = chain.get_block_at_height(1).unwrap().header.stake_modifier;

        // Create a "mempool" transaction (a dummy tx that the engine should include).
        let dummy_tx = crate::block::Transaction {
            version: 1,
            tx_type: crate::block::TxType::Standard,
            inputs: vec![crate::block::TxInput {
                prev_txid: [0xff; 32],
                prev_vout: 0,
                script_sig: vec![],
                sequence: 0,
            }],
            outputs: vec![crate::block::TxOutput {
                value: 5000,
                script_pubkey: {
                    let mut s = vec![0x76, 0xa9, 0x14];
                    s.extend([0xaa; 20]);
                    s
                },
            }],
            lock_time: 2,
            claim_address: None,
            claim_signature: None,
        };

        let mut stake_block = None;
        let mut ts = funding_ts + crate::consensus::MIN_STAKE_AGE as u32;
        for _ in 0..100_000 {
            if let Some(block) = engine.build_stake_block(
                chain.best_hash().unwrap(),
                prev_modifier,
                2,
                ts,
                utxos.clone(),
                vec![dummy_tx.clone()],
            ) {
                stake_block = Some(block);
                break;
            }
            ts += 1;
        }
        let block = stake_block.expect("should find stake kernel");
        // Block should have the coinstake + the mempool tx.
        assert!(
            block.transactions.len() >= 2,
            "block should include mempool txs, got {}",
            block.transactions.len()
        );
        // Verify the mempool tx is in the block.
        let has_mempool = block.transactions.iter().any(|tx| {
            tx.tx_type == crate::block::TxType::Standard
                && tx.outputs.iter().any(|o| o.value == 5000)
        });
        assert!(has_mempool, "mempool tx with 5000 sats must be in block");

        // Verify the coinstake is valid (first tx in block).
        assert_eq!(
            block.transactions[0].tx_type,
            crate::block::TxType::Coinstake
        );
    }
}
