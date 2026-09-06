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
}

/// In-memory UTXO set with optional disk persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UtxoSet {
    utxos: Vec<Utxo>,
    #[serde(default)]
    reserved: std::collections::BTreeSet<(String, u32)>,
    #[serde(default)]
    pending_swap_transactions: std::collections::BTreeMap<String, Vec<u8>>,
    #[serde(default)]
    swap_contracts: std::collections::BTreeMap<String, SwapContract>,
    #[serde(default)]
    next_index: u32,
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

    pub fn reserve(&mut self, txid: &str, vout: u32) {
        self.remove(txid, vout);
        self.reserved.insert((txid.to_owned(), vout));
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
        let mut sorted: Vec<Utxo> = self.utxos.clone();
        sorted.sort_by_key(|utxo| std::cmp::Reverse(utxo.value));
        let mut selected = Vec::new();
        let mut sum = 0u64;
        for u in sorted {
            sum += u.value;
            selected.push(u);
            if sum >= amount + fee {
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
}
