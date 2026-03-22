/// Snapshot writer.
///
/// Serializes the UTXO snapshot to two formats:
/// 1. A compact binary format (`.bin`) for embedding in the new client binary.
/// 2. A human-readable JSON format (`.json`) for auditing and community verification.

use std::io::Write;
use crate::{
    error::{Result, SnapshotError},
    utxo_set::UtxoSnapshot,
};

/// Write the snapshot to a JSON file (for community audit and verification).
pub fn write_json(snapshot: &UtxoSnapshot, path: &std::path::Path) -> Result<()> {
    let json = serde_json::to_string_pretty(snapshot)
        .map_err(|e| SnapshotError::Serialization(e.to_string()))?;

    std::fs::write(path, json)?;

    tracing::info!("Wrote JSON snapshot to: {}", path.display());
    Ok(())
}

/// Write the snapshot to a compact binary format for embedding in the client.
///
/// Binary format:
///   [4 bytes: magic "VTR\x01"]
///   [4 bytes: snapshot_height (LE)]
///   [8 bytes: total_supply (LE)]
///   [8 bytes: total_addresses (LE)]
///   [32 bytes: entries_hash (raw bytes)]
///   [8 bytes: created_at (LE)]
///   For each entry:
///     [1 byte: address_len]
///     [N bytes: address (UTF-8)]
///     [8 bytes: balance (LE)]
pub fn write_binary(snapshot: &UtxoSnapshot, path: &std::path::Path) -> Result<()> {
    let mut buf = Vec::with_capacity(snapshot.entries.len() * 50);

    // Magic header
    buf.extend_from_slice(b"VTR\x01");

    // Metadata
    buf.extend_from_slice(&snapshot.metadata.snapshot_height.to_le_bytes());
    buf.extend_from_slice(&snapshot.metadata.total_supply.to_le_bytes());
    buf.extend_from_slice(&snapshot.metadata.total_addresses.to_le_bytes());

    // Entries hash (32 bytes)
    let hash_bytes = hex::decode(&snapshot.metadata.entries_hash)
        .map_err(|e| SnapshotError::Serialization(format!("Invalid hash: {}", e)))?;
    if hash_bytes.len() != 32 {
        return Err(SnapshotError::Serialization("Hash must be 32 bytes".into()));
    }
    buf.extend_from_slice(&hash_bytes);

    // Timestamp
    buf.extend_from_slice(&snapshot.metadata.created_at.to_le_bytes());

    // Entry count
    buf.extend_from_slice(&(snapshot.entries.len() as u64).to_le_bytes());

    // Entries
    for entry in &snapshot.entries {
        let addr_bytes = entry.address.as_bytes();
        if addr_bytes.len() > 255 {
            return Err(SnapshotError::Serialization("Address too long".into()));
        }
        buf.push(addr_bytes.len() as u8);
        buf.extend_from_slice(addr_bytes);
        buf.extend_from_slice(&entry.balance.to_le_bytes());
    }

    // Write to file
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    file.write_all(&buf)?;

    tracing::info!(
        "Wrote binary snapshot to: {} ({} bytes, {} entries)",
        path.display(),
        buf.len(),
        snapshot.entries.len()
    );
    Ok(())
}

/// Print a human-readable summary of the snapshot to stdout.
pub fn print_summary(snapshot: &UtxoSnapshot) {
    let m = &snapshot.metadata;
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║           vTorrent Legacy Chain Snapshot                 ║");
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  Snapshot Height : {:>36} ║", m.snapshot_height);
    println!("║  Total Addresses : {:>36} ║", m.total_addresses);
    println!("║  Total UTXOs     : {:>36} ║", m.total_utxos);
    println!("║  Total Supply    : {:>30} VTR ║", m.total_supply as f64 / 1e8);
    println!("║  Created At      : {:>36} ║", m.created_at);
    println!("║  Best Block      : {:>36} ║", &m.best_block_hash[..16.min(m.best_block_hash.len())]);
    println!("║  Entries Hash    : {:>36} ║", &m.entries_hash[..16.min(m.entries_hash.len())]);
    println!("╚══════════════════════════════════════════════════════════╝");

    // Show top 10 balances
    println!("\nTop 10 Addresses by Balance:");
    let mut top: Vec<_> = snapshot.entries.iter().collect();
    top.sort_by(|a, b| b.balance.cmp(&a.balance));
    for (i, entry) in top.iter().take(10).enumerate() {
        println!(
            "  {:2}. {} — {:.8} VTR",
            i + 1,
            entry.address,
            entry.balance as f64 / 1e8
        );
    }
}
