/// Genesis block creation for the vTorrent 2.0 chain.
///
/// The genesis block contains:
/// 1. A coinbase transaction with the genesis message.
/// 2. A LegacyClaim distribution transaction containing all legacy VTR balances
///    extracted from the legacy chain at block height 1,680,456 (2018-01-10).
///
/// Legacy chain snapshot statistics:
///   Block height:  1,680,456
///   Chain tip:     2018-01-10 07:24:00 UTC
///   Total supply:  692,429.60310419 VTR
///   Addresses:     33
///   UTXOs:         434

use crate::block::{Block, BlockHeader, Transaction, TxOutput, TxType};

/// The genesis block message.
pub const GENESIS_MESSAGE: &str =
    "vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed";

/// The genesis block timestamp (set at actual launch time).
pub const GENESIS_TIMESTAMP: u32 = 1_700_000_000;

/// The genesis block difficulty target.
pub const GENESIS_BITS: u32 = 0x1e0fffff;

/// The legacy chain snapshot height.
pub const LEGACY_SNAPSHOT_HEIGHT: u32 = 1_680_456;

/// The legacy chain snapshot date.
pub const LEGACY_SNAPSHOT_DATE: &str = "2018-01-10 07:24:00 UTC";

/// The total legacy supply in satoshis (692,429.60310419 VTR).
pub const LEGACY_TOTAL_SUPPLY_SATOSHIS: u64 = 69_242_960_310_419;

/// Legacy snapshot: (address, balance_satoshis) pairs sorted by balance descending.
/// Extracted from blk0001.dat at block height 1,680,456.
pub const LEGACY_SNAPSHOT: &[(&str, u64)] = &[
    ("VUqcYWFiTYUKBaAyde8TSmw33JVAqvcBNP", 60_120_000_000_000),
    ("VH6w62jDRYpYHjR2eJjFzXRC4MQvvs93a6",  2_260_000_000_000),
    ("VPjuqYsi2Es1Jab9BLkyWp48DLrgqv2B11",  2_220_000_000_000),
    ("VE9kuAWeMA87sfP6LYuqM1cbdvXw1EyHvs",  1_180_000_000_000),
    ("VBsQyY8246GsobLH7JpmiLBHPK3LoDGRAK",    880_000_000_000),
    ("VTc6Gbcz8Q4qeWYZYvpRNvxHtvH1DsaqPi",    820_000_000_000),
    ("VLh8QRmG4odpqAL884XJ4zbrYj8tvQccKS",    580_000_000_000),
    ("VDGD3dr13LWPm2jCgAXEhb2LdTX9yYE717",    320_000_000_000),
    ("VTw6Dv8Zftc55RdiUZwtJAa81BAVA1Tcbb",    140_000_000_000),
    ("VSzK7EnT1ZcCtFz2ygr8Fxh94fAcdiUrEH",    100_000_000_000),
    ("VZ6TWVhSa6kYkeVaCSUm6HpgxaeRLvMXJp",    100_000_000_000),
    ("VXrgoEFD4x8wcsddZyUhk2F5sb6Dfq1ocb",     80_000_000_000),
    ("VQ3p9X1za2s1teF6rPZPVPj4ZP9bYNNCxf",     80_000_000_000),
    ("VQyvLbd5QgQoiV5bDhbLga8HRng52aqaVt",     74_879_000_000),
    ("VGVQs9F6YG1dE9hc1iegSueR6ePHP2Roa9",     60_000_000_000),
    ("VTefTwBJwGqzkJAZ7y5aqoKrPtBSwuYYQR",     40_000_000_000),
    ("VHQHa48XHFXj6CLHMxGMRVqiHfQfoWxskN",     37_432_000_000),
    ("VYtHydKEfS6UCBA6WF9gAZoK5oGPk6B7pK",     28_851_831_651),
    ("VJ8vuWRZfWJQa88JrjPzEZSn7CxVZcoudT",     25_738_599_377),
    ("VW23aoG1Te2HA6u9o3b9NYBwVg3wirj39p",     20_000_000_000),
    ("VDMTcacG8ar5UV54i8akFfvhNxLaHecZek",     20_000_000_000),
    ("VRFLMfHoeRwHHQLHJWSJNGEwJZWkuMD85h",     10_238_837_767),
    ("VPUdc7wMRS4Ykw3VuHqwdtzfkgptzRKLK2",      7_849_235_465),
    ("VLsZff1vMJczee3YftvZGJJxUpZECNXrqQ",      7_476_510_840),
    ("VKQpP1osy7CmvGu4d1ow7qGNW8ZT3yahUV",      7_168_069_523),
    ("VB5XGyLnHjz8MyfmdwBxmrFTxdhbFb9dWc",      7_096_326_178),
    ("VF25rjjuMW37b8MPY5o23Xwna5nHeDKs91",      5_705_179_394),
    ("VEpg2eWLK5jf92Lg4kz2L79jtuYxRY3G2N",      5_204_418_223),
    ("VXy57BCFbmfvcicRi4io5U8skXDxc98PtV",      5_144_519_145),
    ("VGpNPLAGVbSyr2XaqQ3RMrsma6n69w1WS2",        132_338_007),
    ("VY9ZZaUSXu8JyAnqhBB77NjJYKCinNe9z6",         21_444_849),
    ("VXTbhZmLD3pFTY4M51YuFJgL9P21m9o4dF",         11_000_000),
    ("VL3MBZ9UgoDUHcxJgi2Hu1Umh1SoEwx4gb",         11_000_000),
];

/// Build the P2PKH scriptPubKey for a given address string.
/// For the genesis block, we encode the address as UTF-8 bytes in the script.
/// The actual P2PKH script will be derived from the address during claim validation.
fn address_to_script(address: &str) -> Vec<u8> {
    // Encode as: OP_RETURN <address_bytes> for genesis distribution outputs.
    // These outputs are not directly spendable — they are claimed via LegacyClaim txs.
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
        outputs: vec![
            TxOutput {
                value: 0,
                script_pubkey: GENESIS_MESSAGE.as_bytes().to_vec(),
            }
        ],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };

    // TX 1: Legacy distribution — one output per legacy address.
    // These outputs are locked and can only be claimed by proving ownership
    // of the corresponding legacy private key via a LegacyClaim transaction.
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

    // Compute the merkle root from the transactions
    let temp_block = Block {
        header: header.clone(),
        transactions: transactions.clone(),
    };
    header.merkle_root = temp_block.compute_merkle_root();

    Block { header, transactions }
}

/// Look up the claimable balance for a legacy address in the snapshot.
/// Returns the balance in satoshis, or 0 if the address is not in the snapshot.
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
    fn test_genesis_block_creation() {
        let genesis = create_genesis_block();
        // Should have coinbase + legacy distribution
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
        assert_eq!(total, LEGACY_TOTAL_SUPPLY_SATOSHIS,
            "Snapshot total should be 692,429.60310419 VTR");
    }

    #[test]
    fn test_legacy_snapshot_address_count() {
        assert_eq!(LEGACY_SNAPSHOT.len(), 33,
            "Snapshot should contain exactly 33 legacy addresses");
    }

    #[test]
    fn test_legacy_snapshot_largest_holder() {
        let largest = LEGACY_SNAPSHOT[0];
        assert_eq!(largest.0, "VUqcYWFiTYUKBaAyde8TSmw33JVAqvcBNP");
        assert_eq!(largest.1, 60_120_000_000_000); // 601,200 VTR
    }

    #[test]
    fn test_get_legacy_balance() {
        assert_eq!(get_legacy_balance("VUqcYWFiTYUKBaAyde8TSmw33JVAqvcBNP"), 60_120_000_000_000);
        assert_eq!(get_legacy_balance("VnotInSnapshot"), 0);
    }

    #[test]
    fn test_genesis_legacy_outputs_count() {
        let genesis = create_genesis_block();
        let dist_tx = &genesis.transactions[1];
        assert_eq!(dist_tx.outputs.len(), 33);
    }

    #[test]
    fn test_genesis_legacy_total_value() {
        let genesis = create_genesis_block();
        let dist_tx = &genesis.transactions[1];
        let total: u64 = dist_tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(total, LEGACY_TOTAL_SUPPLY_SATOSHIS);
    }
}
