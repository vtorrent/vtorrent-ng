use crate::{
    block::{Block, Transaction},
    consensus::{
        check_stake_kernel_v2, compute_pos_reward_with, is_stakeable_with, validate_legacy_claim,
        validate_transaction, MAX_BLOCK_SIZE, MAX_SUPPLY,
    },
    error::{NodeError, Result},
    genesis::get_legacy_balance,
};
use std::collections::{BTreeSet, HashSet};
use vtorrent_script::{Engine, Script, ScriptEnv};

use super::{compute_supply_delta, AppliedForkBlock, Chain, RolledBackBlock, Utxo};

/// Return the P2CS script governing a spend: the UTXO script itself when it is
/// a raw P2CS, or the redeem script revealed in the scriptSig when the UTXO is
/// P2SH-wrapped P2CS.
pub(crate) fn p2cs_redeem_script(utxo_script: &[u8], script_sig: &[u8]) -> Option<Vec<u8>> {
    let script = Script::from_bytes(utxo_script.to_vec()).ok()?;
    match vtorrent_script::classify_script(&script) {
        vtorrent_script::ScriptType::P2CS { .. } => Some(utxo_script.to_vec()),
        vtorrent_script::ScriptType::P2SH => {
            let sig = Script::from_bytes(script_sig.to_vec()).ok()?;
            let redeem = sig
                .iter()
                .filter_map(|item| match item {
                    vtorrent_script::script::ScriptItem::PushData(data) => Some(data),
                    _ => None,
                })
                .last()?;
            let redeem_script = Script::from_bytes(redeem.to_vec()).ok()?;
            matches!(
                vtorrent_script::classify_script(&redeem_script),
                vtorrent_script::ScriptType::P2CS { .. }
            )
            .then(|| redeem.to_vec())
        }
        _ => None,
    }
}

/// A non-coinstake spend of a P2CS UTXO must be authorized by the cold
/// spending key. The P2CS script's hot branch is otherwise usable by any
/// transaction, which would let the online staking key steal delegated funds.
///
/// The scriptSig shape is `<sig> <pubkey> <selector>` for a raw P2CS UTXO and
/// `<sig> <pubkey> <selector> <redeem_script>` for a P2SH-wrapped one. Checking
/// the spending-key signature directly is independent of how the selector is
/// encoded.
pub(crate) fn p2cs_cold_spend_authorized(
    tx: &Transaction,
    input_index: usize,
    utxo: &Utxo,
) -> bool {
    let Some(p2cs_script) =
        p2cs_redeem_script(&utxo.script_pubkey, &tx.inputs[input_index].script_sig)
    else {
        return false;
    };
    let Ok(script) = Script::from_bytes(p2cs_script) else {
        return false;
    };
    let vtorrent_script::ScriptType::P2CS {
        spending_hash,
        locktime,
        ..
    } = vtorrent_script::classify_script(&script)
    else {
        return false;
    };
    if tx.lock_time < locktime {
        return false;
    }
    let Ok(script_sig) = Script::from_bytes(tx.inputs[input_index].script_sig.clone()) else {
        return false;
    };
    let mut pushes: Vec<&[u8]> = script_sig
        .iter()
        .filter_map(|item| match item {
            vtorrent_script::script::ScriptItem::PushData(data) => Some(data),
            _ => None,
        })
        .collect();
    let is_p2sh = Script::from_bytes(utxo.script_pubkey.clone())
        .ok()
        .map(|s| {
            matches!(
                vtorrent_script::classify_script(&s),
                vtorrent_script::ScriptType::P2SH
            )
        })
        .unwrap_or(false);
    if is_p2sh {
        // The last push is the redeem script, not part of the signature stack.
        pushes.pop();
    }
    if pushes.len() < 3 {
        return false;
    }
    let sig = pushes[pushes.len() - 3];
    let pubkey = pushes[pushes.len() - 2];
    if vtorrent_core::crypto::hash160(pubkey) != spending_hash {
        return false;
    }
    let Some((sighash_type, der)) = sig.split_last() else {
        return false;
    };
    if *sighash_type != 0x01 {
        return false;
    }
    let Ok(sig) = secp256k1::ecdsa::Signature::from_der(der) else {
        return false;
    };
    let Ok(pubkey) = secp256k1::PublicKey::from_slice(pubkey) else {
        return false;
    };
    let msg = secp256k1::Message::from_digest(tx.sighash(input_index, &utxo.script_pubkey));
    secp256k1::Secp256k1::verification_only()
        .verify_ecdsa(&msg, &sig, &pubkey)
        .is_ok()
}

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
    /// Signed change to `Chain::total_staked` (stakeable UTXO value added minus
    /// removed). Applied on commit and reversed on rollback.
    pub(crate) staked_delta: i64,
    pub(crate) utxo_root: [u8; 32],
    previous_governance: Option<crate::governance::GovernanceUndo>,
    previous_params: crate::consensus::ConsensusParams,
    previous_supply: u64,
    previous_staked: u64,
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

    let touched: BTreeSet<_> = journal
        .changes
        .iter()
        .map(|change| match change {
            UtxoChange::Added { key } | UtxoChange::Removed { key, .. } => *key,
        })
        .collect();
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

    let mut utxos_to_remove = Vec::new();
    let mut utxos_to_restore = Vec::new();
    for key in touched {
        if let Some(utxo) = chain.utxo_set.get(&key) {
            utxos_to_restore.push(utxo.clone());
        } else {
            utxos_to_remove.push(key);
        }
    }

    for addr in &claimed_to_remove {
        chain.claimed_addresses.remove(addr);
    }

    chain.total_supply = journal.previous_supply;
    chain.total_staked = journal.previous_staked;
    chain.params = journal.previous_params;
    if let Some(governance) = journal.previous_governance {
        chain.governance.restore_undo(&governance);
    }

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

fn new_journal(chain: &Chain, block_hash: [u8; 32], height: u32) -> BlockJournal {
    BlockJournal {
        block_hash,
        height,
        changes: Vec::new(),
        claimed_addresses: Vec::new(),
        supply_delta: 0,
        staked_delta: 0,
        utxo_root: [0u8; 32],
        previous_governance: None,
        previous_params: chain.params,
        previous_supply: chain.total_supply,
        previous_staked: chain.total_staked,
    }
}

fn begin_governance_block(
    chain: &mut Chain,
    height: u32,
    timestamp: u32,
    governance_active: bool,
    journal: &BlockJournal,
) {
    if governance_active {
        chain
            .governance
            .begin_block(height, &mut chain.params, &chain.utxo_set, timestamp);
        if chain.params.min_stake_amount != journal.previous_params.min_stake_amount {
            chain.total_staked = chain
                .utxo_set
                .values()
                .filter(|u| crate::consensus::is_stakeable_with(&chain.params, u))
                .map(|u| u.value)
                .sum();
        }
    }
}

fn apply_transaction(
    chain: &mut Chain,
    tx: &Transaction,
    height: u32,
    timestamp: u32,
    parent_modifier: u64,
    journal: &mut BlockJournal,
    governance_active: bool,
) -> Result<()> {
    if governance_active {
        chain.governance.process_transaction(
            tx,
            height,
            timestamp,
            &chain.utxo_set,
            chain.params,
            chain.total_staked,
            chain.height_index[0],
        )?;
    }
    apply_transaction_journaled(
        chain,
        tx,
        height,
        timestamp,
        parent_modifier,
        journal,
        governance_active,
    )
}

pub(crate) fn apply_block_journaled(
    chain: &mut Chain,
    block: &Block,
    height: u32,
) -> Result<BlockJournal> {
    let governance_active = chain.governance_active(height);
    let governance_before = governance_active.then(|| chain.governance.clone());
    let mut journal = new_journal(chain, block.hash(), height);

    let parent_modifier = if height == 0 {
        0
    } else {
        chain
            .headers
            .get(&block.header.prev_block_hash)
            .map(|b| b.stake_modifier)
            .ok_or_else(|| NodeError::Chain("Parent block not found for stake modifier".into()))?
    };

    let application = (|| -> Result<()> {
        begin_governance_block(
            chain,
            height,
            block.header.timestamp,
            governance_active,
            &journal,
        );

        for tx in &block.transactions {
            apply_transaction(
                chain,
                tx,
                height,
                block.header.timestamp,
                parent_modifier,
                &mut journal,
                governance_active,
            )?;
        }

        let new_supply = chain.total_supply.saturating_add(journal.supply_delta);
        if new_supply > MAX_SUPPLY {
            return Err(NodeError::InvalidBlock(format!(
                "Block would exceed maximum supply: {} + {} > {}",
                chain.total_supply, journal.supply_delta, MAX_SUPPLY
            )));
        }
        chain.total_supply = new_supply;
        // Apply the staked-supply delta (stakeable value added minus removed).
        chain.total_staked = if journal.staked_delta >= 0 {
            chain
                .total_staked
                .saturating_add(journal.staked_delta as u64)
        } else {
            chain
                .total_staked
                .saturating_sub(journal.staked_delta.unsigned_abs())
        };

        // Reuse the chain's commitment buffers (byte-identical to
        // `compute_utxo_root_ordered`; see docs/utxo-commitment-scratch-design.md).
        journal.utxo_root = chain.recompute_utxo_root();
        Ok(())
    })();

    journal.previous_governance = governance_before
        .as_ref()
        .map(|before| chain.governance.undo_since(before));
    if let Err(error) = application {
        rollback_journal(chain, &journal);
        return Err(error);
    }

    Ok(journal)
}

pub(crate) fn rollback_journal(chain: &mut Chain, journal: &BlockJournal) {
    chain.params = journal.previous_params;
    chain.total_supply = journal.previous_supply;
    chain.total_staked = journal.previous_staked;
    if let Some(governance) = &journal.previous_governance {
        chain.governance.restore_undo(governance);
    }
    rollback_changes(chain, &journal.changes, &journal.claimed_addresses);
}

fn rollback_changes(chain: &mut Chain, changes: &[UtxoChange], claimed_addresses: &[String]) {
    for change in changes.iter().rev() {
        match change {
            UtxoChange::Added { key } => {
                chain.utxo_set.remove(key);
            }
            UtxoChange::Removed { key, utxo } => {
                chain.utxo_set.insert(*key, utxo.clone());
            }
        }
    }
    for addr in claimed_addresses.iter().rev() {
        chain.claimed_addresses.remove(addr);
    }
}

pub(crate) fn select_block_transactions(
    chain: &mut Chain,
    first_tx: &Transaction,
    candidates: Vec<Transaction>,
    height: u32,
    timestamp: u32,
) -> Result<Vec<Transaction>> {
    if height != chain.best_height() + 1 {
        return Err(NodeError::Chain(
            "Template height must extend the current tip".into(),
        ));
    }
    if !(first_tx.is_coinstake() || (chain.allow_pow_test_blocks && first_tx.is_coinbase())) {
        return Err(NodeError::InvalidBlock(
            "Template must begin with coinstake or regtest coinbase".into(),
        ));
    }
    let parent = chain
        .best_hash()
        .ok_or_else(|| NodeError::Chain("Template parent missing".into()))?;
    let modifier = chain.headers[&parent].stake_modifier;
    let governance_active = chain.governance_active(height);
    let governance_before = governance_active.then(|| chain.governance.clone());
    let mut journal = new_journal(chain, [0; 32], height);
    let selection = (|| -> Result<Vec<Transaction>> {
        begin_governance_block(chain, height, timestamp, governance_active, &journal);
        validate_transaction(first_tx)?;
        let mut size = first_tx.serialized_size();
        if size > MAX_BLOCK_SIZE {
            return Err(NodeError::InvalidBlock(
                "Template prefix exceeds block size limit".into(),
            ));
        }
        apply_transaction(
            chain,
            first_tx,
            height,
            timestamp,
            modifier,
            &mut journal,
            governance_active,
        )?;
        if chain.total_supply.saturating_add(journal.supply_delta) > MAX_SUPPLY {
            return Err(NodeError::InvalidBlock(
                "Template prefix exceeds maximum supply".into(),
            ));
        }
        let mut kept = Vec::new();
        let mut txids = std::collections::HashSet::from([first_tx.txid()]);
        for tx in candidates {
            let tx_size = tx.serialized_size();
            if tx_size > MAX_BLOCK_SIZE - size
                || tx.is_coinbase()
                || tx.is_coinstake()
                || txids.contains(&tx.txid())
                || validate_transaction(&tx).is_err()
            {
                continue;
            }
            let changes = journal.changes.len();
            let claims = journal.claimed_addresses.len();
            let supply_delta = journal.supply_delta;
            let staked_delta = journal.staked_delta;
            let governance_undo =
                governance_active.then(|| chain.governance.transaction_undo(&tx, height));
            let result = apply_transaction(
                chain,
                &tx,
                height,
                timestamp,
                modifier,
                &mut journal,
                governance_active,
            );
            if result.is_err()
                || chain.total_supply.saturating_add(journal.supply_delta) > MAX_SUPPLY
            {
                if let Some(undo) = governance_undo {
                    chain.governance.restore_undo(&undo);
                }
                rollback_changes(
                    chain,
                    &journal.changes[changes..],
                    &journal.claimed_addresses[claims..],
                );
                journal.changes.truncate(changes);
                journal.claimed_addresses.truncate(claims);
                journal.supply_delta = supply_delta;
                journal.staked_delta = staked_delta;
                continue;
            }
            size += tx_size;
            txids.insert(tx.txid());
            kept.push(tx);
        }
        Ok(kept)
    })();
    journal.previous_governance = governance_before
        .as_ref()
        .map(|before| chain.governance.undo_since(before));
    rollback_journal(chain, &journal);
    selection
}

fn apply_transaction_journaled(
    chain: &mut Chain,
    tx: &Transaction,
    height: u32,
    timestamp: u32,
    parent_stake_modifier: u64,
    journal: &mut BlockJournal,
    governance_active: bool,
) -> Result<()> {
    let txid = tx.txid();

    let mut total_input: u64 = 0;
    let mut stake_input: Option<Utxo> = None;
    if !tx.is_coinbase() {
        for (input_index, input) in tx.inputs.iter().enumerate() {
            let key = (input.prev_txid, input.prev_vout);
            if governance_active
                && chain
                    .governance
                    .is_utxo_locked(&input.prev_txid, input.prev_vout, height)
                && !tx.is_coinstake()
            {
                return Err(NodeError::InvalidTransaction(
                    "input is locked by a governance vote".into(),
                ));
            }
            if let Some(utxo) = chain.utxo_set.remove(&key) {
                journal.changes.push(UtxoChange::Removed {
                    key,
                    utxo: utxo.clone(),
                });

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

                    // The P2CS hot branch is valid script-wise for any
                    // transaction. Only a coinstake may use it; every other
                    // spend must be authorized by the cold spending key.
                    if !tx.is_coinstake()
                        && p2cs_redeem_script(
                            &utxo.script_pubkey,
                            &tx.inputs[input_index].script_sig,
                        )
                        .is_some()
                        && !p2cs_cold_spend_authorized(tx, input_index, &utxo)
                    {
                        return Err(NodeError::InvalidTransaction(
                            "P2CS spend must be authorized by the cold spending key".into(),
                        ));
                    }
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

                if crate::consensus::is_stakeable_with(&chain.params, &utxo) {
                    // Checked: a crafted block can carry tens of thousands of
                    // MAX_MONEY outputs, and `overflow-checks = true` would
                    // turn an unchecked `-=` into a panic on the block path.
                    journal.staked_delta = journal
                        .staked_delta
                        .checked_sub(utxo.value as i64)
                        .ok_or_else(|| {
                            NodeError::InvalidTransaction("Staked-supply delta underflow".into())
                        })?;
                }
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
        if height == 0 && tx.claim_address.is_none() {
            if tx.claim_signature.is_some() {
                return Err(NodeError::InvalidTransaction(
                    "Genesis distribution must not carry a claim signature".into(),
                ));
            }
        } else {
            let addr = tx.claim_address.as_ref().ok_or_else(|| {
                NodeError::InvalidTransaction("Legacy claim is missing its address".into())
            })?;
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

        // A height-1 bootstrap block exists solely to seed the first stakeable
        // UTXO (genesis has none, so no coinstake can be produced — T3). If its
        // claim output is not stakeable it cannot unblock staking, so reject it.
        if height == 1 {
            let creates_stakeable = tx.outputs.iter().enumerate().any(|(vout, output)| {
                crate::consensus::is_stakeable(&Utxo {
                    txid,
                    vout: vout as u32,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    height,
                    timestamp,
                })
            });
            if !creates_stakeable {
                return Err(NodeError::InvalidTransaction(
                    "Bootstrap claim must create a stakeable (P2PKH >= MIN_STAKE_AMOUNT) output"
                        .into(),
                ));
            }
        }
    }

    for (vout, output) in tx.outputs.iter().enumerate() {
        // Exclude data-carrier OP_RETURN outputs (but retain the genesis
        // legacy-distribution claim markers). Must match the staking producer.
        if !crate::block::is_utxo_eligible(output, tx, height) {
            continue;
        }
        let key = (txid, vout as u32);
        let utxo = Utxo {
            txid,
            vout: vout as u32,
            value: output.value,
            script_pubkey: output.script_pubkey.clone(),
            height,
            timestamp,
        };
        if crate::consensus::is_stakeable_with(&chain.params, &utxo) {
            // Checked for the same reason as the removal above: an unchecked
            // `+=` over many MAX_MONEY outputs panics under overflow-checks.
            journal.staked_delta = journal
                .staked_delta
                .checked_add(output.value as i64)
                .ok_or_else(|| {
                    NodeError::InvalidTransaction("Staked-supply delta overflow".into())
                })?;
        }
        chain.utxo_set.insert(key, utxo);
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
        let staked = stake_input.as_ref().ok_or_else(|| {
            NodeError::InvalidTransaction("Coinstake must spend a stake input".into())
        })?;
        if !is_stakeable_with(&chain.params, staked) {
            return Err(NodeError::InvalidTransaction(
                "Coinstake input is not a stakeable output".into(),
            ));
        }
        let coin_age = timestamp.saturating_sub(staked.timestamp);
        if staked.value < chain.params.min_stake_amount {
            return Err(NodeError::InvalidTransaction(format!(
                "Stake value {} is below minimum {}",
                staked.value, chain.params.min_stake_amount
            )));
        }
        let coin_age = u64::from(coin_age);
        // The height-1 bootstrap UTXO is brand new at height 2, so it is exempt
        // from the minimum age for exactly that one block. Identify it by its
        // parent block being the bootstrap claim block (a height-1 block whose
        // only tx is a LegacyClaim), so regtest faucet coinbases at height 1 are
        // not accidentally exempted. It can only be staked at height 2, so the
        // exemption expires naturally.
        let is_bootstrap_utxo = height == 2
            && staked.height == 1
            && chain
                .get_block_at_height(1)
                .and_then(|b| b.transactions.first())
                .map(|tx| tx.is_legacy_claim())
                .unwrap_or(false);
        let min_age_ok = is_bootstrap_utxo || coin_age >= chain.params.min_stake_age;
        if !min_age_ok || coin_age > chain.params.max_stake_age {
            return Err(NodeError::InvalidTransaction(format!(
                "Stake age {} is outside the allowed range {}..={}",
                coin_age, chain.params.min_stake_age, chain.params.max_stake_age
            )));
        }
        // v2 kernel: probability is the staker's share of the total staked
        // supply as of the pre-block state (`chain.total_staked` is only
        // updated after this loop, so it still holds the pre-state value).
        // This caps a whale's block share at its stake share instead of
        // letting any UTXO >= 42,949.67 VTR win unconditionally.
        if !check_stake_kernel_v2(parent_stake_modifier, staked, timestamp, chain.total_staked) {
            return Err(NodeError::InvalidTransaction(
                "Coinstake kernel hash does not meet the stake target".into(),
            ));
        }
        let max_reward = compute_pos_reward_with(&chain.params, staked.value, coin_age);
        let minted = tx.total_output().saturating_sub(staked.value);
        if minted > max_reward {
            return Err(NodeError::InvalidTransaction(format!(
                "Coinstake mints {} above the allowed reward {}",
                minted, max_reward
            )));
        }

        // Cold-staking security hinge (R1): a coinstake spending a P2CS input
        // must re-lock the stake to the **same** P2CS script. Otherwise the hot
        // staking key could redirect the stake to an arbitrary script and steal
        // the coins. See docs/cold-staking-p2cs-design.md.
        //
        // The rule requires **all value** in the coinstake to re-lock to the
        // same P2CS script — not merely that the script appears among the
        // outputs. Otherwise the hot staking key could pay 1 satoshi back to
        // the P2CS script and redirect the entire stake (and the reward) to
        // itself. The empty marker output (value 0) is exempt.
        if let Some(p2cs_script) =
            p2cs_redeem_script(&staked.script_pubkey, &tx.inputs[0].script_sig)
        {
            let redirected = crate::block::p2cs_redirected_value_with_redeem(
                &staked.script_pubkey,
                &p2cs_script,
                &tx.outputs,
            );
            if redirected > 0 {
                return Err(NodeError::InvalidTransaction(format!(
                    "P2CS coinstake must re-lock all value to the same cold-stake script; \
                     {} satoshis were redirected elsewhere",
                    redirected
                )));
            }
            // R1 also forbids destroying the principal: a coinstake that
            // returns only 1 satoshi to the P2CS script redirects nothing, but
            // burns the delegated funds. Require at least the staked value to
            // be re-locked.
            let relocked = tx
                .outputs
                .iter()
                .filter(|o| o.value > 0 && o.script_pubkey == staked.script_pubkey)
                .map(|o| o.value)
                .fold(0u64, u64::saturating_add);
            if relocked < staked.value {
                return Err(NodeError::InvalidTransaction(format!(
                    "P2CS coinstake re-locks {} satoshis below the staked value {}",
                    relocked, staked.value
                )));
            }
        }
    }

    if governance_active {
        if let Some(staked) = &stake_input {
            chain.governance.transfer_stake_lock(tx, staked, height)?;
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
    let height_index = chain.height_index.clone();
    let tx_index = chain.tx_index.clone();
    let utxo_set = chain.utxo_set.clone();
    let claimed_addresses = chain.claimed_addresses.clone();
    let journals = chain.journals.clone();
    let total_supply = chain.total_supply;
    // `reorganize_to_inner` mutates `total_staked` (rollback reverses each
    // rolled-back block's delta, apply adds each fork block's) before it can
    // fail. Omitting it here would leave the v2 kernel denominator drifted
    // after a failed reorg, so two nodes with identical UTXO sets could
    // disagree on kernel validity — a silent consensus split.
    let total_staked = chain.total_staked;
    let governance = chain.governance.clone();
    let params = chain.params;

    match reorganize_to_inner(chain, new_tip, new_tip_height) {
        Ok(result) => Ok(result),
        Err(error) => {
            chain.height_index = height_index;
            chain.tx_index = tx_index;
            chain.utxo_set = utxo_set;
            chain.claimed_addresses = claimed_addresses;
            chain.journals = journals;
            chain.total_supply = total_supply;
            chain.total_staked = total_staked;
            chain.governance = governance;
            chain.params = params;
            Err(error)
        }
    }
}

fn reorganize_to_inner(
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

    let rollback_depth = chain.best_height().saturating_sub(fork_height) as usize;
    if rollback_depth > chain.journals.len() {
        return Err(NodeError::Chain(format!(
            "Reorg depth {} exceeds available rollback history {}",
            rollback_depth,
            chain.journals.len()
        )));
    }

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
        if block.header.is_pos() && block.header.utxo_root != journal.utxo_root {
            return Err(NodeError::InvalidBlock(format!(
                "UTXO root {} does not match computed post-state root {}",
                hex::encode(block.header.utxo_root),
                hex::encode(journal.utxo_root)
            )));
        }
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
            block,
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
