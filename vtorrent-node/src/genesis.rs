/// Genesis block creation for the vTorrent 2.0 chain.
///
/// The genesis block contains:
/// 1. A coinbase transaction with the genesis message.
/// 2. The snapshot of all legacy VTR balances (embedded as LegacyClaim outputs).
///    In the actual release, this will be loaded from the embedded snapshot binary.

use crate::block::{Block, BlockHeader, Transaction, TxOutput, TxType};

/// The genesis block message (like Bitcoin's "Chancellor on brink of second bailout").
pub const GENESIS_MESSAGE: &str =
    "vTorrent 2.0 - Revived 2025 - Old holders made whole - No exchange needed";

/// The genesis block timestamp (will be set at actual launch time).
pub const GENESIS_TIMESTAMP: u32 = 1_700_000_000;

/// The genesis block difficulty target.
pub const GENESIS_BITS: u32 = 0x1e0fffff;

/// Create the genesis block for the vTorrent 2.0 chain.
pub fn create_genesis_block() -> Block {
    // Genesis coinbase transaction
    let coinbase = Transaction {
        version: 1,
        tx_type: TxType::Coinbase,
        inputs: vec![],
        outputs: vec![
            TxOutput {
                value: 0, // Genesis coinbase has no spendable output
                script_pubkey: GENESIS_MESSAGE.as_bytes().to_vec(),
            }
        ],
        lock_time: 0,
        claim_address: None,
        claim_signature: None,
    };

    let transactions = vec![coinbase];

    let mut header = BlockHeader {
        version: 1,
        prev_block_hash: [0u8; 32],
        merkle_root: [0u8; 32],
        timestamp: GENESIS_TIMESTAMP,
        bits: GENESIS_BITS,
        nonce: 0,
        stake_modifier: 0,
    };

    // Build a temporary block to compute the merkle root
    let temp_block = Block {
        header: header.clone(),
        transactions: transactions.clone(),
    };
    header.merkle_root = temp_block.compute_merkle_root();

    Block { header, transactions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block_creation() {
        let genesis = create_genesis_block();
        assert_eq!(genesis.transactions.len(), 1);
        assert!(genesis.transactions[0].is_coinbase());
        assert_ne!(genesis.hash(), [0u8; 32]);
    }

    #[test]
    fn test_genesis_merkle_root_valid() {
        let genesis = create_genesis_block();
        let computed = genesis.compute_merkle_root();
        assert_eq!(computed, genesis.header.merkle_root);
    }
}
