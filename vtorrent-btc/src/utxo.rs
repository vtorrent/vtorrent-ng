//! UTXO set tracking for the wallet's addresses.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Hard cap on journalled heights. Independent of `--btc-header-retention`, so
/// an unbounded header chain cannot produce an unbounded journal.
pub(crate) const MAX_UNDO_ENTRIES: usize = 200_000;

/// A spendable output owned by the wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    /// Transaction id (hex).
    pub txid: String,
    /// Output index.
    pub vout: u32,
    /// Value in satoshis.
    pub value: u64,
    /// The address this output pays to.
    pub address: String,
    /// Block height where this output was confirmed (0 = mempool).
    pub height: u32,
}

/// Reservation state an outpoint had before a block removed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PriorState {
    /// The outpoint was spendable.
    Free,
    /// The outpoint was held for a swap; the record restores it as reserved.
    Reserved(Utxo),
    /// The outpoint had already been spent while reserved.
    SpentReserved,
}

/// An output a block removed from the set, with the state to restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemovedOutput {
    pub utxo: Utxo,
    pub prior: PriorState,
}

/// Per-height record of what one block did to the set, so a reorg can undo it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub height: u32,
    pub created: Vec<Utxo>,
    pub removed: Vec<RemovedOutput>,
}

/// A wallet-local spend the scan has not observed on chain yet.
///
/// A send removes its inputs immediately, before any block can confirm them, so
/// its removal belongs to no scanned height. Keeping it out of the height-indexed
/// journal is what makes the boundary case correct: a spend built at cursor `C`
/// can confirm at any `H >= C`, so a fork `F` with `C <= F < H` would reorg the
/// spend out while the journalled height `C` survives [`UtxoSet::undo_above`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingRemoval {
    /// The outpoint the send removed, with the state to restore.
    pub removed: RemovedOutput,
    /// Transaction id the send broadcast. The scan retires the record when it
    /// sees this transaction spend the outpoint.
    pub txid: String,
    /// Scan cursor when the send was built.
    pub height: u32,
}

/// Public contract terms needed to recover the BTC refund after restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapContract {
    pub order_id: String,
    pub funding_txid: String,
    pub hash_lock: [u8; 32],
    pub recipient: String,
    pub refund_address: String,
    pub expiry: u32,
    pub amount: u64,
    pub refund_raw: Option<Vec<u8>>,
    /// The wallet outpoint reserved to fund this swap, so the reservation can
    /// be released after the funding tx is confirmed dead. `None` for
    /// contracts persisted before this field existed (never auto-released).
    #[serde(default)]
    pub input_txid: Option<String>,
    #[serde(default)]
    pub input_vout: Option<u32>,
}

/// In-memory UTXO set with optional disk persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UtxoSet {
    utxos: Vec<Utxo>,
    #[serde(default)]
    reserved: std::collections::BTreeSet<(String, u32)>,
    #[serde(default)]
    reserved_utxos: Vec<Utxo>,
    #[serde(default)]
    spent_reserved: std::collections::BTreeSet<(String, u32)>,
    #[serde(default)]
    pending_swap_transactions: std::collections::BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    swap_contracts: std::collections::BTreeMap<String, SwapContract>,
    #[serde(default)]
    next_index: u32,
    #[serde(default)]
    last_scanned_height: u32,
    #[serde(default)]
    rescan_required: bool,
    /// Earliest height this wallet intends to cover, recorded when it starts
    /// scanning. A birthday below the retained floor means history is missing.
    #[serde(default)]
    birthday: Option<u32>,
    /// Hash of the tip the scan cursor reached. Persisted, always cleared on
    /// load, and read by no decision today: it is the seam for a future
    /// lineage-persistence design, not a coverage check.
    #[serde(default)]
    last_scanned_tip: Option<[u8; 32]>,
    /// Sparse ascending journal of chain-derived changes, for rollback.
    #[serde(default)]
    undo: Vec<UndoEntry>,
    /// Wallet-local spends the scan has not observed yet, appended by a send.
    /// Independent of the journal's height ordering: a spend built at the scan
    /// cursor can confirm at or above it.
    #[serde(default)]
    pending_removals: Vec<PendingRemoval>,
    /// Reservation spends observed by this process's reconciliation scan,
    /// mapped to the reserved record they replaced and the chain height the
    /// observation was made at. Live-process only: a covered rollback restores
    /// the observations made above its fork.
    #[serde(default, skip)]
    observed_reservation_spends: std::collections::BTreeMap<(String, u32), (Utxo, u32)>,
    /// Set once any confirmed output has been tracked, so a floor change can
    /// never silently adopt a height this wallet may have held funds below.
    #[serde(default)]
    seen_funds: bool,
}

impl UtxoSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_index(&self) -> u32 {
        self.next_index
    }

    pub fn set_next_index(&mut self, index: u32) {
        self.next_index = index;
    }

    pub fn last_scanned_height(&self) -> u32 {
        self.last_scanned_height
    }

    pub fn birthday(&self) -> Option<u32> {
        self.birthday
    }

    pub fn set_birthday(&mut self, birthday: Option<u32>) {
        self.birthday = birthday;
    }

    pub fn set_last_scanned_height(&mut self, height: u32) {
        self.last_scanned_height = height;
    }

    pub fn last_scanned_tip(&self) -> Option<[u8; 32]> {
        self.last_scanned_tip
    }

    pub fn set_last_scanned_tip(&mut self, tip: Option<[u8; 32]>) {
        self.last_scanned_tip = tip;
    }

    pub(crate) fn rescan_required(&self) -> bool {
        self.rescan_required
    }

    pub(crate) fn set_rescan_required(&mut self, required: bool) {
        self.rescan_required = required;
    }

    /// Insert a spendable output.
    ///
    /// Returns true only when the output was newly recorded. An outpoint the
    /// set already holds — or one currently reserved — is left untouched.
    pub fn add(&mut self, utxo: Utxo) -> bool {
        if self.reserved.contains(&(utxo.txid.clone(), utxo.vout)) {
            return false;
        }
        if !self
            .utxos
            .iter()
            .any(|u| u.txid == utxo.txid && u.vout == utxo.vout)
        {
            self.seen_funds = true;
            self.utxos.push(utxo);
            return true;
        }
        false
    }

    /// Whether this wallet has ever tracked a confirmed output.
    pub fn has_seen_funds(&self) -> bool {
        self.seen_funds
    }

    pub fn remove(&mut self, txid: &str, vout: u32) {
        self.utxos.retain(|u| !(u.txid == txid && u.vout == vout));
    }

    /// Clear chain-derived UTXOs while preserving recovery metadata.
    ///
    /// A reorg invalidates confirmed outputs, but reservations, signed swap
    /// transactions, contracts, and the address index are wallet state, not
    /// chain state. Keeping them prevents a rescan from making a reserved
    /// input selectable again and preserves recovery after restart.
    pub fn clear_chain_utxos(&mut self) {
        self.utxos.clear();
    }

    /// Drop chain-derived UTXOs confirmed above `height`.
    ///
    /// A reorg only invalidates blocks above the last common ancestor, so the
    /// rescan can resume from there instead of re-deriving the whole retained
    /// window. Mempool entries (height 0) and wallet metadata are untouched.
    pub fn rollback_above(&mut self, height: u32) {
        self.utxos.retain(|utxo| utxo.height <= height);
    }

    /// The wallet's record of an outpoint, whether it is spendable in `utxos`
    /// or held in `reserved_utxos`. Reserved inputs have already left the
    /// spendable set, so a scan that sees one spent still needs its full record
    /// to journal the removal.
    pub fn outpoint(&self, txid: &str, vout: u32) -> Option<Utxo> {
        self.utxos
            .iter()
            .chain(self.reserved_utxos.iter())
            .find(|utxo| utxo.txid == txid && utxo.vout == vout)
            .cloned()
    }

    /// The undo journal, oldest entry first.
    pub fn undo_entries(&self) -> &[UndoEntry] {
        &self.undo
    }

    /// Replace the journal verbatim, bypassing [`Self::push_undo`]'s ordering
    /// guard, so tests can construct an unordered or gapped journal.
    #[cfg(test)]
    pub(crate) fn set_undo_raw(&mut self, entries: Vec<UndoEntry>) {
        self.undo = entries;
    }

    /// Record one height's changes, keeping the journal ascending and bounded.
    ///
    /// An entry for the same height as the last one is merged, because a block
    /// can touch the wallet through several transactions. An entry for a height
    /// *below* the tail is ignored: it can only come from a reconciliation scan
    /// re-reading old history, and appending it would make [`Self::undo_above`]
    /// reject the whole journal as non-ascending. The oldest entries are then
    /// trimmed so the journal never exceeds [`MAX_UNDO_ENTRIES`], whatever the
    /// header retention and whichever scan path appends.
    pub fn push_undo(&mut self, entry: UndoEntry) {
        if let Some(last) = self.undo.last_mut() {
            if last.height == entry.height {
                last.created.extend(entry.created);
                last.removed.extend(entry.removed);
                return;
            }
            if last.height > entry.height {
                return;
            }
        }
        self.undo.push(entry);
        if self.undo.len() > MAX_UNDO_ENTRIES {
            self.undo.drain(..self.undo.len() - MAX_UNDO_ENTRIES);
        }
    }

    /// Drop every recovery record built on the chain state being reset.
    ///
    /// Reservation observations belong here too: they describe a spend on the
    /// abandoned branch, and replaying one after a full reset would re-reserve
    /// an outpoint the new branch may already have spent.
    pub fn clear_undo(&mut self) {
        self.undo.clear();
        self.pending_removals.clear();
        self.observed_reservation_spends.clear();
    }

    /// The wallet-local spends the scan has not observed yet.
    pub fn pending_removals(&self) -> &[PendingRemoval] {
        &self.pending_removals
    }

    /// Record the outpoints a wallet-local send removed.
    ///
    /// Each record carries the spending transaction and the cursor the send was
    /// built at, so the scan can retire it when the spend confirms and a covered
    /// rollback can restore it without consulting the journal's heights.
    pub fn record_pending_removals(&mut self, utxos: &[Utxo], txid: &str, height: u32) {
        for utxo in utxos {
            if self.pending_removals.iter().any(|pending| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            }) {
                continue;
            }
            let prior = if let Some(reserved) = self.reserved_utxo(&utxo.txid, utxo.vout) {
                PriorState::Reserved(reserved.clone())
            } else if self.was_reserved_spent(&utxo.txid, utxo.vout) {
                PriorState::SpentReserved
            } else {
                PriorState::Free
            };
            self.pending_removals.push(PendingRemoval {
                removed: RemovedOutput {
                    utxo: utxo.clone(),
                    prior,
                },
                txid: txid.to_owned(),
                height,
            });
        }
        if self.pending_removals.len() > MAX_UNDO_ENTRIES {
            self.pending_removals
                .drain(..self.pending_removals.len() - MAX_UNDO_ENTRIES);
        }
    }

    /// Retire the pending record for an outpoint the scan just saw spent.
    ///
    /// The spending transaction is matched first so the record is retired by the
    /// send it belongs to. A record for the same outpoint is still taken when
    /// the txid differs, because an outpoint can only be spent once: a
    /// replacement transaction then keeps the fold complete instead of losing
    /// the removal.
    pub fn take_pending_removal(
        &mut self,
        spending_txid: &str,
        txid: &str,
        vout: u32,
    ) -> Option<RemovedOutput> {
        let matches = |pending: &PendingRemoval| {
            pending.removed.utxo.txid == txid && pending.removed.utxo.vout == vout
        };
        let index = self
            .pending_removals
            .iter()
            .position(|pending| matches(pending) && pending.txid == spending_txid)
            .or_else(|| self.pending_removals.iter().position(matches))?;
        Some(self.pending_removals.remove(index).removed)
    }

    /// Cancel the removal state a failed broadcast must not leave behind.
    ///
    /// A send no longer journals at the cursor, so the matching record is a
    /// pending removal. A removal a scan already journalled is cancelled too,
    /// so the send never happened as far as a later rollback is concerned.
    pub fn cancel_local_removals(&mut self, utxos: &[Utxo]) {
        for entry in &mut self.undo {
            entry.removed.retain(|removed| {
                !utxos
                    .iter()
                    .any(|utxo| removed.utxo.txid == utxo.txid && removed.utxo.vout == utxo.vout)
            });
        }
        self.pending_removals.retain(|pending| {
            !utxos.iter().any(|utxo| {
                pending.removed.utxo.txid == utxo.txid && pending.removed.utxo.vout == utxo.vout
            })
        });
    }

    /// Remember that this process's reconciliation scan observed `utxo` spent
    /// while it was reserved, so a covered rollback can put the reservation
    /// back instead of leaving `spent_reserved` behind forever.
    ///
    /// `observed_at` is the chain height the observation was made at. A
    /// rollback only restores observations above its fork: a spend observed
    /// below the fork happened on the surviving branch and is still true.
    pub(crate) fn record_reservation_spend(&mut self, utxo: Utxo, observed_at: u32) {
        self.observed_reservation_spends
            .insert((utxo.txid.clone(), utxo.vout), (utxo, observed_at));
    }

    /// Drop journal entries below `floor`, then keep at most `max_entries`
    /// of the newest ones.
    ///
    /// Pending removals are never pruned by height: a record is self-contained
    /// and may be the only copy of a send whose confirming spend has not been
    /// seen. Dropping it by broadcast height would leave a covered rollback
    /// unable to restore the input. Only the count cap bounds them.
    pub fn prune_undo(&mut self, floor: u32, max_entries: usize) {
        self.undo.retain(|entry| entry.height >= floor);
        if self.undo.len() > max_entries {
            self.undo.drain(..self.undo.len() - max_entries);
        }
        if self.pending_removals.len() > max_entries {
            self.pending_removals
                .drain(..self.pending_removals.len() - max_entries);
        }
    }

    /// Roll the set back to `fork` by replaying the journal.
    ///
    /// Returns true only when the journal covers every height above `fork`:
    /// entries must be strictly ascending and the lowest one above the fork
    /// must be `fork + 1`. Otherwise nothing is mutated and the caller falls
    /// back to a full reset.
    ///
    /// A `removed` outpoint the same entry also created is not restored: the
    /// block created and spent it, so undoing the creation must win. Restoring
    /// it would leave a phantom UTXO the post-fork rescan can never remove.
    ///
    /// Pending wallet-local removals and reservation observations are put back
    /// before the replay, not after: an entry that undoes the creation of the
    /// very outpoint they belong to must win, or the rollback would leave a
    /// phantom record behind. A pending spend the new branch still confirms is
    /// removed again by the post-fork scan, so restoring it is self-correcting.
    /// An observation below the fork survives: it recorded a spend on the
    /// branch the rollback keeps.
    pub fn undo_above(&mut self, fork: u32) -> bool {
        if !self.undo_covers(fork) {
            return false;
        }
        for (key, (utxo, observed_at)) in std::mem::take(&mut self.observed_reservation_spends) {
            if observed_at <= fork {
                self.observed_reservation_spends
                    .insert(key, (utxo, observed_at));
                continue;
            }
            self.spent_reserved.remove(&key);
            self.reserve_utxo(utxo);
        }
        for pending in std::mem::take(&mut self.pending_removals) {
            self.restore_removed(pending.removed);
        }
        while let Some(entry) = self.undo.last() {
            if entry.height <= fork {
                break;
            }
            let entry = self.undo.pop().expect("entry just inspected");
            let created_in_block: std::collections::HashSet<(&str, u32)> = entry
                .created
                .iter()
                .map(|utxo| (utxo.txid.as_str(), utxo.vout))
                .collect();
            for removed in entry.removed {
                if created_in_block.contains(&(removed.utxo.txid.as_str(), removed.utxo.vout)) {
                    continue;
                }
                self.restore_removed(removed);
            }
            for created in entry.created {
                self.forget_outpoint(&created.txid, created.vout);
            }
        }
        self.last_scanned_tip = None;
        true
    }

    /// Drop every record of an outpoint: spendable, reserved, and
    /// spent-reserved. Undoing a creation must not leave a phantom reservation
    /// whose funding transaction no longer exists.
    fn forget_outpoint(&mut self, txid: &str, vout: u32) {
        let key = (txid.to_owned(), vout);
        self.utxos.retain(|u| !(u.txid == txid && u.vout == vout));
        self.reserved.remove(&key);
        self.reserved_utxos
            .retain(|u| !(u.txid == txid && u.vout == vout));
        self.spent_reserved.remove(&key);
    }

    fn undo_covers(&self, fork: u32) -> bool {
        let ascending = self
            .undo
            .windows(2)
            .all(|pair| pair[0].height < pair[1].height);
        if !ascending {
            return false;
        }
        self.undo
            .iter()
            .find(|entry| entry.height > fork)
            .is_some_and(|entry| entry.height == fork.saturating_add(1))
    }

    fn restore_removed(&mut self, removed: RemovedOutput) {
        let RemovedOutput { utxo, prior } = removed;
        let key = (utxo.txid.clone(), utxo.vout);
        self.insert_direct(utxo);
        match prior {
            PriorState::Free => {}
            PriorState::Reserved(reserved) => {
                self.spent_reserved.remove(&key);
                // Only re-reserve while something still needs the input held.
                // A released swap contract cannot release it again, and no
                // confirmed spend exists to expire the reservation, so an
                // orphaned reservation would hide the outpoint forever.
                if self.swap_holds_outpoint(&key.0, key.1) {
                    self.reserve_utxo(reserved);
                }
            }
            PriorState::SpentReserved => {
                self.reserved.remove(&key);
                self.reserved_utxos
                    .retain(|u| !(u.txid == key.0 && u.vout == key.1));
                self.utxos.retain(|u| !(u.txid == key.0 && u.vout == key.1));
                self.spent_reserved.insert(key);
            }
        }
    }

    /// Whether a persisted swap contract or a retained signed funding
    /// transaction still spends `txid:vout`.
    fn swap_holds_outpoint(&self, txid: &str, vout: u32) -> bool {
        if self.swap_contracts.values().any(|contract| {
            contract.input_txid.as_deref() == Some(txid) && contract.input_vout == Some(vout)
        }) {
            return true;
        }
        self.pending_swap_transactions.values().any(|raw| {
            bitcoin::consensus::deserialize::<bitcoin::Transaction>(raw).is_ok_and(|tx| {
                tx.input.iter().any(|input| {
                    input.previous_output.txid.to_string() == txid
                        && input.previous_output.vout == vout
                })
            })
        })
    }

    fn insert_direct(&mut self, utxo: Utxo) {
        if self
            .utxos
            .iter()
            .any(|existing| existing.txid == utxo.txid && existing.vout == utxo.vout)
        {
            return;
        }
        self.seen_funds = true;
        self.utxos.push(utxo);
    }

    pub fn reserve(&mut self, txid: &str, vout: u32) {
        self.remove(txid, vout);
        self.reserved.insert((txid.to_owned(), vout));
    }

    /// Reserve a known UTXO and retain its details so a later scan can detect
    /// whether the outpoint was spent.
    pub fn reserve_utxo(&mut self, utxo: Utxo) {
        self.reserve(&utxo.txid, utxo.vout);
        self.reserved_utxos
            .retain(|existing| !(existing.txid == utxo.txid && existing.vout == utxo.vout));
        self.reserved_utxos.push(utxo);
    }

    pub fn reserved_utxo(&self, txid: &str, vout: u32) -> Option<&Utxo> {
        self.reserved_utxos
            .iter()
            .find(|utxo| utxo.txid == txid && utxo.vout == vout)
    }

    /// Mark a reserved outpoint as spent by a confirmed transaction.
    pub fn note_spend(&mut self, txid: &str, vout: u32) -> bool {
        let key = (txid.to_owned(), vout);
        if self.reserved.remove(&key) {
            self.reserved_utxos
                .retain(|utxo| !(utxo.txid == txid && utxo.vout == vout));
            self.spent_reserved.insert(key);
            true
        } else {
            false
        }
    }

    pub fn was_reserved_spent(&self, txid: &str, vout: u32) -> bool {
        self.spent_reserved.contains(&(txid.to_owned(), vout))
    }

    pub fn clear_spent_reserved(&mut self, txid: &str, vout: u32) {
        self.spent_reserved.remove(&(txid.to_owned(), vout));
    }

    /// Release a reservation so the input can be selected again.
    ///
    /// Only safe once the funding transaction is known not to be live (see
    /// `BtcWallet::release_swap_reservation`): releasing an input whose signed
    /// funding tx could still confirm would let a second swap spend it.
    /// Returns true if a reservation was actually removed.
    pub fn release(&mut self, txid: &str, vout: u32) -> bool {
        let key = (txid.to_owned(), vout);
        self.reserved_utxos
            .retain(|utxo| !(utxo.txid == txid && utxo.vout == vout));
        self.reserved.remove(&key)
    }

    /// All persisted swap contracts, for reconciliation.
    pub fn swap_contracts(&self) -> Vec<SwapContract> {
        self.swap_contracts.values().cloned().collect()
    }

    /// Drop a swap contract and its retained signed transaction.
    pub fn remove_swap_contract(&mut self, order_id: &str) -> Option<SwapContract> {
        let contract = self.swap_contracts.remove(order_id)?;
        self.pending_swap_transactions
            .remove(&contract.funding_txid);
        Some(contract)
    }

    pub fn record_swap_transaction(&mut self, txid: String, raw: Vec<u8>) {
        self.pending_swap_transactions.insert(txid, raw);
    }

    pub fn record_swap_contract(&mut self, contract: SwapContract) {
        self.swap_contracts
            .insert(contract.order_id.clone(), contract);
    }

    pub fn swap_contract(&self, order_id: &str) -> Option<&SwapContract> {
        self.swap_contracts.get(order_id)
    }

    pub fn pending_swap_transactions(&self) -> &std::collections::BTreeMap<String, Vec<u8>> {
        &self.pending_swap_transactions
    }

    pub fn total(&self) -> u64 {
        self.utxos.iter().map(|u| u.value).sum()
    }

    pub fn list(&self) -> &[Utxo] {
        &self.utxos
    }

    /// Select UTXOs to cover `amount` (plus `fee`), largest-first.
    pub fn select(&self, amount: u64, fee: u64) -> Option<Vec<Utxo>> {
        if amount > crate::MAX_MONEY_SATS || fee > crate::MAX_MONEY_SATS {
            return None;
        }
        let required = amount.checked_add(fee)?;
        let mut sorted: Vec<Utxo> = self.utxos.clone();
        sorted.sort_by_key(|utxo| std::cmp::Reverse(utxo.value));
        let mut selected = Vec::new();
        let mut sum = 0u64;
        for u in sorted {
            sum = sum.saturating_add(u.value);
            selected.push(u);
            if sum >= required {
                return Some(selected);
            }
        }
        None
    }

    /// Persist the UTXO set to a JSON file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write atomically via a temp file + rename.
        let tmp = path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        std::io::Write::write_all(&mut file, json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        tracing::debug!(
            "UTXO set saved: {} entries → {}",
            self.utxos.len(),
            path.display()
        );
        Ok(())
    }

    /// Load the UTXO set from a JSON file.  Returns an empty set if the
    /// file does not exist.
    ///
    /// The undo journal, its pending removals, and its tip are wallet-local
    /// recovery aids, so a corrupt one is discarded rather than failing the
    /// wallet: the journal loads empty and the wallet falls back to case 2 or 3.
    /// The settled balance is not recovery data, so a corrupt `utxos` field
    /// still fails the load.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let json = match std::fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let mut value: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        sanitize_journal_fields(&mut value);
        let set: UtxoSet = serde_json::from_value(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tracing::debug!(
            "UTXO set loaded: {} entries from {}",
            set.utxos.len(),
            path.display()
        );
        Ok(set)
    }
}

/// Discard a journalled field that does not parse, leaving the rest of the set
/// intact. Only the undo journal, its pending removals, and its tip are
/// tolerated this way.
fn sanitize_journal_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.contains_key("undo")
        && serde_json::from_value::<Vec<UndoEntry>>(object["undo"].clone()).is_err()
    {
        object.insert("undo".into(), serde_json::Value::Array(Vec::new()));
    }
    if object.contains_key("pending_removals")
        && serde_json::from_value::<Vec<PendingRemoval>>(object["pending_removals"].clone())
            .is_err()
    {
        object.insert(
            "pending_removals".into(),
            serde_json::Value::Array(Vec::new()),
        );
    }
    if object.contains_key("last_scanned_tip")
        && serde_json::from_value::<Option<[u8; 32]>>(object["last_scanned_tip"].clone()).is_err()
    {
        object.remove("last_scanned_tip");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utxo(txid: &str, vout: u32, value: u64) -> Utxo {
        Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: "bc1qtest".to_string(),
            height: 100,
        }
    }

    #[test]
    fn test_add_and_total() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("b", 0, 2000));
        assert_eq!(set.total(), 3000);
    }

    #[test]
    fn test_dedup() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        set.add(utxo("a", 0, 1000));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn test_select_covers_amount() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 500));
        set.add(utxo("b", 0, 1000));
        set.add(utxo("c", 0, 2000));
        let selected = set.select(1500, 100).unwrap();
        assert!(selected.iter().map(|u| u.value).sum::<u64>() >= 1600);
    }

    #[test]
    fn test_select_rejects_overflowing_amount_or_fee() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 1000));
        assert!(set.select(u64::MAX, 1).is_none());
        assert!(set.select(1, u64::MAX).is_none());
        assert!(set.select(crate::MAX_MONEY_SATS + 1, 0).is_none());
    }

    #[test]
    fn test_select_insufficient() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 100));
        assert!(set.select(1000, 0).is_none());
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("vtorrent_utxo_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");

        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 5000));
        set.add(utxo("bb", 1, 3000));
        set.save(&path).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.total(), 8000);
        assert_eq!(loaded.list().len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let path = std::env::temp_dir().join("nonexistent_utxo_file.json");
        let set = UtxoSet::load(&path).unwrap();
        assert_eq!(set.total(), 0);
    }

    #[test]
    fn test_release_restores_selectability() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve("a", 0);
        assert!(set.list().is_empty(), "reserved input must leave selection");
        assert!(set.release("a", 0), "release must report the reservation");
        assert!(!set.release("a", 0), "second release is a no-op");
        // A rescan can now re-add it.
        set.add(utxo("a", 0, 5000));
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn test_scan_cursor_round_trips() {
        let dir = std::env::temp_dir().join(format!("vtorrent_utxo_cursor_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");
        let mut set = UtxoSet::new();
        set.set_last_scanned_height(123);
        set.save(&path).unwrap();
        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.last_scanned_height(), 123);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_note_spend_marks_a_reserved_outpoint() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve_utxo(utxo("a", 0, 5000));
        assert!(set.reserved_utxo("a", 0).is_some());
        assert!(set.note_spend("a", 0));
        assert!(set.reserved_utxo("a", 0).is_none());
        assert!(set.was_reserved_spent("a", 0));
        assert!(!set.note_spend("a", 0));
        set.clear_spent_reserved("a", 0);
        assert!(!set.was_reserved_spent("a", 0));
    }

    #[test]
    fn test_rollback_above_keeps_blocks_at_or_below_the_fork() {
        let mut set = UtxoSet::new();
        for (txid, height) in [
            ("old", 100u32),
            ("fork", 120),
            ("above", 121),
            ("newer", 130),
        ] {
            set.add(Utxo {
                txid: txid.to_string(),
                vout: 0,
                value: 1000,
                address: "bc1qtest".to_string(),
                height,
            });
        }
        set.reserve("reserved", 0);
        set.set_next_index(9);

        set.rollback_above(120);

        let mut kept: Vec<_> = set.list().iter().map(|u| u.txid.clone()).collect();
        kept.sort();
        assert_eq!(kept, vec!["fork".to_string(), "old".to_string()]);
        assert_eq!(set.next_index(), 9, "address index is not chain state");
        assert!(
            set.release("reserved", 0),
            "reservation must survive a reorg"
        );
    }

    #[test]
    fn test_rollback_above_zero_keeps_only_mempool_entries() {
        let mut set = UtxoSet::new();
        set.add(Utxo {
            txid: "mempool".to_string(),
            vout: 0,
            value: 500,
            address: "bc1qtest".to_string(),
            height: 0,
        });
        set.add(utxo("confirmed", 0, 1000));

        set.rollback_above(0);

        assert_eq!(set.list().len(), 1);
        assert_eq!(set.list()[0].txid, "mempool");
    }

    #[test]
    fn test_clear_chain_utxos_preserves_recovery_metadata() {
        let mut set = UtxoSet::new();
        set.add(utxo("a", 0, 5000));
        set.reserve("b", 0);
        set.set_next_index(7);
        set.record_swap_transaction("tx1".into(), vec![1, 2, 3]);
        set.record_swap_contract(SwapContract {
            order_id: "order1".into(),
            funding_txid: "tx1".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 1000,
            refund_raw: None,
            input_txid: Some("b".into()),
            input_vout: Some(0),
        });

        set.clear_chain_utxos();

        assert!(set.list().is_empty());
        assert_eq!(set.next_index(), 7);
        assert!(set.release("b", 0), "reservation must survive a reorg");
        assert!(set.pending_swap_transactions().contains_key("tx1"));
        assert!(set.swap_contract("order1").is_some());
    }

    #[test]
    fn undo_entry_restores_outputs_removed_above_the_fork() {
        let mut set = UtxoSet::new();
        set.add(Utxo {
            txid: "aa".into(),
            vout: 0,
            value: 500,
            address: "bc1q".into(),
            height: 5,
        });
        set.push_undo(UndoEntry {
            height: 6,
            created: vec![Utxo {
                txid: "bb".into(),
                vout: 0,
                value: 700,
                address: "bc1q".into(),
                height: 6,
            }],
            removed: vec![RemovedOutput {
                utxo: Utxo {
                    txid: "aa".into(),
                    vout: 0,
                    value: 500,
                    address: "bc1q".into(),
                    height: 5,
                },
                prior: PriorState::Free,
            }],
        });
        set.add(Utxo {
            txid: "bb".into(),
            vout: 0,
            value: 700,
            address: "bc1q".into(),
            height: 6,
        });
        set.remove("aa", 0);

        assert!(set.undo_above(5));
        let ids: Vec<_> = set.list().iter().map(|u| u.txid.clone()).collect();
        assert_eq!(
            ids,
            vec!["aa".to_string()],
            "created above fork dropped, removed restored"
        );
        assert_eq!(set.list()[0].value, 500);
        assert_eq!(set.list()[0].height, 5);
    }

    #[test]
    fn undo_restores_reserved_state_instead_of_freeing_the_input() {
        let mut set = UtxoSet::new();
        let reserved = utxo("cc", 0, 900);
        set.reserve_utxo(reserved.clone());
        set.record_swap_contract(SwapContract {
            order_id: "order-cc".into(),
            funding_txid: "ff".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 900,
            refund_raw: None,
            input_txid: Some("cc".into()),
            input_vout: Some(0),
        });
        set.note_spend("cc", 0);
        set.push_undo(UndoEntry {
            height: 7,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: reserved.clone(),
                prior: PriorState::Reserved(reserved.clone()),
            }],
        });

        assert!(set.undo_above(6));
        assert!(
            set.reserved_utxo("cc", 0).is_some(),
            "input returns reserved, not selectable"
        );
        assert!(
            !set.was_reserved_spent("cc", 0),
            "a restored reservation must not stay marked spent"
        );
        assert!(!set.list().iter().any(|u| u.txid == "cc"));
    }

    #[test]
    fn undo_does_not_resurrect_a_reservation_a_released_swap_no_longer_needs() {
        let mut set = UtxoSet::new();
        let reserved = utxo("cd", 0, 900);
        set.reserve_utxo(reserved.clone());
        set.record_swap_contract(SwapContract {
            order_id: "order-cd".into(),
            funding_txid: "fe".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 900,
            refund_raw: None,
            input_txid: Some("cd".into()),
            input_vout: Some(0),
        });
        set.push_undo(UndoEntry {
            height: 7,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: reserved.clone(),
                prior: PriorState::Reserved(reserved.clone()),
            }],
        });
        // The swap is released before the reorg: nothing can release the
        // reservation again, so restoring it would hide the input forever.
        set.note_spend("cd", 0);
        set.remove_swap_contract("order-cd");

        assert!(set.undo_above(6));
        assert!(
            set.reserved_utxo("cd", 0).is_none(),
            "no live swap holds the outpoint, so it must not come back reserved"
        );
        assert!(
            set.list().iter().any(|u| u.txid == "cd" && u.vout == 0),
            "the outpoint must return spendable"
        );
        assert!(!set.was_reserved_spent("cd", 0));
    }

    #[test]
    fn undo_above_restores_a_spent_reserved_outpoint_as_already_spent() {
        let mut set = UtxoSet::new();
        let record = utxo("dd", 0, 400);
        set.reserve_utxo(record.clone());
        set.push_undo(UndoEntry {
            height: 8,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record,
                prior: PriorState::SpentReserved,
            }],
        });

        assert!(set.undo_above(7));
        assert!(set.was_reserved_spent("dd", 0));
        assert!(set.reserved_utxo("dd", 0).is_none());
        assert!(
            !set.release("dd", 0),
            "a spent reservation must not reappear as reserved"
        );
        assert!(!set.list().iter().any(|u| u.txid == "dd"));
    }

    #[test]
    fn outpoint_finds_spendable_and_reserved_records() {
        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 500));
        set.reserve_utxo(utxo("bb", 1, 700));

        let spendable = set.outpoint("aa", 0).expect("spendable outpoint");
        assert_eq!(spendable.value, 500);
        let reserved = set.outpoint("bb", 1).expect("reserved outpoint");
        assert_eq!(reserved.value, 700);
        assert_eq!(reserved.vout, 1);
        assert_eq!(reserved.height, 100);
        assert!(set.outpoint("cc", 0).is_none());
    }

    #[test]
    fn undo_above_drops_an_outpoint_a_block_created_and_spent() {
        let mut set = UtxoSet::new();
        // A block can create an output and spend it in the same block. Undoing
        // the block must leave the outpoint absent: the restore of the spend
        // must not resurrect the creation that is also being undone.
        let created = Utxo {
            txid: "ab".repeat(32),
            vout: 0,
            value: 700,
            address: "bc1q".into(),
            height: 9,
        };
        set.push_undo(UndoEntry {
            height: 9,
            created: vec![created.clone()],
            removed: vec![RemovedOutput {
                utxo: created,
                prior: PriorState::Free,
            }],
        });

        assert!(set.undo_above(8));
        assert!(
            set.outpoint(&"ab".repeat(32), 0).is_none(),
            "an output created and spent in the undone block must not be restored"
        );
        assert!(set.list().is_empty());
    }

    #[test]
    fn undo_above_reports_no_coverage_when_the_fork_is_below_the_journal() {
        let mut set = UtxoSet::new();
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });
        assert!(!set.undo_above(5));
        assert_eq!(set.undo_entries().len(), 1, "no partial application");
    }

    #[test]
    fn undo_above_reports_no_coverage_for_an_unordered_or_gapped_journal() {
        // `push_undo` drops an out-of-order entry, so build the bad journals
        // directly: the coverage check must reject them without mutating.
        for entries in [
            vec![
                UndoEntry {
                    height: 12,
                    created: vec![utxo("aa", 0, 500)],
                    removed: vec![],
                },
                UndoEntry {
                    height: 10,
                    created: vec![],
                    removed: vec![],
                },
            ],
            vec![UndoEntry {
                height: 12,
                created: vec![],
                removed: vec![],
            }],
        ] {
            let mut set = UtxoSet::new();
            set.add(utxo("aa", 0, 500));
            set.set_last_scanned_tip(Some([9u8; 32]));
            set.set_undo_raw(entries);
            let before = serde_json::to_string(&set).unwrap();

            assert!(!set.undo_above(9), "a bad journal must not claim coverage");
            assert_eq!(
                serde_json::to_string(&set).unwrap(),
                before,
                "an uncovered rollback must leave the set byte-identical"
            );
        }
    }

    #[test]
    fn undo_above_clears_reservation_state_of_a_dropped_created_outpoint() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        // The outpoint is created at 101, reserved, then spent while reserved
        // at 102. Its creation is above the fork, so the rollback drops it.
        set.push_undo(UndoEntry {
            height: 101,
            created: vec![record.clone()],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![RemovedOutput {
                utxo: record.clone(),
                prior: PriorState::Reserved(record.clone()),
            }],
        });
        set.note_spend(&record.txid, record.vout);

        assert!(set.undo_above(100));
        assert!(
            set.outpoint(&record.txid, record.vout).is_none(),
            "a dropped creation must not leave a phantom reservation"
        );
        assert!(!set.was_reserved_spent(&record.txid, record.vout));
        assert!(!set.release(&record.txid, record.vout));
        assert!(!set.reserved.contains(&(record.txid.clone(), record.vout)));
    }

    #[test]
    fn undo_above_does_not_resurrect_a_reservation_for_a_dropped_created_outpoint() {
        let record = utxo("ab", 0, 700);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        // The reconciliation scan observed the spend, but the outpoint's own
        // creation is above the fork, so the replay drops it entirely. Restoring
        // the observation afterwards would reserve an outpoint that no longer
        // exists anywhere else in the set.
        set.record_reservation_spend(record.clone(), 101);
        set.push_undo(UndoEntry {
            height: 101,
            created: vec![record.clone()],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(100));
        assert!(
            set.outpoint(&record.txid, record.vout).is_none(),
            "a dropped creation must not return as a phantom reservation"
        );
        assert!(!set.was_reserved_spent(&record.txid, record.vout));
        assert!(!set.release(&record.txid, record.vout));
        assert!(!set.reserved.contains(&(record.txid.clone(), record.vout)));
    }

    #[test]
    fn undo_above_restores_a_reservation_observed_spent_by_reconciliation() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);
        assert!(set.was_reserved_spent(&record.txid, record.vout));
        set.push_undo(UndoEntry {
            height: 10,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(9));
        assert!(
            !set.was_reserved_spent(&record.txid, record.vout),
            "a covered rollback must not leave the observed spend behind"
        );
        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_some(),
            "the outpoint returns to its pre-observation reserved state"
        );
    }

    #[test]
    fn undo_above_leaves_an_observation_below_the_fork_alone() {
        let record = utxo("ae", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone(), 10);
        // An entry above the fork makes a fork at 300 covered.
        set.push_undo(UndoEntry {
            height: 301,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(300));

        assert!(
            set.reserved_utxo(&record.txid, record.vout).is_none(),
            "a spend observed below the fork happened on the surviving branch"
        );
        assert!(
            set.was_reserved_spent(&record.txid, record.vout),
            "the observation must stay spent"
        );
        assert_eq!(
            set.observed_reservation_spends
                .get(&(record.txid.clone(), record.vout))
                .map(|(_, observed_at)| *observed_at),
            Some(10),
            "an observation below the fork is untouched, not discarded"
        );
    }

    fn folded(entries: &[(&str, u32, u64)]) -> Vec<(String, u32, u64, u32)> {
        let mut folded: Vec<(String, u32, u64, u32)> = entries
            .iter()
            .map(|(txid, vout, value)| (txid.to_string(), *vout, *value, 100))
            .collect();
        folded.sort();
        folded
    }

    #[test]
    fn undo_above_n_blocks_equals_a_forward_fold_to_the_fork() {
        let base = utxo("aa", 0, 500);
        let six = utxo("bb", 0, 600);
        let eight = utxo("cc", 0, 800);
        let mut set = UtxoSet::new();
        // Fold the chain forward: aa is created below the fork, bb at 6 spends
        // aa, cc at 8 spends bb.
        set.add(base.clone());
        // Height 5 is a sentinel so fork 4 is covered (the first journalled
        // height above a fork must be fork + 1).
        set.push_undo(UndoEntry {
            height: 5,
            created: vec![],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 6,
            created: vec![six.clone()],
            removed: vec![RemovedOutput {
                utxo: base.clone(),
                prior: PriorState::Free,
            }],
        });
        set.push_undo(UndoEntry {
            height: 8,
            created: vec![eight.clone()],
            removed: vec![RemovedOutput {
                utxo: six.clone(),
                prior: PriorState::Free,
            }],
        });
        set.remove(&base.txid, base.vout);
        set.add(six.clone());
        set.remove(&six.txid, six.vout);
        set.add(eight.clone());

        assert!(set.undo_above(4));
        let rolled: Vec<_> = set
            .list()
            .iter()
            .map(|u| (u.txid.clone(), u.vout, u.value, u.height))
            .collect();
        let expected = folded(&[("aa", 0, 500)]);
        assert_eq!(rolled, expected, "N-entry replay must equal the fork state");
    }

    #[test]
    fn a_corrupt_journal_is_discarded_without_losing_the_utxo_set() {
        let dir =
            std::env::temp_dir().join(format!("vtorrent_utxo_corrupt_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("utxos.json");
        let mut set = UtxoSet::new();
        set.add(utxo("aa", 0, 5000));
        set.set_last_scanned_tip(Some([7u8; 32]));
        let mut value = serde_json::to_value(&set).unwrap();
        value["undo"] = serde_json::json!("not-a-journal");
        value["last_scanned_tip"] = serde_json::json!("not-a-tip");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        let loaded = UtxoSet::load(&path).expect("a corrupt journal must not fail the load");
        assert_eq!(loaded.total(), 5000);
        assert_eq!(loaded.list().len(), 1);
        assert!(loaded.undo_entries().is_empty());
        assert_eq!(loaded.last_scanned_tip(), None);

        // The settled balance is not recovery data: corrupting it still fails.
        value["utxos"] = serde_json::json!("not-a-balance");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        assert!(UtxoSet::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn push_undo_ignores_an_older_height_and_keeps_coverage() {
        let mut set = UtxoSet::new();
        set.push_undo(UndoEntry {
            height: 9,
            created: vec![],
            removed: vec![],
        });
        set.push_undo(UndoEntry {
            height: 5,
            created: vec![],
            removed: vec![],
        });

        assert_eq!(
            set.undo_entries().len(),
            1,
            "older entries are not appended"
        );
        assert_eq!(set.undo_entries()[0].height, 9);
        assert!(
            set.undo_above(8),
            "a reconciliation scan must not disable journal coverage"
        );
        assert!(set.undo_entries().is_empty());
    }

    #[test]
    fn push_undo_trims_the_oldest_entries_at_the_cap() {
        let mut set = UtxoSet::new();
        for height in 0..=MAX_UNDO_ENTRIES as u32 {
            set.push_undo(UndoEntry {
                height,
                created: vec![],
                removed: vec![],
            });
        }

        assert_eq!(set.undo_entries().len(), MAX_UNDO_ENTRIES);
        assert_eq!(
            set.undo_entries()[0].height,
            1,
            "the oldest entry is dropped"
        );
        assert_eq!(
            set.undo_entries().last().unwrap().height,
            MAX_UNDO_ENTRIES as u32
        );
    }

    #[test]
    fn prune_undo_drops_below_floor_and_enforces_cap() {
        let mut set = UtxoSet::new();
        for height in 1..=5u32 {
            set.push_undo(UndoEntry {
                height,
                created: vec![],
                removed: vec![],
            });
        }
        set.prune_undo(3, 10);
        assert_eq!(set.undo_entries().first().unwrap().height, 3);
        set.prune_undo(0, 2);
        assert_eq!(set.undo_entries().len(), 2);
        assert_eq!(set.undo_entries().last().unwrap().height, 5);
    }

    #[test]
    fn undo_above_restores_a_pending_removal_built_at_the_fork() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.add(record.clone());
        set.remove(&record.txid, record.vout);
        set.record_pending_removals(std::slice::from_ref(&record), "ff", 101);
        // Coverage for a fork at 101: the first journalled height above it must
        // be 102.
        set.push_undo(UndoEntry {
            height: 102,
            created: vec![],
            removed: vec![],
        });

        assert!(set.undo_above(101));
        assert!(
            set.outpoint(&record.txid, record.vout).is_some(),
            "a pending spend built at the fork cursor is reorged out and must return"
        );
        assert!(
            set.pending_removals().is_empty(),
            "the post-fork scan re-observes the spend, so the record is retired"
        );
    }

    #[test]
    fn a_failed_broadcast_cancels_its_pending_removal() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.add(record.clone());
        set.remove(&record.txid, record.vout);
        set.record_pending_removals(std::slice::from_ref(&record), "ff", 101);

        assert!(set.add(record.clone()));
        set.cancel_local_removals(std::slice::from_ref(&record));

        assert!(
            set.pending_removals().is_empty(),
            "the send never happened, so its pending removal must be cancelled"
        );
        assert_eq!(set.list().len(), 1);
    }

    #[test]
    fn clear_undo_clears_pending_removals() {
        let mut set = UtxoSet::new();
        set.record_pending_removals(&[utxo("aa", 0, 500)], "ff", 101);
        set.clear_undo();
        assert!(set.pending_removals().is_empty());
    }

    #[test]
    fn clear_undo_clears_reservation_observations() {
        let mut set = UtxoSet::new();
        let record = utxo("aa", 0, 500);
        set.reserve_utxo(record.clone());
        set.note_spend(&record.txid, record.vout);
        set.record_reservation_spend(record.clone(), 10);

        set.clear_undo();

        assert!(
            set.observed_reservation_spends.is_empty(),
            "a full reset abandons observations made on the old chain state"
        );
    }

    #[test]
    fn prune_undo_keeps_pending_removals_below_the_floor() {
        let mut set = UtxoSet::new();
        set.record_pending_removals(&[utxo("aa", 0, 500)], "ff", 5);
        set.record_pending_removals(&[utxo("bb", 0, 700)], "ee", 6);

        set.prune_undo(6, MAX_UNDO_ENTRIES);

        assert_eq!(
            set.pending_removals().len(),
            2,
            "a pending removal is self-contained and may be the only copy of an unseen send"
        );
        assert_eq!(set.pending_removals()[0].removed.utxo.txid, "aa");
        assert!(set.undo_entries().is_empty());
    }

    #[test]
    fn prune_undo_caps_pending_removals() {
        let mut set = UtxoSet::new();
        for vout in 0..3u32 {
            set.record_pending_removals(&[utxo("aa", vout, 500)], "ff", 5);
        }

        set.prune_undo(0, 2);

        assert_eq!(set.pending_removals().len(), 2);
        assert_eq!(
            set.pending_removals()[1].removed.utxo.vout,
            2,
            "the newest records are kept"
        );
    }

    #[test]
    fn journal_and_tip_round_trip_through_persistence() {
        let path = std::env::temp_dir().join(format!("btc-undo-rt-{}.json", std::process::id()));
        let mut set = UtxoSet::new();
        set.set_last_scanned_tip(Some([7u8; 32]));
        set.push_undo(UndoEntry {
            height: 4,
            created: vec![],
            removed: vec![],
        });
        set.save(&path).unwrap();

        let loaded = UtxoSet::load(&path).unwrap();
        assert_eq!(loaded.last_scanned_tip(), Some([7u8; 32]));
        assert_eq!(loaded.undo_entries().len(), 1);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_remove_swap_contract_drops_pending_tx() {
        let mut set = UtxoSet::new();
        set.record_swap_transaction("tx1".into(), vec![1, 2, 3]);
        set.record_swap_contract(SwapContract {
            order_id: "order1".into(),
            funding_txid: "tx1".into(),
            hash_lock: [0u8; 32],
            recipient: "bc1q".into(),
            refund_address: "bc1q".into(),
            expiry: 100,
            amount: 1000,
            refund_raw: None,
            input_txid: Some("a".into()),
            input_vout: Some(0),
        });
        let removed = set.remove_swap_contract("order1").unwrap();
        assert_eq!(removed.funding_txid, "tx1");
        assert!(set.swap_contract("order1").is_none());
        assert!(
            set.pending_swap_transactions().is_empty(),
            "pending tx must be dropped with the contract"
        );
    }
}
