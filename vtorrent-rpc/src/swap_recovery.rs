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
    }
    Ok(Some(record))
}

/// Durably save the complete local recovery state before admitting or broadcasting funds.
pub async fn persist(
    state: &AppState,
    order: &SwapOrder,
    swap: Option<&SwapState>,
) -> RpcResult<()> {
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
                for (old_id, new_id) in [
                    (old.vtr_funding_txid, new.vtr_funding_txid),
                    (old.btc_funding_txid, new.btc_funding_txid),
                    (old.vtr_claim_txid, new.vtr_claim_txid),
                    (old.vtr_refund_txid, new.vtr_refund_txid),
                    (old.btc_claim_txid, new.btc_claim_txid),
                    (old.btc_refund_txid, new.btc_refund_txid),
                ] {
                    if old_id.is_some() && old_id != new_id {
                        return Err(failure("refusing to replace a prepared transaction"));
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
            if current.hash_lock != saved.hash_lock
                || (current.vtr_funding_txid.is_some()
                    && saved.vtr_funding_txid.is_some()
                    && current.vtr_funding_txid != saved.vtr_funding_txid)
                || (current.btc_funding_txid.is_some()
                    && saved.btc_funding_txid.is_some()
                    && current.btc_funding_txid != saved.btc_funding_txid)
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
                        vtr_funding_tx,
                        vtr_claim_tx,
                        vtr_refund_tx
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

/// Reject invalid signatures or missing inputs before reserving a recovery action.
pub async fn validate_vtr(
    state: &AppState,
    transaction: &vtorrent_node::block::Transaction,
) -> RpcResult<()> {
    let chain = state.chain.lock().await;
    if chain.get_transaction(&transaction.txid()).is_some() {
        return Ok(());
    }
    if chain.compute_tx_fee(transaction).is_none() {
        return Err(RpcError::BadRequest(
            "VTR recovery transaction inputs are missing or spent".into(),
        ));
    }
    let height = chain.best_height();
    let timestamp = chain
        .get_block_at_height(height)
        .map(|b| b.header.timestamp)
        .unwrap_or(0);
    chain
        .verify_tx_scripts(transaction, height, timestamp)
        .map_err(|e| RpcError::BadRequest(format!("VTR recovery transaction is invalid: {e}")))
}

/// Re-submit an identical recovered transaction after revalidating it against the chain.
pub async fn submit_vtr(
    state: &AppState,
    transaction: &vtorrent_node::block::Transaction,
) -> RpcResult<()> {
    let chain = state.chain.lock().await;
    let txid = transaction.txid();
    if chain.get_transaction(&txid).is_some() {
        return Ok(());
    }
    let mut mempool = state.mempool.lock().await;
    if mempool.get_transaction(&txid).is_none() {
        mempool
            .admit_with_chain_fee(&chain, transaction.clone())
            .map_err(failure)?;
    }
    if let Some(sender) = &state.tx_submit {
        let _ = sender.try_send(transaction.clone());
    }
    Ok(())
}
