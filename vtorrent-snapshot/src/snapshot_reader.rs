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
    let entry_count = usize::try_from(read_u64!())
        .map_err(|_| SnapshotError::Serialization("Entry count is too large".into()))?;

    // Cap the allocation: entry_count comes from the file header and must not
    // drive an unbounded allocation. Each entry needs at least 1 (addr_len) +
    // 1 (address byte) + 8 (balance) = 10 bytes, so the count cannot exceed
    // the remaining data size.
    let max_entries = data.len().saturating_sub(cursor) / 10;
    if entry_count > max_entries {
        return Err(SnapshotError::Serialization(
            "Entry count exceeds the remaining snapshot data".into(),
        ));
    }

    let mut entries = Vec::with_capacity(entry_count);
    let mut seen = std::collections::HashSet::with_capacity(entry_count);

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
        if address.is_empty() {
            return Err(SnapshotError::Serialization(
                "Empty snapshot address".into(),
            ));
        }
        if !seen.insert(address.clone()) {
            return Err(SnapshotError::Serialization(format!(
                "Duplicate snapshot address: {address}"
            )));
        }
        cursor += addr_len;

        let balance = read_u64!();

        entries.push(SnapshotEntry {
            address,
            balance,
            utxo_count: 0, // Not stored in binary format
        });
    }

    if cursor != data.len() {
        return Err(SnapshotError::Serialization(
            "Snapshot has trailing bytes".into(),
        ));
    }

    let mut snapshot = UtxoSnapshot {
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
    };
    // Verify the hash in file order before sorting: the writer hashes entries
    // as written, and a third-party snapshot may not be sorted.
    verify_integrity(&snapshot)?;
    if snapshot.metadata.total_addresses != snapshot.entries.len() as u64 {
        return Err(SnapshotError::Serialization(
            "Snapshot address count does not match its entries".into(),
        ));
    }
    let mut total_supply = 0u64;
    for entry in &snapshot.entries {
        total_supply = total_supply
            .checked_add(entry.balance)
            .ok_or_else(|| SnapshotError::Serialization("Snapshot supply overflows u64".into()))?;
    }
    if snapshot.metadata.total_supply != total_supply {
        return Err(SnapshotError::Serialization(
            "Snapshot supply does not match its entries".into(),
        ));
    }
    // lookup_balance() binary-searches entries, which is only correct on
    // sorted data. Sort after verification so lookups stay sound.
    snapshot.entries.sort_by(|a, b| a.address.cmp(&b.address));
    Ok(snapshot)
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

    fn encoded(snapshot: &UtxoSnapshot) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!(
            "vtr-snap-test-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_binary(snapshot, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(path);
        bytes
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encoded(&make_test_snapshot());
        bytes.push(0);
        assert!(parse_binary(&bytes).is_err());
    }

    #[test]
    fn rejects_an_entry_count_larger_than_the_data() {
        let mut bytes = encoded(&make_test_snapshot());
        bytes[64..72].copy_from_slice(&3u64.to_le_bytes());
        assert!(parse_binary(&bytes).is_err());
    }

    #[test]
    fn rejects_duplicate_addresses() {
        use sha2::{Digest, Sha256};

        let mut snapshot = make_test_snapshot();
        snapshot.entries[1].address = snapshot.entries[0].address.clone();
        snapshot.metadata.total_addresses = 2;
        let mut hasher = Sha256::new();
        for entry in &snapshot.entries {
            hasher.update(entry.address.as_bytes());
            hasher.update(entry.balance.to_le_bytes());
        }
        snapshot.metadata.entries_hash = hex::encode(hasher.finalize());
        assert!(parse_binary(&encoded(&snapshot)).is_err());
    }

    #[test]
    fn rejects_metadata_supply_that_does_not_match_entries() {
        let mut bytes = encoded(&make_test_snapshot());
        // total_supply occupies bytes 8..16.
        bytes[8] ^= 1;
        assert!(parse_binary(&bytes).is_err());
    }

    #[test]
    fn rejects_an_entry_hash_mismatch() {
        let mut bytes = encoded(&make_test_snapshot());
        // The first entry's balance starts after magic(4) + height(4) +
        // supply(8) + addresses(8) + hash(32) + created(8) + count(8) +
        // addr_len(1) + "VAddr1"(6).
        let offset = 4 + 4 + 8 + 8 + 32 + 8 + 8 + 1 + 6;
        bytes[offset] ^= 1;
        assert!(parse_binary(&bytes).is_err());
    }

    #[test]
    fn rejects_an_address_count_that_does_not_match_entries() {
        let mut bytes = encoded(&make_test_snapshot());
        // total_addresses occupies bytes 16..24.
        bytes[16] = 3;
        assert!(parse_binary(&bytes).is_err());
    }
}
