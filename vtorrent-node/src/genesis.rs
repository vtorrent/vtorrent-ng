/// Genesis block creation for the vTorrent 2.0 chain.
///
/// The genesis block contains:
/// 1. A coinbase transaction with the genesis message.
/// 2. A LegacyClaim distribution transaction containing all legacy VTR balances
///    extracted from the legacy chain at block height 1,680,456.
///
/// Legacy chain snapshot statistics:
///   Block height:  1,680,456
///   Chain tip:     2018-01-10 07:24:00 UTC
///   Total supply:  11,589,746.63136877 VTR
///   Addresses:     59,375
///   UTXOs:         79,586
///   Transactions:  3,431,559
use crate::block::{Block, BlockHeader, Transaction, TxOutput, TxType};
use std::sync::LazyLock;

/// The genesis block message.
pub const GENESIS_MESSAGE: &str =
    "vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed";

/// The genesis block timestamp (set at actual launch time).
pub const GENESIS_TIMESTAMP: u32 = 1_700_000_000;

/// The genesis block difficulty target.
pub const GENESIS_BITS: u32 = 0x1e0fffff;

/// The legacy chain snapshot height.
pub const LEGACY_SNAPSHOT_HEIGHT: u32 = 1680456;

/// The legacy chain snapshot date.
pub const LEGACY_SNAPSHOT_DATE: &str = "2018-01-10 07:24:00 UTC";

/// The total legacy supply in satoshis (11,589,746.63136877 VTR).
pub const LEGACY_TOTAL_SUPPLY_SATOSHIS: u64 = 1158974663136877;

/// Total number of legacy addresses in the snapshot.
pub const LEGACY_ADDRESS_COUNT: usize = 59375;

/// Decode a genesis snapshot blob: [u32 LE count][34B ASCII addr][u64 LE balance] * count
pub fn decode_snapshot(bytes: &[u8]) -> Vec<(String, u64)> {
    assert!(bytes.len() >= 4, "snapshot blob too short");
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    assert_eq!(count, LEGACY_ADDRESS_COUNT, "snapshot count mismatch");
    assert_eq!(bytes.len(), 4 + count * 42, "snapshot blob length mismatch");
    let mut out = Vec::with_capacity(count);
    let mut offset = 4;
    for _ in 0..count {
        let addr_bytes = &bytes[offset..offset + 34];
        offset += 34;
        let addr_end = addr_bytes.iter().position(|&b| b == 0).unwrap_or(34);
        let addr = std::str::from_utf8(&addr_bytes[..addr_end])
            .expect("invalid utf8")
            .to_string();
        let bal = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        offset += 8;
        out.push((addr, bal));
    }
    out
}

/// Legacy snapshot: (address, balance_satoshis) pairs sorted by balance descending.
/// Extracted from blk0001.dat at block height 1,680,456.
/// Contains 59,375 addresses with a total of 11,589,746.63136877 VTR.
/// Decoded lazily from `genesis_snapshot.bin` to avoid parsing 59k lines on every fmt/IDE.
const SNAPSHOT_BYTES: &[u8] = include_bytes!("genesis_snapshot.bin");

fn decode_static(bytes: &[u8]) -> &'static [(&'static str, u64)] {
    let vec = decode_snapshot(bytes);
    let leaked: Vec<(&'static str, u64)> = vec
        .into_iter()
        .map(|(s, b)| {
            let static_str: &'static str = Box::leak(s.into_boxed_str());
            (static_str, b)
        })
        .collect();
    Box::leak(leaked.into_boxed_slice())
}

/// Legacy snapshot — decoded lazily from `genesis_snapshot.bin`.
/// This replaces the previous 59k-line const array to improve fmt/IDE performance.
pub static LEGACY_SNAPSHOT: LazyLock<&'static [(&'static str, u64)]> =
    LazyLock::new(|| decode_static(SNAPSHOT_BYTES));

/// Build the OP_RETURN script for a genesis distribution output.
/// These outputs are not directly spendable — they are claimed via LegacyClaim txs.
fn address_to_script(address: &str) -> Vec<u8> {
    let mut script = vec![0x6a]; // OP_RETURN
    let addr_bytes = address.as_bytes();
    script.push(addr_bytes.len() as u8);
    script.extend_from_slice(addr_bytes);
    script
}

/// Create the genesis block for the vTorrent 2.0 chain.
pub fn create_genesis_block() -> Block {
    // TX 0: Genesis coinbase (no spendable output, just the genesis message)
    let coinbase = Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![],
        outputs: vec![TxOutput {
            value: 0,
            script_pubkey: GENESIS_MESSAGE.as_bytes().to_vec(),
        }],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };

    // TX 1: Legacy distribution — one output per legacy address.
    let legacy_outputs: Vec<TxOutput> = LEGACY_SNAPSHOT
        .iter()
        .map(|(addr, satoshis)| TxOutput {
            value: *satoshis,
            script_pubkey: address_to_script(addr),
        })
        .collect();

    let legacy_distribution = Transaction {
        version: 1,
        tx_type: TxType::LegacyClaim,
        inputs: vec![],
        outputs: legacy_outputs,
        lock_time: LEGACY_SNAPSHOT_HEIGHT,
        claim_address: None,
        claim_signature: None,
    };

    let transactions = vec![coinbase, legacy_distribution];

    let mut header = BlockHeader {
        version: 1,
        prev_block_hash: [0u8; 32],
        merkle_root: [0u8; 32],
        timestamp: GENESIS_TIMESTAMP,
        bits: GENESIS_BITS,
        nonce: 0,
        stake_modifier: 0,
    };

    let temp_block = Block {
        header: header.clone(),
        transactions: transactions.clone(),
    };
    header.merkle_root = temp_block.compute_merkle_root();

    Block {
        header,
        transactions,
    }
}

/// Look up the claimable balance for a legacy address in the snapshot.
pub fn get_legacy_balance(address: &str) -> u64 {
    LEGACY_SNAPSHOT
        .iter()
        .find(|(addr, _)| *addr == address)
        .map(|(_, bal)| *bal)
        .unwrap_or(0)
}

/// Check if a legacy address is in the snapshot.
pub fn is_legacy_address(address: &str) -> bool {
    LEGACY_SNAPSHOT.iter().any(|(addr, _)| *addr == address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_sum_matches_documented_supply() {
        // Guards against accidental edits to the embedded snapshot table:
        // the per-address balances must sum to the documented legacy supply.
        let total: u64 = LEGACY_SNAPSHOT.iter().map(|(_, bal)| bal).sum();
        assert_eq!(total, LEGACY_TOTAL_SUPPLY_SATOSHIS);
        assert_eq!(LEGACY_SNAPSHOT.len(), LEGACY_ADDRESS_COUNT);
        // Addresses must be unique — duplicates would allow double claims.
        let addrs: std::collections::HashSet<&str> =
            LEGACY_SNAPSHOT.iter().map(|(a, _)| *a).collect();
        assert_eq!(addrs.len(), LEGACY_SNAPSHOT.len());
    }

    #[test]
    fn test_snapshot_binary_roundtrip() {
        let decoded = decode_snapshot(include_bytes!("genesis_snapshot.bin"));
        assert_eq!(decoded.len(), LEGACY_ADDRESS_COUNT);
        assert_eq!(
            decoded.iter().map(|(_, b)| *b).sum::<u64>(),
            LEGACY_TOTAL_SUPPLY_SATOSHIS
        );
    }

    #[test]
    fn test_genesis_block_creation() {
        let genesis = create_genesis_block();
        assert_eq!(genesis.transactions.len(), 2);
        assert!(genesis.transactions[0].is_coinbase());
        assert!(genesis.transactions[1].is_legacy_claim());
        assert_ne!(genesis.hash(), [0u8; 32]);
    }

    #[test]
    fn test_genesis_merkle_root_valid() {
        let genesis = create_genesis_block();
        let computed = genesis.compute_merkle_root();
        assert_eq!(computed, genesis.header.merkle_root);
    }

    #[test]
    fn test_legacy_snapshot_total_supply() {
        let total: u64 = LEGACY_SNAPSHOT.iter().map(|(_, b)| b).sum();
        assert_eq!(total, LEGACY_TOTAL_SUPPLY_SATOSHIS);
    }

    #[test]
    fn test_legacy_snapshot_address_count() {
        assert_eq!(LEGACY_SNAPSHOT.len(), LEGACY_ADDRESS_COUNT);
    }

    #[test]
    fn test_get_legacy_balance_known() {
        // Largest holder should have a non-zero balance
        let (addr, expected) = LEGACY_SNAPSHOT[0];
        assert_eq!(get_legacy_balance(addr), expected);
    }

    #[test]
    fn test_get_legacy_balance_unknown() {
        assert_eq!(get_legacy_balance("VnotInSnapshot"), 0);
    }

    #[test]
    fn test_genesis_legacy_outputs_count() {
        let genesis = create_genesis_block();
        let dist_tx = &genesis.transactions[1];
        assert_eq!(dist_tx.outputs.len(), LEGACY_ADDRESS_COUNT);
    }

    #[test]
    fn test_genesis_legacy_total_value() {
        let genesis = create_genesis_block();
        let dist_tx = &genesis.transactions[1];
        let total: u64 = dist_tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(total, LEGACY_TOTAL_SUPPLY_SATOSHIS);
    }
}
