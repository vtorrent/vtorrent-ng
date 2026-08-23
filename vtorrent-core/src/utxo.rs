use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A reference to an unspent transaction output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutPoint {
    /// Transaction ID (SHA256d of the transaction).
    pub txid: [u8; 32],
    /// Output index within the transaction.
    pub vout: u32,
}

/// An unspent transaction output entry in the UTXO set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    pub outpoint: OutPoint,
    /// Value in satoshis (1 VTR = 100,000,000 satoshis).
    pub value: u64,
    /// The locking script (scriptPubKey).
    pub script_pubkey: Vec<u8>,
    /// Block height at which this UTXO was created.
    pub height: u32,
    /// Whether this UTXO is a coinbase (PoW block reward).
    pub is_coinbase: bool,
}

/// The complete UTXO snapshot extracted from the legacy blockchain.
/// This is embedded in the genesis block of the new chain to enable claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoSnapshot {
    /// The block height at which the snapshot was taken.
    pub snapshot_height: u32,
    /// The block hash at the snapshot height (for verification).
    pub snapshot_block_hash: [u8; 32],
    /// Total VTR value in the snapshot (sum of all UTXOs).
    pub total_value: u64,
    /// Map from address string to total balance (aggregated for display).
    pub balances: HashMap<String, u64>,
    /// Full UTXO set for precise claim verification.
    pub utxos: Vec<Utxo>,
}

impl UtxoSnapshot {
    /// Create a new empty snapshot.
    pub fn new(height: u32, block_hash: [u8; 32]) -> Self {
        Self {
            snapshot_height: height,
            snapshot_block_hash: block_hash,
            total_value: 0,
            balances: HashMap::new(),
            utxos: Vec::new(),
        }
    }

    /// Add a UTXO to the snapshot.
    pub fn add_utxo(&mut self, utxo: Utxo, address: &str) {
        self.total_value = self.total_value.saturating_add(utxo.value);
        let balance = self.balances.entry(address.to_string()).or_insert(0);
        *balance = balance.saturating_add(utxo.value);
        self.utxos.push(utxo);
    }

    /// Get the claimable balance for a given legacy address.
    pub fn balance_for(&self, address: &str) -> u64 {
        *self.balances.get(address).unwrap_or(&0)
    }

    /// Total number of addresses with a non-zero balance.
    pub fn address_count(&self) -> usize {
        self.balances.len()
    }

    /// Serialize the snapshot to JSON for embedding in genesis.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Deserialize a snapshot from JSON.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}
