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
    /// Reservation spends observed by this process's reconciliation scan,
    /// mapped to the reserved record they replaced. Live-process only: a
    /// covered rollback clears it and puts the reservations back.
    #[serde(default, skip)]
    observed_reservation_spends: std::collections::BTreeMap<(String, u32), Utxo>,
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

    pub fn clear_undo(&mut self) {
        self.undo.clear();
    }

    /// Journal outpoints removed by a wallet-local action (a send) at the
    /// current scan cursor.
    ///
    /// The confirming scan can no longer record the removal, because the
    /// outpoint has already left the set, so without this entry a covered
    /// reorg would not restore an output whose spend never made it on chain.
    /// The cursor height is the journal tail, so a block that later touches
    /// the same height merges into this entry.
    pub fn journal_local_removals(&mut self, utxos: &[Utxo], height: u32) {
        if utxos.is_empty() {
            return;
        }
        let removed = utxos
            .iter()
            .map(|utxo| {
                let prior = if let Some(reserved) = self.reserved_utxo(&utxo.txid, utxo.vout) {
                    PriorState::Reserved(reserved.clone())
                } else if self.was_reserved_spent(&utxo.txid, utxo.vout) {
                    PriorState::SpentReserved
                } else {
                    PriorState::Free
                };
                RemovedOutput {
                    utxo: utxo.clone(),
                    prior,
                }
            })
            .collect();
        self.push_undo(UndoEntry {
            height,
            created: vec![],
            removed,
        });
    }

    /// Journal outpoints a wallet-local action (a failed broadcast) re-added.
    ///
    /// When the matching removal is still in the journal the two cancel: the
    /// send never happened, so the journal must keep claiming nothing about
    /// that outpoint. Only an addition with no recorded removal is journalled
    /// as `created`, because undoing it must remove the record again.
    pub fn journal_local_additions(&mut self, utxos: &[Utxo], height: u32) {
        if utxos.is_empty() {
            return;
        }
        let mut created = Vec::new();
        for utxo in utxos {
            let cancelled = self.undo.iter_mut().rev().find_map(|entry| {
                entry
                    .removed
                    .iter()
                    .position(|removed| {
                        removed.utxo.txid == utxo.txid && removed.utxo.vout == utxo.vout
                    })
                    .map(|index| {
                        entry.removed.remove(index);
                    })
            });
            if cancelled.is_none() {
                created.push(utxo.clone());
            }
        }
        if !created.is_empty() {
            self.push_undo(UndoEntry {
                height,
                created,
                removed: vec![],
            });
        }
    }

    /// Remember that this process's reconciliation scan observed `utxo` spent
    /// while it was reserved, so a covered rollback can put the reservation
    /// back instead of leaving `spent_reserved` behind forever.
    pub(crate) fn record_reservation_spend(&mut self, utxo: Utxo) {
        self.observed_reservation_spends
            .insert((utxo.txid.clone(), utxo.vout), utxo);
    }

    /// Drop journal entries below `floor`, then keep at most `max_entries`
    /// of the newest ones.
    pub fn prune_undo(&mut self, floor: u32, max_entries: usize) {
        self.undo.retain(|entry| entry.height >= floor);
        if self.undo.len() > max_entries {
            self.undo.drain(..self.undo.len() - max_entries);
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
    pub fn undo_above(&mut self, fork: u32) -> bool {
        if !self.undo_covers(fork) {
            return false;
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
        for (_, utxo) in std::mem::take(&mut self.observed_reservation_spends) {
            let key = (utxo.txid.clone(), utxo.vout);
            self.spent_reserved.remove(&key);
            self.reserve_utxo(utxo);
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
                self.reserve_utxo(reserved);
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
    /// The undo journal and its tip are wallet-local recovery aids, so a
    /// corrupt one is discarded rather than failing the wallet: the journal
    /// loads empty and the wallet falls back to case 2 or 3. The settled
    /// balance is not recovery data, so a corrupt `utxos` field still fails
    /// the load.
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
/// intact. Only the undo journal and its tip are tolerated this way.
fn sanitize_journal_fields(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if object.contains_key("undo")
        && serde_json::from_value::<Vec<UndoEntry>>(object["undo"].clone()).is_err()
    {
        object.insert("undo".into(), serde_json::Value::Array(Vec::new()));
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
    fn undo_above_restores_a_reservation_observed_spent_by_reconciliation() {
        let record = utxo("aa", 0, 500);
        let mut set = UtxoSet::new();
        set.reserve_utxo(record.clone());
        assert!(set.note_spend(&record.txid, record.vout));
        set.record_reservation_spend(record.clone());
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
