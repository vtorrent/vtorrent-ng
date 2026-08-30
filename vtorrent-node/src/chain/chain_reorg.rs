use crate::{
    block::{Block, Transaction},
    consensus::{check_stake_kernel, compute_pos_reward, validate_legacy_claim, MAX_SUPPLY},
    error::{NodeError, Result},
    genesis::get_legacy_balance,
};
use std::collections::HashSet;
use vtorrent_script::{Engine, Script, ScriptEnv};

use super::{compute_supply_delta, AppliedForkBlock, Chain, RolledBackBlock, Utxo};

#[derive(Debug, Clone)]
pub(crate) enum UtxoChange {
    Added { key: ([u8; 32], u32) },
    Removed { key: ([u8; 32], u32), utxo: Utxo },
}

#[derive(Debug, Clone)]
pub(crate) struct BlockJournal {
    pub(crate) block_hash: [u8; 32],
    pub(crate) height: u32,
    pub(crate) changes: Vec<UtxoChange>,
    pub(crate) claimed_addresses: Vec<String>,
    pub(crate) supply_delta: u64,
}

pub(crate) fn ancestors(chain: &Chain, mut tip: [u8; 32]) -> Vec<[u8; 32]> {
    let mut path = Vec::new();
    let genesis = chain.height_index[0];
    loop {
        path.push(tip);
        if tip == genesis {
            break;
        }
        match chain.parent_map.get(&tip) {
            Some(&parent) => tip = parent,
            None => break,
        }
    }
    path
}

pub(crate) fn rollback_one_block(chain: &mut Chain) -> Result<(Vec<Transaction>, RolledBackBlock)> {
    let journal = chain
        .journals
        .pop_back()
        .ok_or_else(|| NodeError::Chain("No journal to roll back".into()))?;

    let rolled_back_txs: Vec<Transaction> = chain
        .blocks
        .get(&journal.block_hash)
        .map(|b| b.transactions.clone())
        .unwrap_or_default();

    let mut utxos_to_remove: Vec<([u8; 32], u32)> = Vec::new();
    let mut utxos_to_restore: Vec<Utxo> = Vec::new();
    for change in &journal.changes {
        match change {
            UtxoChange::Added { key } => utxos_to_remove.push(*key),
            UtxoChange::Removed { key, utxo } => {
                utxos_to_remove.push(*key);
                utxos_to_restore.push(utxo.clone());
            }
        }
    }
    let claimed_to_remove = journal.claimed_addresses.clone();

    for change in journal.changes.into_iter().rev() {
        match change {
            UtxoChange::Added { key } => {
                chain.utxo_set.remove(&key);
            }
            UtxoChange::Removed { key, utxo } => {
                chain.utxo_set.insert(key, utxo);
            }
        }
    }

    for addr in &claimed_to_remove {
        chain.claimed_addresses.remove(addr);
    }

    chain.total_supply = chain.total_supply.saturating_sub(journal.supply_delta);

    remove_block_transactions(chain, journal.block_hash);

    chain.height_index.pop();

    tracing::debug!(
        "Rolled back block {} at height {}",
        hex::encode(journal.block_hash),
        journal.height
    );

    Ok((
        rolled_back_txs,
        RolledBackBlock {
            hash: journal.block_hash,
            height: journal.height,
            utxos_to_remove,
            utxos_to_restore,
            claimed_to_remove,
        },
    ))
}

pub(crate) fn index_block_transactions(chain: &mut Chain, block_hash: [u8; 32], block: &Block) {
    for (tx_offset, tx) in block.transactions.iter().enumerate() {
        chain.tx_index.insert(tx.txid(), (block_hash, tx_offset));
    }
}

pub(crate) fn remove_block_transactions(chain: &mut Chain, block_hash: [u8; 32]) {
    let txids: Vec<[u8; 32]> = chain
        .blocks
        .get(&block_hash)
        .map(|block| block.transactions.iter().map(Transaction::txid).collect())
        .unwrap_or_default();
    for txid in txids {
        chain.tx_index.remove(&txid);
    }
}

pub(crate) fn apply_block_journaled(
    chain: &mut Chain,
    block: &Block,
    height: u32,
) -> Result<BlockJournal> {
    let mut journal = BlockJournal {
        block_hash: block.hash(),
        height,
        changes: Vec::new(),
        claimed_addresses: Vec::new(),
        supply_delta: 0,
    };

    let parent_modifier = if height == 0 {
        0
    } else {
        chain
            .blocks
            .get(&block.header.prev_block_hash)
            .map(|b| b.header.stake_modifier)
            .ok_or_else(|| NodeError::Chain("Parent block not found for stake modifier".into()))?
    };

    for tx in &block.transactions {
        if let Err(e) = apply_transaction_journaled(
            chain,
            tx,
            height,
            block.header.timestamp,
            parent_modifier,
            &mut journal,
        ) {
            rollback_journal(chain, &journal);
            return Err(e);
        }
    }

    let new_supply = chain.total_supply.saturating_add(journal.supply_delta);
    if new_supply > MAX_SUPPLY {
        rollback_journal(chain, &journal);
        return Err(NodeError::InvalidBlock(format!(
            "Block would exceed maximum supply: {} + {} > {}",
            chain.total_supply, journal.supply_delta, MAX_SUPPLY
        )));
    }
    chain.total_supply = new_supply;

    Ok(journal)
}

pub(crate) fn rollback_journal(chain: &mut Chain, journal: &BlockJournal) {
    for change in journal.changes.iter().rev() {
        match change {
            UtxoChange::Added { key } => {
                chain.utxo_set.remove(key);
            }
            UtxoChange::Removed { key, utxo } => {
                chain.utxo_set.insert(*key, utxo.clone());
            }
        }
    }
    for addr in journal.claimed_addresses.iter().rev() {
        chain.claimed_addresses.remove(addr);
    }
}

fn apply_transaction_journaled(
    chain: &mut Chain,
    tx: &Transaction,
    height: u32,
    timestamp: u32,
    parent_stake_modifier: u64,
    journal: &mut BlockJournal,
) -> Result<()> {
    let txid = tx.txid();

    let mut total_input: u64 = 0;
    let mut stake_input: Option<Utxo> = None;
    if !tx.is_coinbase() {
        for (input_index, input) in tx.inputs.iter().enumerate() {
            let key = (input.prev_txid, input.prev_vout);
            if let Some(utxo) = chain.utxo_set.remove(&key) {
                total_input = total_input.saturating_add(utxo.value);

                let script_bytes = utxo.script_pubkey.clone();
                let stake_value = utxo.value;
                let stake_height = utxo.height;
                let stake_timestamp = utxo.timestamp;

                if !tx.is_legacy_claim() {
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
                    let script_sig = Script::from_bytes(input.script_sig.clone()).map_err(|e| {
                        NodeError::InvalidTransaction(format!("Invalid scriptSig: {}", e))
                    })?;
                    let script_pubkey = Script::from_bytes(script_bytes.clone()).map_err(|e| {
                        NodeError::InvalidTransaction(format!("Invalid scriptPubKey: {}", e))
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

    if tx.is_legacy_claim() {
        if let Some(addr) = &tx.claim_address {
            if chain.claimed_addresses.contains(addr) {
                return Err(NodeError::ClaimAlreadyProcessed(addr.clone()));
            }
            let snapshot_balance = get_legacy_balance(addr);
            validate_legacy_claim(tx, snapshot_balance).map_err(|e| {
                NodeError::InvalidTransaction(format!("Invalid legacy claim: {}", e))
            })?;
            chain.claimed_addresses.insert(addr.clone());
            journal.claimed_addresses.push(addr.clone());
        }
    }

    for (vout, output) in tx.outputs.iter().enumerate() {
        let key = (txid, vout as u32);
        chain.utxo_set.insert(
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

    if !tx.is_coinbase() && !tx.is_coinstake() && !tx.is_legacy_claim() {
        let total_output = tx.total_output();
        if total_output > total_input {
            return Err(NodeError::InvalidTransaction(format!(
                "Transaction creates value: inputs {} < outputs {}",
                total_input, total_output
            )));
        }
    }

    if tx.is_coinstake() {
        let staked = stake_input.ok_or_else(|| {
            NodeError::InvalidTransaction("Coinstake must spend a stake input".into())
        })?;
        let coin_age = timestamp.saturating_sub(staked.timestamp);
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

    let total_output = tx.total_output();
    let supply_delta = compute_supply_delta(tx, total_input, total_output);
    journal.supply_delta = journal.supply_delta.saturating_add(supply_delta);

    Ok(())
}

pub(crate) fn reorganize_to(
    chain: &mut Chain,
    new_tip: [u8; 32],
    new_tip_height: u32,
) -> Result<(
    Vec<Transaction>,
    Vec<RolledBackBlock>,
    Vec<AppliedForkBlock>,
)> {
    let old_tip = chain.best_hash().unwrap_or([0u8; 32]);

    let new_chain = ancestors(chain, new_tip);
    let old_chain = ancestors(chain, old_tip);

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

    let fork_height = chain
        .block_height(&fork_point)
        .ok_or_else(|| NodeError::Chain("Fork point not on main chain".into()))?;

    tracing::info!("Reorg: fork point at height {}", fork_height);

    let mut applied_fork_blocks: Vec<AppliedForkBlock> = Vec::new();
    let mut rolled_back_txs: Vec<Transaction> = Vec::new();
    let mut rolled_back_blocks: Vec<RolledBackBlock> = Vec::new();
    while chain.best_height() > fork_height {
        let (txs, rb) = rollback_one_block(chain)?;
        rolled_back_txs.extend(txs);
        rolled_back_blocks.push(rb);
    }

    let mut to_apply: Vec<[u8; 32]> = Vec::new();
    let mut cursor = new_tip;
    while cursor != fork_point {
        to_apply.push(cursor);
        cursor = chain
            .parent_map
            .get(&cursor)
            .copied()
            .ok_or_else(|| NodeError::Chain("Missing parent during reorg apply".into()))?;
    }
    to_apply.reverse();

    for (i, hash) in to_apply.iter().enumerate() {
        let height = fork_height + 1 + i as u32;
        let block = chain
            .blocks
            .get(hash)
            .ok_or_else(|| {
                NodeError::Chain(format!("Missing block {} during reorg", hex::encode(hash)))
            })?
            .clone();

        let journal = apply_block_journaled(chain, &block, height)?;
        index_block_transactions(chain, *hash, &block);
        let claimed_addresses = journal.claimed_addresses.clone();
        let (utxos_added, utxos_removed): (Vec<Utxo>, Vec<([u8; 32], u32)>) = journal
            .changes
            .iter()
            .fold(
                (Vec::new(), Vec::new()),
                |(mut added, mut removed), c| match c {
                    UtxoChange::Added { key } => {
                        if let Some(u) = chain.utxo_set.get(key) {
                            added.push(u.clone());
                        }
                        removed.retain(|(t, v)| t != &key.0 || v != &key.1);
                        (added, removed)
                    }
                    UtxoChange::Removed { key, .. } => {
                        removed.push(*key);
                        (added, removed)
                    }
                },
            );
        applied_fork_blocks.push(AppliedForkBlock {
            block: block.clone(),
            height,
            utxos_added,
            utxos_removed,
            claimed_addresses,
        });
        chain.journals.push_back(journal);
        chain.height_index.push(*hash);
    }

    if chain.best_hash() != Some(new_tip) || chain.best_height() != new_tip_height {
        return Err(NodeError::Chain(format!(
            "reorg verification failed: expected tip {:?} at height {}, got {:?} at height {}",
            new_tip,
            new_tip_height,
            chain.best_hash(),
            chain.best_height()
        )));
    }

    Ok((rolled_back_txs, rolled_back_blocks, applied_fork_blocks))
}
