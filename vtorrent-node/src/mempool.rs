//! Transaction mempool with fee market and Replace-By-Fee (RBF) support.
//!
//! # Fee Market
//! Each transaction pays a fee equal to the sum of its inputs minus the sum of
//! its outputs. Transactions are prioritised by **fee rate** (satoshis per byte
//! of serialised transaction size). When the mempool is full, the lowest-fee-rate
//! transaction is evicted to make room for a higher-paying one.
//!
//! # Replace-By-Fee (RBF)
//! A transaction may replace an existing unconfirmed transaction with the same
//! input(s) if:
//! 1. The replacement signals RBF (the `rbf` flag is set).
//! 2. The replacement fee rate is at least `MIN_RBF_FEE_BUMP` sat/byte higher
//!    than the transaction being replaced.
//! 3. The absolute fee of the replacement is at least as large as the original.
//!
//! This follows the spirit of Bitcoin Core's BIP-125 policy.

use std::collections::HashMap;
use crate::{
    block::Transaction,
    consensus::validate_transaction,
    error::{NodeError, Result},
};

/// Minimum fee rate increase (sat/byte) required for an RBF replacement.
pub const MIN_RBF_FEE_BUMP: u64 = 1;

/// Minimum absolute fee (satoshis) for any transaction to enter the mempool.
pub const MIN_RELAY_FEE: u64 = 1_000; // 0.00001 VTR

/// A mempool entry wrapping a transaction with its computed fee metadata.
#[derive(Debug, Clone)]
pub struct MempoolEntry {
    /// The transaction.
    pub tx: Transaction,
    /// Absolute fee in satoshis (inputs - outputs).
    /// For transactions without explicit input values (e.g. coinbase-style),
    /// this is estimated from the declared fee field or set to 0.
    pub fee_sats: u64,
    /// Serialised transaction size in bytes (used to compute fee rate).
    pub size_bytes: usize,
    /// Unix timestamp when the transaction entered the mempool.
    pub received_at: u64,
    /// Whether this transaction signals Replace-By-Fee.
    pub rbf: bool,
}

impl MempoolEntry {
    /// Fee rate in satoshis per byte.
    pub fn fee_rate(&self) -> u64 {
        if self.size_bytes == 0 { return 0; }
        self.fee_sats / self.size_bytes as u64
    }
}

/// The transaction mempool with fee market and RBF support.
pub struct Mempool {
    /// Transactions indexed by txid.
    entries: HashMap<[u8; 32], MempoolEntry>,
    /// Maximum number of transactions in the mempool.
    max_size: usize,
    /// Minimum fee rate (sat/byte) to enter the mempool.
    /// Dynamically raised when the mempool is full.
    min_fee_rate: u64,
}

impl Mempool {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_size,
            min_fee_rate: 1, // 1 sat/byte minimum by default
        }
    }

    /// Add a transaction to the mempool.
    ///
    /// Enforces:
    /// - Minimum relay fee
    /// - Dynamic minimum fee rate (raised when mempool is full)
    /// - RBF replacement rules
    /// - Eviction of the lowest-fee-rate tx when full (if new tx pays more)
    pub fn add_transaction(&mut self, tx: Transaction) -> Result<()> {
        validate_transaction(&tx)?;

        let txid = tx.txid();

        // Compute fee and size
        let fee_sats = tx.fee_sats();
        let size_bytes = tx.serialized_size();
        let fee_rate = if size_bytes > 0 { fee_sats / size_bytes as u64 } else { 0 };
        let rbf = tx.signals_rbf();

        // Enforce minimum absolute fee
        if fee_sats < MIN_RELAY_FEE {
            return Err(NodeError::Chain(format!(
                "Fee too low: {} sat < {} sat minimum",
                fee_sats, MIN_RELAY_FEE
            )));
        }

        // Enforce dynamic minimum fee rate
        if fee_rate < self.min_fee_rate {
            return Err(NodeError::Chain(format!(
                "Fee rate too low: {} sat/byte < {} sat/byte minimum",
                fee_rate, self.min_fee_rate
            )));
        }

        // Check for duplicate
        if self.entries.contains_key(&txid) {
            return Ok(());
        }

        // Check for RBF replacement: does this tx spend the same inputs as an existing tx?
        let conflicting_txid = self.find_conflict(&tx);
        if let Some(conflict_txid) = conflicting_txid {
            let conflict = self.entries.get(&conflict_txid)
                .ok_or_else(|| NodeError::Chain("Conflict entry missing".into()))?;

            if !rbf {
                return Err(NodeError::Chain(
                    "Transaction conflicts with mempool entry; signal RBF to replace".into()
                ));
            }

            // RBF rules: new fee rate must be at least MIN_RBF_FEE_BUMP higher
            if fee_rate < conflict.fee_rate() + MIN_RBF_FEE_BUMP {
                return Err(NodeError::Chain(format!(
                    "RBF replacement fee rate {} sat/byte must exceed {} + {} sat/byte",
                    fee_rate, conflict.fee_rate(), MIN_RBF_FEE_BUMP
                )));
            }

            // Absolute fee must be at least as large
            if fee_sats < conflict.fee_sats {
                return Err(NodeError::Chain(format!(
                    "RBF replacement absolute fee {} sat must be >= {} sat",
                    fee_sats, conflict.fee_sats
                )));
            }

            tracing::info!(
                "RBF: replacing {} with {} (fee rate {} -> {} sat/byte)",
                hex::encode(conflict_txid),
                hex::encode(txid),
                conflict.fee_rate(),
                fee_rate
            );
            self.entries.remove(&conflict_txid);
        }

        // If mempool is full, try to evict the lowest-fee-rate entry
        if self.entries.len() >= self.max_size {
            if let Some(lowest_txid) = self.lowest_fee_rate_txid() {
                let lowest_rate = self.entries[&lowest_txid].fee_rate();
                if fee_rate > lowest_rate {
                    tracing::debug!(
                        "Mempool full: evicting {} ({} sat/byte) for {} ({} sat/byte)",
                        hex::encode(lowest_txid), lowest_rate,
                        hex::encode(txid), fee_rate
                    );
                    self.entries.remove(&lowest_txid);
                    // Raise the dynamic minimum fee rate
                    self.min_fee_rate = lowest_rate + 1;
                } else {
                    return Err(NodeError::Chain(format!(
                        "Mempool full; minimum fee rate is now {} sat/byte",
                        self.min_fee_rate
                    )));
                }
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.entries.insert(txid, MempoolEntry {
            tx,
            fee_sats,
            size_bytes,
            received_at: now,
            rbf,
        });

        tracing::debug!(
            "Mempool: added {} ({} sat, {} sat/byte)",
            hex::encode(txid), fee_sats, fee_rate
        );
        Ok(())
    }

    /// Remove a transaction from the mempool (after inclusion in a block).
    pub fn remove_transaction(&mut self, txid: &[u8; 32]) {
        self.entries.remove(txid);
    }

    /// Get a specific transaction by txid.
    pub fn get_transaction(&self, txid: &[u8; 32]) -> Option<&Transaction> {
        self.entries.get(txid).map(|e| &e.tx)
    }

    /// Get all transactions sorted by fee rate (highest first) — used by staker.
    pub fn get_transactions(&self) -> Vec<Transaction> {
        let mut entries: Vec<&MempoolEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.fee_rate().cmp(&a.fee_rate()));
        entries.into_iter().map(|e| e.tx.clone()).collect()
    }

    /// Get all mempool entries with their fee metadata.
    pub fn get_entries(&self) -> Vec<&MempoolEntry> {
        let mut entries: Vec<&MempoolEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.fee_rate().cmp(&a.fee_rate()));
        entries
    }

    /// Returns the number of transactions in the mempool.
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// Returns the current dynamic minimum fee rate (sat/byte).
    pub fn min_fee_rate(&self) -> u64 {
        self.min_fee_rate
    }

    /// Returns the total fees waiting in the mempool (sat).
    pub fn total_fees(&self) -> u64 {
        self.entries.values().map(|e| e.fee_sats).sum()
    }

    /// Returns the median fee rate of all mempool transactions (sat/byte).
    pub fn median_fee_rate(&self) -> u64 {
        let mut rates: Vec<u64> = self.entries.values().map(|e| e.fee_rate()).collect();
        if rates.is_empty() { return self.min_fee_rate; }
        rates.sort_unstable();
        rates[rates.len() / 2]
    }

    /// Returns the recommended fee rate for next-block inclusion (sat/byte).
    ///
    /// Uses the 75th-percentile fee rate of current mempool entries, or the
    /// minimum fee rate if the mempool is less than 50% full.
    pub fn recommended_fee_rate(&self) -> u64 {
        if self.entries.len() < self.max_size / 2 {
            return self.min_fee_rate;
        }
        let mut rates: Vec<u64> = self.entries.values().map(|e| e.fee_rate()).collect();
        rates.sort_unstable();
        let p75 = rates.len() * 3 / 4;
        rates.get(p75).copied().unwrap_or(self.min_fee_rate)
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Find a mempool transaction that spends the same input as `tx`.
    fn find_conflict(&self, tx: &Transaction) -> Option<[u8; 32]> {
        for input in &tx.inputs {
            for (txid, entry) in &self.entries {
                for existing_input in &entry.tx.inputs {
                    if existing_input.prev_txid == input.prev_txid
                        && existing_input.prev_vout == input.prev_vout
                    {
                        return Some(*txid);
                    }
                }
            }
        }
        None
    }

    /// Find the txid of the entry with the lowest fee rate.
    fn lowest_fee_rate_txid(&self) -> Option<[u8; 32]> {
        self.entries.iter()
            .min_by_key(|(_, e)| e.fee_rate())
            .map(|(txid, _)| *txid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Transaction, TxInput, TxOutput, TxType};

    fn make_tx(fee: u64, size_hint: usize, rbf: bool) -> Transaction {
        // Build a simple transaction with a declared fee via output values
        // Input: 100_000 sat, Output: 100_000 - fee sat
        let output_value = 100_000u64.saturating_sub(fee);
        Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![TxInput {
                prev_txid: [size_hint as u8; 32], // unique per test
                prev_vout: 0,
                script_sig: vec![],
                sequence: if rbf { 0xFFFFFFFD } else { 0xFFFFFFFF },
            }],
            outputs: vec![TxOutput {
                value: output_value,
                script_pubkey: vec![0x76, 0xa9, 0x14, 0x00, 0x88, 0xac],
            }],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        }
    }

    #[test]
    fn test_add_transaction_basic() {
        let mut mp = Mempool::new(100);
        let tx = make_tx(MIN_RELAY_FEE, 1, false);
        assert!(mp.add_transaction(tx).is_ok());
        assert_eq!(mp.size(), 1);
    }

    #[test]
    fn test_fee_too_low_rejected() {
        let mut mp = Mempool::new(100);
        let tx = make_tx(MIN_RELAY_FEE - 1, 2, false);
        assert!(mp.add_transaction(tx).is_err());
    }

    #[test]
    fn test_duplicate_ignored() {
        let mut mp = Mempool::new(100);
        let tx = make_tx(MIN_RELAY_FEE, 3, false);
        mp.add_transaction(tx.clone()).unwrap();
        mp.add_transaction(tx).unwrap(); // should not error
        assert_eq!(mp.size(), 1);
    }

    #[test]
    fn test_get_transactions_sorted_by_fee_rate() {
        let mut mp = Mempool::new(100);
        mp.add_transaction(make_tx(MIN_RELAY_FEE, 10, false)).unwrap();
        mp.add_transaction(make_tx(MIN_RELAY_FEE * 5, 11, false)).unwrap();
        mp.add_transaction(make_tx(MIN_RELAY_FEE * 2, 12, false)).unwrap();
        let txs = mp.get_transactions();
        // Highest fee rate first
        let fees: Vec<u64> = txs.iter().map(|t| t.fee_sats()).collect();
        assert!(fees[0] >= fees[1]);
        assert!(fees[1] >= fees[2]);
    }

    #[test]
    fn test_rbf_replacement() {
        let mut mp = Mempool::new(100);
        // Original tx with low fee
        let original = make_tx(MIN_RELAY_FEE, 20, true);
        mp.add_transaction(original.clone()).unwrap();
        assert_eq!(mp.size(), 1);

        // Replacement with same input, higher fee, signals RBF
        let mut replacement = make_tx(MIN_RELAY_FEE * 10, 20, true);
        replacement.inputs[0].prev_txid = original.inputs[0].prev_txid; // same input
        mp.add_transaction(replacement).unwrap();
        assert_eq!(mp.size(), 1); // replaced, not added
    }

    #[test]
    fn test_rbf_without_signal_rejected() {
        let mut mp = Mempool::new(100);
        let original = make_tx(MIN_RELAY_FEE, 30, false);
        mp.add_transaction(original.clone()).unwrap();

        let mut replacement = make_tx(MIN_RELAY_FEE * 10, 30, false);
        replacement.inputs[0].prev_txid = original.inputs[0].prev_txid;
        // Should fail: conflicts but doesn't signal RBF
        assert!(mp.add_transaction(replacement).is_err());
    }

    #[test]
    fn test_mempool_eviction_when_full() {
        let mut mp = Mempool::new(3);
        // Use a small fee so output value stays well above zero (100_000 - 1_000 = 99_000)
        mp.add_transaction(make_tx(MIN_RELAY_FEE, 40, false)).unwrap();
        mp.add_transaction(make_tx(MIN_RELAY_FEE, 41, false)).unwrap();
        mp.add_transaction(make_tx(MIN_RELAY_FEE, 42, false)).unwrap();
        assert_eq!(mp.size(), 3);

        // High-fee tx: fee = 50_000, output = 50_000 (still > 0)
        let high_fee = make_tx(50_000, 43, false);
        mp.add_transaction(high_fee).unwrap();
        assert_eq!(mp.size(), 3);
    }

    #[test]
    fn test_recommended_fee_rate_empty() {
        let mp = Mempool::new(100);
        assert_eq!(mp.recommended_fee_rate(), mp.min_fee_rate());
    }

    #[test]
    fn test_total_fees() {
        let mut mp = Mempool::new(100);
        mp.add_transaction(make_tx(MIN_RELAY_FEE, 50, false)).unwrap();
        mp.add_transaction(make_tx(MIN_RELAY_FEE * 2, 51, false)).unwrap();
        assert_eq!(mp.total_fees(), MIN_RELAY_FEE * 3);
    }

    #[test]
    fn test_remove_transaction() {
        let mut mp = Mempool::new(100);
        let tx = make_tx(MIN_RELAY_FEE, 60, false);
        let txid = tx.txid();
        mp.add_transaction(tx).unwrap();
        mp.remove_transaction(&txid);
        assert_eq!(mp.size(), 0);
    }
}
