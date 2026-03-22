/// Transaction mempool.
///
/// Holds unconfirmed transactions waiting to be included in a block.

use std::collections::HashMap;
use crate::{
    block::Transaction,
    consensus::validate_transaction,
    error::{NodeError, Result},
};

/// The transaction mempool.
pub struct Mempool {
    /// Transactions indexed by txid.
    transactions: HashMap<[u8; 32], Transaction>,
    /// Maximum number of transactions in the mempool.
    max_size: usize,
}

impl Mempool {
    pub fn new(max_size: usize) -> Self {
        Self {
            transactions: HashMap::new(),
            max_size,
        }
    }

    /// Add a transaction to the mempool.
    pub fn add_transaction(&mut self, tx: Transaction) -> Result<()> {
        if self.transactions.len() >= self.max_size {
            return Err(NodeError::Chain("Mempool is full".into()));
        }

        validate_transaction(&tx)?;

        let txid = tx.txid();
        if self.transactions.contains_key(&txid) {
            return Ok(()); // Already in mempool
        }

        self.transactions.insert(txid, tx);
        tracing::debug!("Added tx {} to mempool", hex::encode(txid));
        Ok(())
    }

    /// Remove a transaction from the mempool (after it's been included in a block).
    pub fn remove_transaction(&mut self, txid: &[u8; 32]) {
        self.transactions.remove(txid);
    }

    /// Get all transactions in the mempool, sorted by fee (highest first).
    pub fn get_transactions(&self) -> Vec<&Transaction> {
        let mut txs: Vec<&Transaction> = self.transactions.values().collect();
        txs.sort_by(|a, b| b.total_output().cmp(&a.total_output()));
        txs
    }

    /// Get the number of transactions in the mempool.
    pub fn size(&self) -> usize {
        self.transactions.len()
    }
}
