use crate::{
    block_parser::{parse_utxo_value, ParsedUtxo},
    error::Result,
    leveldb_reader::RawUtxo,
};
/// UTXO set aggregator.
///
/// Processes all parsed UTXOs and aggregates them by address to produce
/// the final snapshot: a map of address → total balance.
use std::collections::HashMap;

/// An aggregated balance entry for the snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotEntry {
    /// The legacy vTorrent address (starts with 'V' or 'X').
    pub address: String,
    /// Total balance in satoshis across all UTXOs.
    pub balance: u64,
    /// Number of UTXOs contributing to this balance.
    pub utxo_count: u32,
}

/// The full UTXO snapshot ready for serialization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UtxoSnapshot {
    /// Snapshot metadata.
    pub metadata: SnapshotMetadata,
    /// All address → balance entries, sorted by address for deterministic output.
    pub entries: Vec<SnapshotEntry>,
}

/// Snapshot metadata for integrity verification.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotMetadata {
    /// The block height at which the snapshot was taken.
    pub snapshot_height: u32,
    /// Total number of addresses with non-zero balance.
    pub total_addresses: u64,
    /// Total supply in satoshis at snapshot height.
    pub total_supply: u64,
    /// Total number of UTXOs processed.
    pub total_utxos: u64,
    /// SHA-256 hash of the sorted entries (for integrity verification).
    pub entries_hash: String,
    /// Unix timestamp when the snapshot was created.
    pub created_at: u64,
    /// The legacy chain's best block hash at snapshot time (hex).
    pub best_block_hash: String,
}

/// Process raw UTXOs from LevelDB and aggregate them into a snapshot.
pub fn build_snapshot(
    raw_utxos: Vec<RawUtxo>,
    snapshot_height: u32,
    best_block_hash: &str,
) -> Result<UtxoSnapshot> {
    let total_raw = raw_utxos.len() as u64;
    tracing::info!("Processing {} raw UTXOs...", total_raw);

    // Parse all UTXOs
    let mut parsed: Vec<ParsedUtxo> = Vec::with_capacity(raw_utxos.len());
    let mut parse_errors = 0u64;

    for raw in raw_utxos {
        match parse_utxo_value(&raw) {
            Ok(p) => parsed.push(p),
            Err(e) => {
                tracing::debug!("Parse error for UTXO: {}", e);
                parse_errors += 1;
            }
        }
    }

    tracing::info!(
        "Parsed {} UTXOs ({} errors skipped)",
        parsed.len(),
        parse_errors
    );

    // Aggregate by address
    let mut balances: HashMap<String, (u64, u32)> = HashMap::new(); // address -> (balance, utxo_count)

    let mut no_address_count = 0u64;
    let mut no_address_value = 0u64;

    for utxo in &parsed {
        if let Some(addr) = &utxo.address {
            let entry = balances.entry(addr.clone()).or_insert((0, 0));
            entry.0 += utxo.amount;
            entry.1 += 1;
        } else {
            no_address_count += 1;
            no_address_value += utxo.amount;
        }
    }

    if no_address_count > 0 {
        tracing::warn!(
            "{} UTXOs ({} satoshis) could not be mapped to an address (non-standard scripts)",
            no_address_count,
            no_address_value
        );
    }

    // Build sorted entries (sort by address for deterministic output)
    let mut entries: Vec<SnapshotEntry> = balances
        .into_iter()
        .filter(|(_, (balance, _))| *balance > 0)
        .map(|(address, (balance, utxo_count))| SnapshotEntry {
            address,
            balance,
            utxo_count,
        })
        .collect();

    entries.sort_by(|a, b| a.address.cmp(&b.address));

    let total_addresses = entries.len() as u64;
    let total_supply: u64 = entries.iter().map(|e| e.balance).sum();

    tracing::info!(
        "Snapshot: {} addresses, {} total VTR ({} satoshis)",
        total_addresses,
        total_supply as f64 / 100_000_000.0,
        total_supply
    );

    // Compute integrity hash over the sorted entries
    let entries_hash = compute_entries_hash(&entries);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(UtxoSnapshot {
        metadata: SnapshotMetadata {
            snapshot_height,
            total_addresses,
            total_supply,
            total_utxos: parsed.len() as u64,
            entries_hash,
            created_at: now,
            best_block_hash: best_block_hash.to_string(),
        },
        entries,
    })
}

/// Compute a SHA-256 hash over the sorted snapshot entries for integrity verification.
fn compute_entries_hash(entries: &[SnapshotEntry]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.address.as_bytes());
        hasher.update(entry.balance.to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Look up the balance for a specific address in the snapshot.
/// Uses binary search on the sorted entries for O(log n) lookup.
pub fn lookup_balance(snapshot: &UtxoSnapshot, address: &str) -> u64 {
    match snapshot
        .entries
        .binary_search_by(|e| e.address.as_str().cmp(address))
    {
        Ok(idx) => snapshot.entries[idx].balance,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_balance_found() {
        let snapshot = UtxoSnapshot {
            metadata: SnapshotMetadata {
                snapshot_height: 100,
                total_addresses: 2,
                total_supply: 300_000_000,
                total_utxos: 3,
                entries_hash: String::new(),
                created_at: 0,
                best_block_hash: String::new(),
            },
            entries: vec![
                SnapshotEntry {
                    address: "VAddr1".into(),
                    balance: 100_000_000,
                    utxo_count: 1,
                },
                SnapshotEntry {
                    address: "VAddr2".into(),
                    balance: 200_000_000,
                    utxo_count: 2,
                },
            ],
        };

        assert_eq!(lookup_balance(&snapshot, "VAddr1"), 100_000_000);
        assert_eq!(lookup_balance(&snapshot, "VAddr2"), 200_000_000);
        assert_eq!(lookup_balance(&snapshot, "VAddr3"), 0);
    }
}
