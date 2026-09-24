mod chain_reorg;

use crate::{
    block::{Block, BlockHeader, Transaction},
    consensus::{compute_stake_modifier, validate_block_inner, validate_legacy_claim},
    error::{NodeError, Result},
    genesis::create_genesis_block,
};
/// Blockchain state manager.
///
/// Manages the chain of blocks, UTXO set, and processes new blocks.
/// Supports chain reorganization (reorg) when a competing fork accumulates
/// more cumulative work than the current main chain.
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use vtorrent_core::time::now_timestamp_u32;

/// Default number of most-recent block *bodies* retained in memory.
///
/// Must exceed `max_reorg_depth` (100) with margin, and cover the maximum
/// event-bridge lag so the daemon's store reconciliation stays contiguous.
pub const DEFAULT_BLOCK_BODY_CACHE: usize = 1_000;

/// Maximum number of off-main-chain (fork) block bodies retained in memory.
/// Bounds connected-fork spam; lowest-cumulative-work *leaf* forks are evicted.
pub const MAX_FORK_BODIES: usize = 256;

/// Source of block bodies pruned from the in-memory [`Chain`].
///
/// Implemented by the persistent store (which owns every block body) and set on
/// the chain at startup by the daemon/tauri layer. Keeps `vtorrent-node` free of
/// a dependency on the store crate. Implementations must be cheap enough not to
/// stall the caller — the fallback is invoked while callers may hold the chain
/// lock, so it must not perform unbounded work.
pub trait BlockBodySource: Send + Sync {
    /// Return the full block for `hash`, if available.
    fn block_body(&self, hash: &[u8; 32]) -> Option<Block>;
    /// Return the full main-chain block at `height`, if available.
    fn block_body_at_height(&self, height: u32) -> Option<Block>;
}

/// Current Unix timestamp as u32 (valid until year 2106).
#[allow(clippy::cast_possible_truncation)]
/// Compute the net supply change a transaction contributes to the chain.
///
/// Standard transactions are value-conserving (outputs <= inputs), so they
/// contribute nothing. Coinbase/coinstake mint the block reward. Legacy
/// claims are funded by the snapshot already counted in the genesis supply,
/// so a user claim (which carries a claim_address) contributes nothing; the
/// genesis distribution tx (claim_address = None) establishes the supply.
pub(crate) fn compute_supply_delta(tx: &Transaction, total_input: u64, total_output: u64) -> u64 {
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
        /// Abandoned main-chain blocks, tip first, with disk-undo data.
        rolled_back_blocks: Vec<RolledBackBlock>,
        /// Fork blocks now part of the main chain, ascending, with disk data.
        applied_fork_blocks: Vec<AppliedForkBlock>,
    },
    /// Block extended a fork that is still shorter than the main chain.
    Fork { fork_tip: [u8; 32] },
    /// Block was already known.
    Duplicate,
}

/// A main-chain block that was abandoned during a reorg, with the data the
/// persistence layer needs to undo its effects on disk.
#[derive(Debug, Clone)]
pub struct RolledBackBlock {
    pub hash: [u8; 32],
    pub height: u32,
    /// UTXOs created by the abandoned block (must be removed from disk).
    pub utxos_to_remove: Vec<([u8; 32], u32)>,
    /// UTXOs spent by the abandoned block (must be restored to disk).
    pub utxos_to_restore: Vec<Utxo>,
    /// Snapshot claims made by the abandoned block (must be un-claimed).
    pub claimed_to_remove: Vec<String>,
}

/// A fork-chain block applied during a reorg, with the data the persistence
/// layer needs to record it on disk.
#[derive(Debug, Clone)]
pub struct AppliedForkBlock {
    pub block: Block,
    pub height: u32,
    pub utxos_added: Vec<Utxo>,
    pub utxos_removed: Vec<([u8; 32], u32)>,
    pub claimed_addresses: Vec<String>,
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
    /// Block bodies indexed by hash (main chain + all known forks). Bounded to
    /// the most recent `block_body_cache` heights; older bodies are pruned and
    /// served on demand by `body_source` (see [`Chain::get_block_owned`]).
    blocks: HashMap<[u8; 32], Block>,
    /// Headers for every known block (main chain + forks). Retained even after a
    /// block's body is pruned, because consensus reads only header fields
    /// (`stake_modifier`, `timestamp`, `bits`, `utxo_root`) from ancestors.
    headers: HashMap<[u8; 32], BlockHeader>,
    /// Maximum number of most-recent block bodies retained in `blocks`.
    /// `usize::MAX` disables pruning (keeps every body, the pre-pruning
    /// behaviour). Heights 0 and 1 are always retained regardless.
    block_body_cache: usize,
    /// Optional source for pruned bodies (usually the persistent store).
    body_source: Option<Arc<dyn BlockBodySource>>,
    /// Block hash at each height on the main chain.
    height_index: Vec<[u8; 32]>,
    /// Main-chain transaction index: txid → (containing block hash, transaction offset).
    /// Fork-only transactions are intentionally excluded until their branch becomes active.
    tx_index: HashMap<[u8; 32], ([u8; 32], usize)>,
    /// UTXO set: (txid, vout) → Utxo.
    utxo_set: BTreeMap<([u8; 32], u32), Utxo>,
    /// Set of legacy addresses that have already been claimed.
    claimed_addresses: HashSet<String>,
    /// Per-block UTXO journals for rollback (main chain only, in order).
    journals: VecDeque<chain_reorg::BlockJournal>,
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
    /// Sum of stakeable UTXO values (satoshis), tracked incrementally. Used as
    /// the denominator of the v2 stake-kernel probability so a staker's block
    /// share equals its stake share.
    total_staked: u64,
    allow_pow_test_blocks: bool,
    min_stake_age: u64,
    max_stake_age: u64,
}

impl Chain {
    /// Initialize a new chain with the genesis block.
    pub fn new() -> Result<Self> {
        let genesis = create_genesis_block();
        let genesis_hash = genesis.hash();

        let mut chain = Self {
            blocks: HashMap::new(),
            headers: HashMap::new(),
            // Pruning is opt-in: library/`Node::new` callers with no body source
            // retain every body (pre-pruning behaviour). The daemon passes
            // `DEFAULT_BLOCK_BODY_CACHE` into `load_into_*_with_cache`.
            block_body_cache: usize::MAX,
            body_source: None,
            height_index: Vec::new(),
            tx_index: HashMap::new(),
            utxo_set: BTreeMap::new(),
            claimed_addresses: HashSet::new(),
            journals: VecDeque::new(),
            max_reorg_depth: 100,
            cumulative_work: HashMap::new(),
            parent_map: HashMap::new(),
            block_heights: HashMap::new(),
            total_supply: 0,
            total_staked: 0,
            allow_pow_test_blocks: cfg!(test),
            min_stake_age: crate::consensus::MIN_STAKE_AGE,
            max_stake_age: crate::consensus::MAX_STAKE_AGE,
        };

        chain.blocks.insert(genesis_hash, genesis.clone());
        chain.headers.insert(genesis_hash, genesis.header.clone());
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

    pub fn new_regtest() -> Result<Self> {
        let mut chain = Self::new()?;
        chain.allow_pow_test_blocks = true;
        Ok(chain)
    }

    /// Initialize a regtest chain with accelerated stake maturity rules.
    pub fn new_regtest_fast() -> Result<Self> {
        let mut chain = Self::new_regtest()?;
        chain.min_stake_age = crate::consensus::REGTEST_FAST_MIN_STAKE_AGE;
        chain.max_stake_age = crate::consensus::REGTEST_FAST_MAX_STAKE_AGE;
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

    /// Total stakeable UTXO value (satoshis) on the main chain. This is the
    /// denominator of the v2 stake-kernel probability.
    pub fn total_staked(&self) -> u64 {
        self.total_staked
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
            .get_header_at_height(self.best_height())
            .map(|h| h.timestamp)
            .unwrap_or(0);
        let timestamp = now_timestamp_u32();
        let timestamp = timestamp.max(prev_timestamp.saturating_add(1));

        // Supply cap: the faucet is regtest-only, but mint_to_address is a
        // public Chain API — enforce the consensus supply ceiling regardless.
        if self.total_supply.saturating_add(amount) > crate::consensus::MAX_SUPPLY {
            return Err(NodeError::Chain(format!(
                "Mint would exceed maximum supply: {} + {} > {}",
                self.total_supply,
                amount,
                crate::consensus::MAX_SUPPLY
            )));
        }

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
                utxo_root: [0u8; 32],
                timestamp,
                bits: crate::genesis::GENESIS_BITS,
                nonce: height,
                stake_modifier: compute_stake_modifier(
                    self.get_header_at_height(self.best_height())
                        .map(|h| h.stake_modifier)
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

    /// Apply the height-1 bootstrap claim block.
    ///
    /// Genesis has no stakeable UTXO, so no coinstake can be produced and
    /// staking can never start (T3). The first legacy claim is mined directly
    /// into a height-1 PoS block whose only transaction is the claim; its P2PKH
    /// output seeds `total_staked` and unblocks normal PoS from height 2.
    ///
    /// One-shot: only valid while the chain is at genesis.
    pub fn apply_bootstrap_claim(&mut self, claim: Transaction) -> Result<[u8; 32]> {
        use crate::block::{BlockHeader, TxType};

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
        // The block height is encoded in the first transaction's lock_time, so
        // the bootstrap claim must carry lock_time = 1. Require it rather than
        // rewriting it, so the caller's txid matches the mined transaction.
        if claim.lock_time != 1 {
            return Err(NodeError::InvalidTransaction(
                "Bootstrap claim must have lock_time = 1 (the block height)".into(),
            ));
        }

        let genesis = self.genesis_block().clone();
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
                stake_modifier: compute_stake_modifier(
                    genesis.header.stake_modifier,
                    &genesis_hash,
                ),
            },
            transactions: vec![claim.clone()],
        };
        block.header.merkle_root = block.compute_merkle_root();

        // Post-apply UTXO root: the genesis UTXO set plus the claim's outputs
        // (the claim has no inputs). Must match the journal or `add_block`
        // rejects the PoS block on the utxo_root check.
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

    /// Get a block by hash.
    pub fn get_block(&self, hash: &[u8; 32]) -> Option<&Block> {
        self.blocks.get(hash)
    }

    /// Get a block by height from memory (main chain only).
    ///
    /// Returns `None` for a pruned body; use [`Chain::get_block_at_height_owned`]
    /// to fall back to the `body_source`.
    pub fn get_block_at_height(&self, height: u32) -> Option<&Block> {
        self.height_index
            .get(height as usize)
            .and_then(|hash| self.blocks.get(hash))
    }

    /// Get the active main-chain block hash at a given height.
    pub fn block_hash_at_height(&self, height: u32) -> Option<[u8; 32]> {
        self.height_index.get(height as usize).copied()
    }

    /// Get the retained header for a block by hash (main chain or fork).
    pub fn get_header(&self, hash: &[u8; 32]) -> Option<&BlockHeader> {
        self.headers.get(hash)
    }

    /// Get the UTXO commitment root at a given height (main chain only).
    ///
    /// Reads the retained header, so it works for pruned heights without a
    /// store round-trip.
    pub fn utxo_root_at_height(&self, height: u32) -> Option<[u8; 32]> {
        self.get_header_at_height(height).map(|h| h.utxo_root)
    }

    /// Header at a main-chain height (retained for every block).
    pub fn get_header_at_height(&self, height: u32) -> Option<&BlockHeader> {
        self.height_index
            .get(height as usize)
            .and_then(|hash| self.headers.get(hash))
    }

    /// Current UTXO commitment root (tip's `utxo_root`).
    pub fn current_utxo_root(&self) -> Option<[u8; 32]> {
        self.get_header_at_height(self.best_height())
            .map(|h| h.utxo_root)
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

    /// Look up a transaction, falling back to the `body_source` for pruned
    /// bodies. Owned variant of [`Chain::get_transaction`].
    pub fn get_transaction_owned(
        &self,
        txid: &[u8; 32],
    ) -> Option<(Transaction, [u8; 32], u32)> {
        let (block_hash, tx_offset) = self.tx_index.get(txid).copied()?;
        let height = self.block_height(&block_hash)?;
        let block = self.get_block_owned(&block_hash)?;
        let tx = block.transactions.get(tx_offset)?.clone();
        Some((tx, block_hash, height))
    }

    /// Get a block body by hash, falling back to the `body_source` if the body
    /// has been pruned from memory.
    pub fn get_block_owned(&self, hash: &[u8; 32]) -> Option<Block> {
        if let Some(block) = self.blocks.get(hash) {
            return Some(block.clone());
        }
        self.body_source
            .as_ref()
            .and_then(|source| source.block_body(hash))
    }

    /// Get a main-chain block body by height, falling back to the `body_source`
    /// if the body has been pruned from memory.
    pub fn get_block_at_height_owned(&self, height: u32) -> Option<Block> {
        if let Some(block) = self.get_block_at_height(height) {
            return Some(block.clone());
        }
        self.body_source
            .as_ref()
            .and_then(|source| source.block_body_at_height(height))
    }

    /// Set how many recent block bodies to retain in memory (`usize::MAX` =
    /// keep all). Takes effect on the next accepted block.
    pub fn set_block_body_cache(&mut self, cache: usize) {
        self.block_body_cache = cache;
    }

    /// Shrink the reorg-journal window. Test-only: lets body-pruning tests run
    /// with a small cache (journal-referenced bodies are always pinned).
    #[cfg(test)]
    pub(crate) fn set_max_reorg_depth(&mut self, depth: u32) {
        self.max_reorg_depth = depth;
    }

    /// Number of block bodies currently retained in memory (diagnostics/tests).
    pub fn body_count(&self) -> usize {
        self.blocks.len()
    }

    /// Attach a source for pruned block bodies (usually the persistent store).
    pub fn set_body_source(&mut self, source: Arc<dyn BlockBodySource>) {
        self.body_source = Some(source);
    }

    /// Drop block bodies outside the retention window.
    ///
    /// Main-chain bodies below the window are removed one height at a time
    /// (amortized O(1) per accepted block); off-main-chain fork bodies are
    /// bounded separately by [`MAX_FORK_BODIES`]. Headers and all index maps are
    /// retained. Heights 0 and 1 are pinned, and any block still referenced by a
    /// reorg journal is kept.
    fn prune_bodies(&mut self) {
        if self.block_body_cache != usize::MAX {
            let len = self.height_index.len();
            if len > self.block_body_cache {
                let victim_height = (len - 1 - self.block_body_cache) as u32;
                if victim_height >= 2 {
                    if let Some(hash) = self.height_index.get(victim_height as usize).copied() {
                        let journalled = self
                            .journals
                            .iter()
                            .any(|journal| journal.block_hash == hash);
                        if !journalled {
                            self.blocks.remove(&hash);
                        }
                    }
                }
            }
        }
        self.prune_fork_bodies();
    }

    /// Number of main-chain bodies the window permits (plus pinned heights).
    fn retained_main_bodies(&self) -> usize {
        let len = self.height_index.len();
        if self.block_body_cache == usize::MAX {
            len
        } else {
            len.min(self.block_body_cache.max(2))
        }
    }

    /// Evict lowest-cumulative-work fork *leaves* until the fork body count is
    /// within [`MAX_FORK_BODIES`]. Only leaves are removed, so no stored
    /// descendant chain is broken.
    fn prune_fork_bodies(&mut self) {
        let bound = self.retained_main_bodies() + MAX_FORK_BODIES;
        while self.blocks.len() > bound {
            let main: HashSet<[u8; 32]> = self.height_index.iter().copied().collect();
            let has_child: HashSet<[u8; 32]> = self.parent_map.values().copied().collect();
            let victim = self
                .blocks
                .keys()
                .filter(|hash| !main.contains(*hash) && !has_child.contains(*hash))
                .min_by_key(|hash| self.cumulative_work.get(*hash).copied().unwrap_or(u64::MAX))
                .copied();
            match victim {
                Some(hash) => {
                    self.blocks.remove(&hash);
                    self.headers.remove(&hash);
                    self.parent_map.remove(&hash);
                    self.block_heights.remove(&hash);
                    self.cumulative_work.remove(&hash);
                }
                None => break,
            }
        }
    }

    /// Get the UTXO for a specific output.
    pub fn get_utxo(&self, txid: &[u8; 32], vout: u32) -> Option<&Utxo> {
        self.utxo_set.get(&(*txid, vout))
    }

    /// Compute the actual reward paid by a coinstake transaction.
    ///
    /// A coinstake returns the staked principal plus the reward
    /// (`outputs[1] = stake + reward`), so summing the outputs overstates the
    /// reward by the full staked amount. The true reward is
    /// `total_output − staked_input_value`, where the input value is resolved
    /// from the transaction that created the staked output.
    ///
    /// Returns `None` if the transaction is not a coinstake or the staked
    /// input cannot be resolved (e.g. pruned history).
    pub fn coinstake_reward(&self, tx: &Transaction) -> Option<u64> {
        if tx.tx_type != crate::block::TxType::Coinstake {
            return None;
        }
        let total_output: u64 = tx.outputs.iter().map(|o| o.value).sum();
        let input = tx.inputs.first()?;
        let (parent, _, _) = self.get_transaction(&input.prev_txid)?;
        let staked = parent.outputs.get(input.prev_vout as usize)?.value;
        total_output.checked_sub(staked)
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

    /// Verify every input's scriptSig against its UTXO's scriptPubKey using
    /// the script engine, against the current chain state.
    ///
    /// Used at mempool admission so script-invalid transactions (bad
    /// signature, unsatisfied timelock) cannot be relayed network-wide and
    /// poison stakers' block templates. Mirrors the per-input checks in
    /// `apply_transaction_journaled`.
    pub fn verify_tx_scripts(&self, tx: &Transaction, height: u32, timestamp: u32) -> Result<()> {
        use vtorrent_script::{Engine, Script, ScriptEnv};

        if tx.is_legacy_claim() {
            let address = tx.claim_address.as_ref().ok_or_else(|| {
                NodeError::InvalidClaim("Legacy claim is missing its address".into())
            })?;
            if self.claimed_addresses.contains(address) {
                return Err(NodeError::ClaimAlreadyProcessed(address.clone()));
            }
            return validate_legacy_claim(tx, crate::genesis::get_legacy_balance(address));
        }
        if tx.is_coinbase() || tx.is_coinstake() {
            return Ok(());
        }
        for (input_index, input) in tx.inputs.iter().enumerate() {
            let utxo = self
                .utxo_set
                .get(&(input.prev_txid, input.prev_vout))
                .ok_or_else(|| {
                    NodeError::InvalidTransaction(format!(
                        "input {}:{} not in UTXO set",
                        hex::encode(input.prev_txid),
                        input.prev_vout
                    ))
                })?;
            let tx_hash = tx.sighash(input_index, &utxo.script_pubkey);
            let env = ScriptEnv {
                tx_hash,
                block_height: height,
                block_time: timestamp,
                tx_lock_time: tx.lock_time,
                input_sequence: input.sequence,
                utxo_height: utxo.height,
                utxo_timestamp: utxo.timestamp,
            };
            let mut engine = Engine::new(env);
            let script_sig = Script::from_bytes(input.script_sig.clone())
                .map_err(|e| NodeError::InvalidTransaction(format!("Invalid scriptSig: {}", e)))?;
            let script_pubkey = Script::from_bytes(utxo.script_pubkey.clone()).map_err(|e| {
                NodeError::InvalidTransaction(format!("Invalid scriptPubkey: {}", e))
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
        Ok(())
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
        vtorrent_core::address::Address::parse(address)
            .map(|addr| addr.p2pkh_script_pubkey())
            .unwrap_or_default()
    }

    /// Get the full UTXO set.
    pub fn get_utxo_set(&self) -> &BTreeMap<([u8; 32], u32), Utxo> {
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

    pub fn get_recent_transactions_for_addresses(
        &self,
        addresses: &[String],
        limit: usize,
    ) -> Vec<(String, u32, u32, String, u64, u64)> {
        let scripts: HashSet<Vec<u8>> = addresses
            .iter()
            .map(|address| self.address_to_p2pkh_script(address))
            .filter(|script| !script.is_empty())
            .collect();
        if scripts.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();
        // Bound the scan depth. This walks the chain newest-first and stops
        // early once `limit` matches are found, so the worst case is a wallet
        // with few or no matching transactions — which would otherwise scan
        // every block while holding the chain lock and stall P2P block
        // processing. The cap bounds that worst case; a full address→txids
        // index is the proper fix for unbounded history.
        const MAX_SCAN_BLOCKS: u32 = 200_000;
        let floor = self.best_height().saturating_sub(MAX_SCAN_BLOCKS);
        for height in (floor..=self.best_height()).rev() {
            let Some(block) = self.get_block_at_height(height) else {
                continue;
            };
            for tx in block.transactions.iter().rev() {
                let received: u64 = tx
                    .outputs
                    .iter()
                    .filter(|output| scripts.contains(&output.script_pubkey))
                    .map(|output| output.value)
                    .sum();
                let sent: u64 = tx
                    .inputs
                    .iter()
                    .filter_map(|input| {
                        self.resolve_output(&input.prev_txid, input.prev_vout)
                            .filter(|output| scripts.contains(&output.script_pubkey))
                            .map(|output| output.value)
                    })
                    .sum();
                if received == 0 && sent == 0 {
                    continue;
                }

                let total_output = tx.total_output();
                let fee = self.tx_fee(tx, total_output);
                let (direction, amount) = if tx.is_coinstake() {
                    ("stake", received.saturating_sub(sent))
                } else if sent > 0 {
                    ("send", sent.saturating_sub(received).saturating_sub(fee))
                } else {
                    ("receive", received)
                };
                result.push((
                    hex::encode(tx.txid()),
                    height,
                    block.header.timestamp,
                    direction.into(),
                    amount,
                    fee,
                ));
                if result.len() == limit {
                    return result;
                }
            }
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
        self.resolve_output(txid, vout).map(|output| output.value)
    }

    fn resolve_output(&self, txid: &[u8; 32], vout: u32) -> Option<&crate::block::TxOutput> {
        let (block_hash, offset) = self.tx_index.get(txid)?;
        let block = self.blocks.get(block_hash)?;
        block.transactions.get(*offset)?.outputs.get(vout as usize)
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

        // Duplicate check. Uses `block_heights` (the full index, retained even
        // after a body is pruned) rather than `blocks`, so a re-received old
        // block is still recognised as a duplicate.
        if self.block_heights.contains_key(&block_hash) {
            return Ok(BlockAcceptance::Duplicate);
        }

        let prev_hash = block.header.prev_block_hash;

        // Determine if this block extends the main chain or a fork
        let main_tip = self.best_hash().unwrap_or([0u8; 32]);

        if prev_hash == main_tip {
            // ── Happy path: extends the main chain ───────────────────────
            let height = self.best_height() + 1;
            let prev_header = self
                .get_header_at_height(height - 1)
                .ok_or_else(|| NodeError::Chain("Previous block not found".into()))?;

            validate_block_inner(
                &block,
                height - 1,
                prev_header.timestamp,
                prev_header.bits,
                prev_header.stake_modifier,
                prev_hash,
                self.allow_pow_test_blocks,
            )
            .map_err(|e| {
                tracing::warn!(
                    height = %(height - 1),
                    hash = %hex::encode(block_hash),
                    reason = %e,
                    "Block validation failed on main chain"
                );
                e
            })?;

            let journal = self.apply_block_journaled(&block, height)?;
            if block.header.is_pos() && block.header.utxo_root != journal.utxo_root {
                crate::chain::chain_reorg::rollback_journal(self, &journal);
                // `apply_block_journaled` already committed both deltas, so
                // undo them here too (rollback_journal only restores the UTXO
                // set and claims).
                self.total_supply = self.total_supply.saturating_sub(journal.supply_delta);
                self.total_staked = if journal.staked_delta >= 0 {
                    self.total_staked
                        .saturating_sub(journal.staked_delta as u64)
                } else {
                    self.total_staked
                        .saturating_add(journal.staked_delta.unsigned_abs())
                };
                return Err(NodeError::InvalidBlock(format!(
                    "UTXO root {} does not match computed post-state root {}",
                    hex::encode(block.header.utxo_root),
                    hex::encode(journal.utxo_root)
                )));
            }

            let parent_work = self.cumulative_work.get(&prev_hash).copied().unwrap_or(0);
            self.cumulative_work.insert(block_hash, parent_work + 1);
            self.parent_map.insert(block_hash, prev_hash);
            self.block_heights.insert(block_hash, height);

            // Extract UTXO diff from journal for persistence
            let mut utxos_added: Vec<Utxo> = Vec::new();
            let mut utxos_removed: Vec<([u8; 32], u32)> = Vec::new();
            for change in &journal.changes {
                match change {
                    chain_reorg::UtxoChange::Added { key } => {
                        if let Some(utxo) = self.utxo_set.get(key) {
                            utxos_added.push(utxo.clone());
                        }
                    }
                    chain_reorg::UtxoChange::Removed { key, .. } => {
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

            let tx_count = block.transactions.len();
            let block_timestamp = block.header.timestamp;
            self.index_block_transactions(block_hash, &block);
            self.headers.insert(block_hash, block.header.clone());
            self.blocks.insert(block_hash, block);
            self.height_index.push(block_hash);
            self.prune_bodies();

            tracing::info!(
                height = %height,
                hash = %hex::encode(block_hash),
                tx_count = %tx_count,
                timestamp = %block_timestamp,
                "Block accepted on main chain"
            );

            Ok(BlockAcceptance::MainChain {
                height,
                utxos_added,
                utxos_removed,
                claimed_addresses,
            })
        } else if self.block_heights.contains_key(&prev_hash) {
            // ── Fork: block's parent is known but not the main tip ────────
            // Classify from the index (`block_heights` covers genesis too, which
            // has no `parent_map` entry) and read the parent's header fields —
            // never its body, which may be pruned.
            let parent_height = self.block_heights.get(&prev_hash).copied().unwrap_or(0);
            let fork_height = parent_height + 1;
            let parent_header = self.headers.get(&prev_hash).ok_or_else(|| {
                NodeError::Chain(format!("missing parent header {}", hex::encode(prev_hash)))
            })?;
            let parent_timestamp = parent_header.timestamp;
            let parent_bits = parent_header.bits;
            let parent_modifier = parent_header.stake_modifier;

            // Validate against the fork parent
            validate_block_inner(
                &block,
                parent_height,
                parent_timestamp,
                parent_bits,
                parent_modifier,
                prev_hash,
                self.allow_pow_test_blocks,
            )?;

            let parent_work = self.cumulative_work.get(&prev_hash).copied().unwrap_or(0);
            let fork_work = parent_work + 1;
            self.cumulative_work.insert(block_hash, fork_work);
            self.parent_map.insert(block_hash, prev_hash);
            self.block_heights.insert(block_hash, fork_height);
            self.headers.insert(block_hash, block.header.clone());
            self.blocks.insert(block_hash, block);

            let main_work = self.cumulative_work.get(&main_tip).copied().unwrap_or(0);

            if fork_work > main_work {
                // ── Reorg: fork is now longer than main chain ─────────────
                let old_tip = main_tip;
                let (rolled_back_txs, rolled_back_blocks, applied_fork_blocks) =
                    match self.reorganize_to(block_hash, fork_height) {
                        Ok(result) => result,
                        Err(error) => {
                            self.blocks.remove(&block_hash);
                            self.cumulative_work.remove(&block_hash);
                            self.parent_map.remove(&block_hash);
                            self.block_heights.remove(&block_hash);
                            self.headers.remove(&block_hash);
                            return Err(error);
                        }
                    };

                let depth = rolled_back_blocks.len() as u32;
                tracing::warn!(
                    "Chain reorg: old tip {} → new tip {} (depth {})",
                    hex::encode(old_tip),
                    hex::encode(block_hash),
                    depth
                );

                self.prune_bodies();
                Ok(BlockAcceptance::Reorg {
                    old_tip,
                    new_tip: block_hash,
                    depth,
                    rolled_back_txs,
                    rolled_back_blocks,
                    applied_fork_blocks,
                })
            } else {
                tracing::debug!(
                    "Fork block {} at height {} (work {} < main {})",
                    hex::encode(block_hash),
                    fork_height,
                    fork_work,
                    main_work
                );
                self.prune_bodies();
                Ok(BlockAcceptance::Fork {
                    fork_tip: block_hash,
                })
            }
        } else {
            // Parent not known — orphan block, reject for now
            tracing::warn!(
                hash = %hex::encode(block_hash),
                parent = %hex::encode(prev_hash),
                "Orphan block rejected: parent not found"
            );
            Err(NodeError::InvalidBlock(format!(
                "Orphan block {}: parent {} not found",
                hex::encode(block_hash),
                hex::encode(prev_hash)
            )))
        }
    }

    /// Find the height of a known block by its hash (O(1)).
    ///
    /// Covers main-chain and fork blocks; `tx_index` and RPC callers only
    /// pass main-chain hashes, so the broader scope is safe.
    pub fn block_height(&self, hash: &[u8; 32]) -> Option<u32> {
        self.block_heights.get(hash).copied()
    }

    /// Reorganize the main chain to make `new_tip` the best tip.
    fn reorganize_to(
        &mut self,
        new_tip: [u8; 32],
        new_tip_height: u32,
    ) -> Result<(
        Vec<Transaction>,
        Vec<RolledBackBlock>,
        Vec<AppliedForkBlock>,
    )> {
        chain_reorg::reorganize_to(self, new_tip, new_tip_height)
    }

    /// Roll back the most recent main chain block, restoring the UTXO set.
    #[cfg(test)]
    fn rollback_one_block(&mut self) -> Result<(Vec<Transaction>, RolledBackBlock)> {
        chain_reorg::rollback_one_block(self)
    }

    /// Add all transactions from an active main-chain block to the transaction index.
    fn index_block_transactions(&mut self, block_hash: [u8; 32], block: &Block) {
        chain_reorg::index_block_transactions(self, block_hash, block);
    }

    /// Apply a block's transactions to the UTXO set, recording a journal for rollback.
    fn apply_block_journaled(
        &mut self,
        block: &Block,
        height: u32,
    ) -> Result<chain_reorg::BlockJournal> {
        chain_reorg::apply_block_journaled(self, block, height)
    }
}

#[cfg(test)]
mod chain_tests;
