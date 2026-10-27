//! UTXO set tracking for the wallet's addresses.

use serde::{Deserialize, Serialize};
use std::path::Path;

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

    pub fn set_last_scanned_height(&mut self, height: u32) {
        self.last_scanned_height = height;
    }

    pub(crate) fn rescan_required(&self) -> bool {
        self.rescan_required
    }

    pub(crate) fn set_rescan_required(&mut self, required: bool) {
        self.rescan_required = required;
    }

    pub fn add(&mut self, utxo: Utxo) {
        if self.reserved.contains(&(utxo.txid.clone(), utxo.vout)) {
            return;
        }
        if !self
            .utxos
            .iter()
            .any(|u| u.txid == utxo.txid && u.vout == utxo.vout)
        {
            self.utxos.push(utxo);
        }
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
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let json = match std::fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(error) => return Err(error),
        };
        let set: UtxoSet = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tracing::debug!(
            "UTXO set loaded: {} entries from {}",
            set.utxos.len(),
            path.display()
        );
        Ok(set)
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
