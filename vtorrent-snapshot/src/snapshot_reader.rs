/// Snapshot reader.
///
/// Loads the binary snapshot file and provides fast O(log n) balance lookups.
/// This is used by the wallet import wizard to show users their claimable balance.
///
/// In the final client build, the snapshot is embedded via `include_bytes!()`.
use crate::{
    error::{Result, SnapshotError},
    utxo_set::{SnapshotEntry, SnapshotMetadata, UtxoSnapshot},
};

const MAGIC: &[u8; 4] = b"VTR\x01";

/// Load a snapshot from a binary file.
pub fn load_binary(path: &std::path::Path) -> Result<UtxoSnapshot> {
    let data = std::fs::read(path)?;
    parse_binary(&data)
}

/// Parse a snapshot from raw binary bytes (for use with `include_bytes!()`).
pub fn parse_binary(data: &[u8]) -> Result<UtxoSnapshot> {
    if data.len() < 4 || &data[..4] != MAGIC {
        return Err(SnapshotError::Serialization(
            "Invalid snapshot magic bytes".into(),
        ));
    }

    let mut cursor = 4usize;

    macro_rules! read_u32 {
        () => {{
            if cursor + 4 > data.len() {
                return Err(SnapshotError::Serialization(
                    "Unexpected end of snapshot".into(),
                ));
            }
            let v = u32::from_le_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
            ]);
            cursor += 4;
            v
        }};
    }

    macro_rules! read_u64 {
        () => {{
            if cursor + 8 > data.len() {
                return Err(SnapshotError::Serialization(
                    "Unexpected end of snapshot".into(),
                ));
            }
            let v = u64::from_le_bytes([
                data[cursor],
                data[cursor + 1],
                data[cursor + 2],
                data[cursor + 3],
                data[cursor + 4],
                data[cursor + 5],
                data[cursor + 6],
                data[cursor + 7],
            ]);
            cursor += 8;
            v
        }};
    }

    let snapshot_height = read_u32!();
    let total_supply = read_u64!();
    let total_addresses = read_u64!();

    // Read 32-byte hash
    if cursor + 32 > data.len() {
        return Err(SnapshotError::Serialization(
            "Unexpected end of snapshot (hash)".into(),
        ));
    }
    let entries_hash = hex::encode(&data[cursor..cursor + 32]);
    cursor += 32;

    let created_at = read_u64!();
    let entry_count = read_u64!() as usize;

    let mut entries = Vec::with_capacity(entry_count);

    for _ in 0..entry_count {
        if cursor >= data.len() {
            return Err(SnapshotError::Serialization(
                "Unexpected end of entries".into(),
            ));
        }

        let addr_len = data[cursor] as usize;
        cursor += 1;

        if cursor + addr_len > data.len() {
            return Err(SnapshotError::Serialization("Address truncated".into()));
        }

        let address = std::str::from_utf8(&data[cursor..cursor + addr_len])
            .map_err(|e| SnapshotError::Serialization(format!("Invalid address UTF-8: {}", e)))?
            .to_string();
        cursor += addr_len;

        let balance = read_u64!();

        entries.push(SnapshotEntry {
            address,
            balance,
            utxo_count: 0, // Not stored in binary format
        });
    }

    Ok(UtxoSnapshot {
        metadata: SnapshotMetadata {
            snapshot_height,
            total_addresses,
            total_supply,
            total_utxos: entry_count as u64,
            entries_hash,
            created_at,
            best_block_hash: String::new(),
        },
        entries,
    })
}

/// Verify the integrity of a loaded snapshot by recomputing the entries hash.
pub fn verify_integrity(snapshot: &UtxoSnapshot) -> Result<()> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    for entry in &snapshot.entries {
        hasher.update(entry.address.as_bytes());
        hasher.update(entry.balance.to_le_bytes());
    }
    let computed = hex::encode(hasher.finalize());

    if computed != snapshot.metadata.entries_hash {
        return Err(SnapshotError::IntegrityFailed {
            expected: snapshot.metadata.entries_hash.clone(),
            actual: computed,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        snapshot_writer::write_binary,
        utxo_set::{SnapshotEntry, SnapshotMetadata, UtxoSnapshot},
    };

    fn make_test_snapshot() -> UtxoSnapshot {
        use sha2::{Digest, Sha256};
        let entries = vec![
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
        ];
        let mut hasher = Sha256::new();
        for e in &entries {
            hasher.update(e.address.as_bytes());
            hasher.update(e.balance.to_le_bytes());
        }
        let entries_hash = hex::encode(hasher.finalize());

        UtxoSnapshot {
            metadata: SnapshotMetadata {
                snapshot_height: 500_000,
                total_addresses: 2,
                total_supply: 300_000_000,
                total_utxos: 3,
                entries_hash,
                created_at: 1_700_000_000,
                best_block_hash: "abc123".into(),
            },
            entries,
        }
    }

    #[test]
    fn test_binary_roundtrip() {
        let snapshot = make_test_snapshot();
        let tmp = std::env::temp_dir().join("vtorrent_test_snapshot.bin");

        write_binary(&snapshot, &tmp).expect("Write failed");
        let loaded = load_binary(&tmp).expect("Load failed");

        assert_eq!(loaded.metadata.snapshot_height, 500_000);
        assert_eq!(loaded.metadata.total_supply, 300_000_000);
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].address, "VAddr1");
        assert_eq!(loaded.entries[0].balance, 100_000_000);
        assert_eq!(loaded.entries[1].address, "VAddr2");
        assert_eq!(loaded.entries[1].balance, 200_000_000);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_integrity_verification() {
        let snapshot = make_test_snapshot();
        verify_integrity(&snapshot).expect("Integrity check failed");
    }
}
