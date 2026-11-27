use crate::error::{RpcError, RpcResult};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use vtorrent_node::atomic_swap::{SwapOrder, SwapState};
use vtorrent_wallet::encryption::{decrypt_wallet, encrypt_wallet, EncryptedWallet};
use zeroize::{Zeroize, Zeroizing};

const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Record {
    version: u8,
    order: SwapOrder,
    swap: Option<SwapState>,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.order.preimage.zeroize();
        if let Some(swap) = &mut self.swap {
            swap.preimage.zeroize();
            if let Some(tx) = &mut swap.vtr_claim_tx {
                for input in &mut tx.inputs {
                    input.script_sig.zeroize();
                }
            }
        }
    }
}

fn failure(message: impl std::fmt::Display) -> RpcError {
    RpcError::Internal(format!("Swap recovery: {message}"))
}

async fn context(state: &AppState, wif: &str) -> RpcResult<Option<(PathBuf, Zeroizing<String>)>> {
    let Some(root) = &state.swap_recovery_dir else {
        return Ok(None);
    };
    let key = vtorrent_core::keys::PrivateKey::from_wif(wif).map_err(failure)?;
    let public = secp256k1::PublicKey::from_secret_key(
        &secp256k1::Secp256k1::new(),
        &secp256k1::SecretKey::from_slice(key.as_bytes()).map_err(failure)?,
    );
    let genesis = state
        .chain
        .lock()
        .await
        .get_block_at_height(0)
        .ok_or_else(|| failure("genesis unavailable"))?
        .hash();
    let network = *state.btc_network.read().await;
    let namespace = hex::encode(Sha256::digest(
        format!("{public}:{}:{network}", hex::encode(genesis)).as_bytes(),
    ));
    let password = Zeroizing::new(format!("vtorrent-swap-recovery-v1:{namespace}:{wif}"));
    Ok(Some((root.join(namespace), password)))
}

fn read_record(path: &Path, password: &str) -> RpcResult<Option<Record>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(failure(e)),
    };
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(failure("invalid or oversized record file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(failure)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(failure("oversized record"));
    }
    let encrypted: EncryptedWallet =
        serde_json::from_slice(&bytes).map_err(|_| failure("invalid encrypted record"))?;
    let plaintext = decrypt_wallet(&encrypted, password)
        .map_err(|_| failure("record authentication failed"))?;
    let record: Record =
        serde_json::from_slice(&plaintext).map_err(|_| failure("invalid record payload"))?;
    if record.version != 1
        || path.file_stem().and_then(|s| s.to_str()) != Some(&hex::encode(record.order.order_id))
    {
        return Err(failure("record version or order identity mismatch"));
    }
    if let Some(secret) = record.order.preimage {
        if record.order.hash_lock != Some(Sha256::digest(secret).into()) {
            return Err(failure("order secret mismatch"));
        }
    }
    if let Some(swap) = &record.swap {
        if swap
            .preimage
            .is_some_and(|secret| <[u8; 32]>::from(Sha256::digest(secret)) != swap.hash_lock)
        {
            return Err(failure("swap secret mismatch"));
        }
        if swap.order_id != record.order.order_id
            || Some(swap.hash_lock) != record.order.hash_lock
            || swap.vtr_funding_txid != record.order.funding_txid
        {
            return Err(failure("inconsistent swap metadata"));
        }
        for (tx, txid) in [
            (&swap.vtr_funding_tx, swap.vtr_funding_txid),
            (&swap.vtr_claim_tx, swap.vtr_claim_txid),
            (&swap.vtr_refund_tx, swap.vtr_refund_txid),
        ] {
            if tx.as_ref().is_some_and(|tx| Some(tx.txid()) != txid) {
                return Err(failure("signed transaction identity mismatch"));
            }
        }
        crate::refund_lineage::validate(&record.order, swap)?;
        crate::btc_claim_lineage::validate(swap)?;
    }
    Ok(Some(record))
}

/// Durably save the complete local recovery state before admitting or broadcasting funds.
pub async fn persist(
    state: &AppState,
    order: &SwapOrder,
    swap: Option<&SwapState>,
) -> RpcResult<()> {
    persist_for_wallet(state, order, swap, None).await
}

/// Remove the recovery record for an order whose funding admission failed.
pub(crate) async fn remove(state: &AppState, order_id: &str) -> RpcResult<()> {
    // Nothing was persisted when recovery is not configured, so there is
    // nothing to remove and no identity check is needed.
    if state.swap_recovery_dir.is_none() {
        return Ok(());
    }
    let Some(wif) = state.wallet_wif.read().await.clone() else {
        return Err(failure(
            "wallet is locked; cannot remove the failed funding recovery record",
        ));
    };
    let (directory, _password) = context(state, &wif)
        .await?
        .ok_or_else(|| failure("swap recovery directory disappeared during removal"))?;
    let path = directory.join(format!("{order_id}.json"));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(format!(
            "could not remove failed funding recovery record: {error}"
        ))),
    }
}

pub(crate) async fn persist_for_wallet(
    state: &AppState,
    order: &SwapOrder,
    swap: Option<&SwapState>,
    expected_wif: Option<&str>,
) -> RpcResult<()> {
    persist_for_wallet_inner(state, order, swap, expected_wif, false).await
}

/// Persist a new signed VTR claim that replaces the previous claim with a
/// higher fee. The replacement must spend the same HTLC output and pay the
/// same taker script; the recovery guard allows only that exact transition.
pub(crate) async fn persist_vtr_claim_replacement(
    state: &AppState,
    order: &SwapOrder,
    swap: &SwapState,
) -> RpcResult<()> {
    persist_for_wallet_inner(state, order, Some(swap), None, true).await
}

async fn persist_for_wallet_inner(
    state: &AppState,
    order: &SwapOrder,
    swap: Option<&SwapState>,
    expected_wif: Option<&str>,
    allow_vtr_claim_replacement: bool,
) -> RpcResult<()> {
    if let Some(swap) = swap {
        crate::refund_lineage::validate(order, swap)?;
        crate::btc_claim_lineage::validate(swap)?;
    }
    if state.swap_recovery_dir.is_none() {
        return Ok(());
    }
    if !state.is_wallet_unlocked().await {
        return Err(RpcError::WalletLocked);
    }
    let wif = state
        .wallet_wif
        .read()
        .await
        .clone()
        .ok_or(RpcError::WalletLocked)?;
    if expected_wif.is_some_and(|expected| expected != wif.as_str()) {
        return Err(RpcError::BadRequest(
            "Wallet changed during swap observation; retry".into(),
        ));
    }
    let Some((directory, password)) = context(state, &wif).await? else {
        return Ok(());
    };
    let record = Record {
        version: 1,
        order: order.clone(),
        swap: swap.cloned(),
    };
    let guard = state.swap_recovery_lock.clone().lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        std::fs::create_dir_all(&directory).map_err(failure)?;
        let path = directory.join(format!("{}.json", hex::encode(record.order.order_id)));
        // Never overwrite a corrupt or differently encrypted recovery record.
        if let Some(previous) = read_record(&path, &password)? {
            let old_terms =
                vtorrent_node::atomic_swap::OrderAnnouncement::from_order(&previous.order);
            let new_terms =
                vtorrent_node::atomic_swap::OrderAnnouncement::from_order(&record.order);
            if serde_json::to_vec(&old_terms).map_err(failure)?
                != serde_json::to_vec(&new_terms).map_err(failure)?
                || previous
                    .order
                    .preimage
                    .is_some_and(|secret| record.order.preimage != Some(secret))
                || previous
                    .order
                    .funding_txid
                    .is_some_and(|txid| record.order.funding_txid != Some(txid))
                || previous
                    .order
                    .taker_address
                    .as_ref()
                    .is_some_and(|address| record.order.taker_address.as_ref() != Some(address))
            {
                return Err(failure("refusing to replace existing contract terms"));
            }
            if let Some(old) = &previous.swap {
                let new = record
                    .swap
                    .as_ref()
                    .ok_or_else(|| failure("refusing to discard swap recovery state"))?;
                if !crate::refund_lineage::is_prefix(
                    &old.vtr_refund_replacements,
                    &new.vtr_refund_replacements,
                ) {
                    return Err(failure("refusing to replace or discard refund lineage"));
                }
                if !crate::btc_claim_lineage::is_prefix(
                    &old.btc_claim_replacements,
                    &new.btc_claim_replacements,
                ) {
                    return Err(failure("refusing to replace or discard claim lineage"));
                }
                for (old_id, new_id) in [
                    (old.vtr_funding_txid, new.vtr_funding_txid),
                    (old.btc_funding_txid, new.btc_funding_txid),
                    (old.vtr_claim_txid, new.vtr_claim_txid),
                    (old.vtr_refund_txid, new.vtr_refund_txid),
                    (old.btc_claim_txid, new.btc_claim_txid),
                    (old.btc_refund_txid, new.btc_refund_txid),
                ] {
                    if old_id.is_some() && old_id != new_id {
                        let replaces_claim = allow_vtr_claim_replacement
                            && old_id == old.vtr_claim_txid
                            && old
                                .vtr_claim_tx
                                .as_ref()
                                .zip(new.vtr_claim_tx.as_ref())
                                .is_some_and(|(old_tx, new_tx)| vtr_claim_replaces(old_tx, new_tx));
                        if !replaces_claim {
                            return Err(failure("refusing to replace a prepared transaction"));
                        }
                    }
                }
            }
        }
        let mut plaintext = Zeroizing::new(Vec::new());
        serde_json::to_writer(&mut *plaintext, &record).map_err(failure)?;
        let encrypted = encrypt_wallet(&plaintext, &password).map_err(failure)?;
        let bytes = serde_json::to_vec(&encrypted).map_err(failure)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(failure("record exceeds size limit"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(failure)?;
        temporary.write_all(&bytes).map_err(failure)?;
        temporary.as_file().sync_all().map_err(failure)?;
        temporary.persist(&path).map_err(failure)?;
        #[cfg(unix)]
        for path in directory.ancestors().take(3) {
            std::fs::File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(failure)?;
        }
        Ok(())
    })
    .await
    .map_err(failure)?
}

/// Authenticate all recovery records before installing any of them on wallet unlock.
pub async fn restore_with_wif(state: &AppState, wif: &str) -> RpcResult<()> {
    let Some((directory, password)) = context(state, wif).await? else {
        return Ok(());
    };
    let records = {
        let guard = state.swap_recovery_lock.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(failure(e)),
            };
            let mut records = Vec::new();
            for entry in entries {
                let path = entry.map_err(failure)?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                if records.len() >= 10_000 {
                    return Err(failure("too many records"));
                }
                records.push(
                    read_record(&path, &password)?.ok_or_else(|| failure("record disappeared"))?,
                );
            }
            Ok(records)
        })
        .await
        .map_err(failure)??
    };
    let mut orders = state.order_book.write().await;
    let mut swaps = state.swaps.write().await;
    for record in &records {
        if let (Some(saved), Some(current)) =
            (&record.swap, swaps.get(&hex::encode(record.order.order_id)))
        {
            if !crate::refund_lineage::is_prefix(
                &current.vtr_refund_replacements,
                &saved.vtr_refund_replacements,
            ) && !crate::refund_lineage::is_prefix(
                &saved.vtr_refund_replacements,
                &current.vtr_refund_replacements,
            ) {
                return Err(failure("recovered refund lineage conflicts with memory"));
            }
            if !crate::btc_claim_lineage::is_prefix(
                &current.btc_claim_replacements,
                &saved.btc_claim_replacements,
            ) && !crate::btc_claim_lineage::is_prefix(
                &saved.btc_claim_replacements,
                &current.btc_claim_replacements,
            ) {
                return Err(failure("recovered claim lineage conflicts with memory"));
            }
            if current.hash_lock != saved.hash_lock
                || (current.vtr_refund_txid.is_some()
                    && saved.vtr_refund_txid.is_some()
                    && current.vtr_refund_txid != saved.vtr_refund_txid)
                || (current.vtr_funding_txid.is_some()
                    && saved.vtr_funding_txid.is_some()
                    && current.vtr_funding_txid != saved.vtr_funding_txid)
                || (current.btc_funding_txid.is_some()
                    && saved.btc_funding_txid.is_some()
                    && current.btc_funding_txid != saved.btc_funding_txid)
                || (current.btc_claim_txid.is_some()
                    && saved.btc_claim_txid.is_some()
                    && current.btc_claim_txid != saved.btc_claim_txid)
            {
                return Err(failure(
                    "recovered contract conflicts with in-memory funding",
                ));
            }
        }
    }
    for record in &records {
        orders.restore_order(record.order.clone());
        if let Some(swap) = &record.swap {
            match swaps.entry(hex::encode(swap.order_id)) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(swap.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let current = entry.get_mut();
                    if current.vtr_refund_replacements.len() < swap.vtr_refund_replacements.len() {
                        current.vtr_refund_replacements = swap.vtr_refund_replacements.clone();
                    }
                    // Claim replacements are append-only, so adopt a longer
                    // chain only when it extends the one in memory, and take
                    // its raw transaction so the next fee calculation prices
                    // the latest replacement.
                    if current.btc_claim_replacements.len() < swap.btc_claim_replacements.len() {
                        if !crate::btc_claim_lineage::is_prefix(
                            &current.btc_claim_replacements,
                            &swap.btc_claim_replacements,
                        ) {
                            return Err(failure("recovered claim lineage conflicts with memory"));
                        }
                        current.btc_claim_replacements = swap.btc_claim_replacements.clone();
                        current.btc_claim_raw = swap.btc_claim_raw.clone();
                    } else if !crate::btc_claim_lineage::is_prefix(
                        &swap.btc_claim_replacements,
                        &current.btc_claim_replacements,
                    ) {
                        return Err(failure("recovered claim lineage conflicts with memory"));
                    }
                    let pending_btc = current.status
                        == vtorrent_node::atomic_swap::SwapStatus::BtcFunding
                        || swap.status == vtorrent_node::atomic_swap::SwapStatus::BtcFunding;
                    macro_rules! fill {
                        ($($field:ident),*) => { $(if current.$field.is_none() { current.$field = swap.$field.clone(); })* };
                    }
                    if current.btc_funding_txid.is_none() {
                        current.btc_amount = swap.btc_amount;
                        current.btc_expiry = swap.btc_expiry;
                    }
                    fill!(
                        preimage,
                        vtr_funding_txid,
                        btc_funding_txid,
                        maker_btc_address,
                        taker_btc_refund_address,
                        vtr_claim_txid,
                        btc_claim_txid,
                        vtr_refund_txid,
                        btc_refund_txid,
                        btc_refund_raw,
                        btc_funding_raw,
                        btc_claim_raw,
                        vtr_funding_tx,
                        vtr_claim_tx,
                        vtr_refund_tx,
                        vtr_observation,
                        btc_observation
                    );
                    current.refresh_status();
                    if pending_btc
                        && current.status == vtorrent_node::atomic_swap::SwapStatus::BtcFunded
                    {
                        current.status = vtorrent_node::atomic_swap::SwapStatus::BtcFunding;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether `new` is a valid fee-bumped replacement of `old` for the same HTLC.
///
/// Both must spend the same outpoint, pay the same taker script, and differ
/// only by a lower output value (higher fee). Only one can ever confirm.
pub(crate) fn vtr_claim_replaces(
    old: &vtorrent_node::block::Transaction,
    new: &vtorrent_node::block::Transaction,
) -> bool {
    old.inputs.len() == 1
        && new.inputs.len() == 1
        && old.outputs.len() == 1
        && new.outputs.len() == 1
        && old.inputs[0].prev_txid == new.inputs[0].prev_txid
        && old.inputs[0].prev_vout == new.inputs[0].prev_vout
        && old.outputs[0].script_pubkey == new.outputs[0].script_pubkey
        && new.outputs[0].value < old.outputs[0].value
        && old.tx_type == new.tx_type
        && old.version == new.version
}

/// Reject invalid signatures or missing inputs before reserving a recovery action.
pub async fn validate_vtr(
    state: &AppState,
    transaction: &vtorrent_node::block::Transaction,
) -> RpcResult<()> {
    let chain = state.chain.lock().await;
    if chain.get_transaction_owned(&transaction.txid()).is_some() {
        return Ok(());
    }
    if chain.compute_tx_fee(transaction).is_none() {
        return Err(RpcError::BadRequest(
            "VTR recovery transaction inputs are missing or spent".into(),
        ));
    }
    let height = chain.best_height();
    let timestamp = chain
        .get_header_at_height(height)
        .map(|h| h.timestamp)
        .unwrap_or(0);
    chain
        .verify_tx_scripts(transaction, height, timestamp)
        .map_err(|e| RpcError::BadRequest(format!("VTR recovery transaction is invalid: {e}")))
}

/// Re-submit an identical recovered transaction after revalidating it against the chain.
pub async fn submit_vtr(
    state: &AppState,
    transaction: &vtorrent_node::block::Transaction,
) -> RpcResult<bool> {
    let chain = state.chain.lock().await;
    let txid = transaction.txid();
    if chain.get_transaction_owned(&txid).is_some() {
        return Ok(true);
    }
    let mut mempool = state.mempool.lock().await;
    let mut admitted = true;
    if mempool.get_transaction(&txid).is_none() {
        if let Err(error) = mempool.admit_with_chain_fee(&chain, transaction.clone()) {
            // A local conflict (for example a maker refund already in this
            // mempool) must not prevent relaying the claim to peers that may
            // not have the conflicting transaction. The chain check above
            // still rejects a claim whose inputs are already spent.
            tracing::warn!(
                txid = %hex::encode(txid),
                error = %error,
                "VTR recovery transaction was not admitted locally; broadcasting it anyway"
            );
            admitted = false;
        }
    }
    if let Some(sender) = &state.tx_submit {
        let _ = sender.try_send(transaction.clone());
    }
    Ok(admitted)
}

/// Submit a refund-like VTR transaction that must be admissible locally
/// before it is relayed. A conflict is returned to the caller instead of
/// broadcasting a transaction that can strand the other leg.
pub async fn submit_vtr_required(
    state: &AppState,
    transaction: &vtorrent_node::block::Transaction,
) -> RpcResult<()> {
    let chain = state.chain.lock().await;
    let txid = transaction.txid();
    if chain.get_transaction_owned(&txid).is_some() {
        return Ok(());
    }
    let mut mempool = state.mempool.lock().await;
    if mempool.get_transaction(&txid).is_none() {
        mempool
            .admit_with_chain_fee(&chain, transaction.clone())
            .map_err(|error| {
                RpcError::BadRequest(format!("VTR refund is not admissible: {error}"))
            })?;
    }
    drop(mempool);
    drop(chain);
    if let Some(sender) = &state.tx_submit {
        // The transaction is admitted locally, so a failed relay is
        // recoverable by retrying the identical request.
        let _ = sender.try_send(transaction.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_node::block::{Transaction, TxInput, TxOutput, TxType};

    fn claim(value: u64, prev_txid: [u8; 32], script: Vec<u8>) -> Transaction {
        Transaction {
            version: 1,
            tx_type: TxType::AtomicSwap,
            inputs: vec![TxInput {
                prev_txid,
                prev_vout: 0,
                script_sig: vec![1],
                sequence: u32::MAX - 2,
            }],
            outputs: vec![TxOutput {
                value,
                script_pubkey: script,
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        }
    }

    #[test]
    fn vtr_claim_replacement_requires_same_outpoint_and_a_higher_fee() {
        let old = claim(1_000, [1u8; 32], vec![0x51]);
        let higher_fee = claim(900, [1u8; 32], vec![0x51]);
        assert!(vtr_claim_replaces(&old, &higher_fee));
        assert!(!vtr_claim_replaces(
            &old,
            &claim(1_000, [1u8; 32], vec![0x51])
        ));
        assert!(!vtr_claim_replaces(
            &old,
            &claim(1_100, [1u8; 32], vec![0x51])
        ));
        assert!(!vtr_claim_replaces(
            &old,
            &claim(900, [2u8; 32], vec![0x51])
        ));
        assert!(!vtr_claim_replaces(
            &old,
            &claim(900, [1u8; 32], vec![0x52])
        ));
    }
}
