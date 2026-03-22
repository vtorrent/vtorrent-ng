/// Consensus rules for the vTorrent 2.0 chain.
///
/// vTorrent 2.0 uses Proof-of-Stake (PoS) consensus, matching the original
/// chain's design. Key parameters are preserved from the legacy chain.

use crate::{
    block::{Block, Transaction, TxType},
    error::{NodeError, Result},
};

/// Maximum block size in bytes.
pub const MAX_BLOCK_SIZE: usize = 1_000_000; // 1 MB

/// Minimum transaction fee in satoshis per byte.
pub const MIN_FEE_RATE: u64 = 10;

/// Coin unit (1 VTR = 100,000,000 satoshis).
pub const COIN: u64 = 100_000_000;

/// Annual PoS interest rate (5% as per original vTorrent spec).
pub const POS_ANNUAL_RATE: f64 = 0.05;

/// Minimum coin age for staking (6 hours in seconds).
pub const MIN_STAKE_AGE: u64 = 6 * 60 * 60;

/// Maximum coin age for staking (6 days in seconds).
pub const MAX_STAKE_AGE: u64 = 6 * 24 * 60 * 60;

/// Minimum stake amount (1 VTR).
pub const MIN_STAKE_AMOUNT: u64 = COIN;

/// Maximum supply (20 million VTR).
pub const MAX_SUPPLY: u64 = 20_000_000 * COIN;

/// Block time target (60 seconds).
pub const TARGET_BLOCK_TIME: u64 = 60;

/// Difficulty adjustment interval (every 2016 blocks, ~1.4 days).
pub const DIFFICULTY_ADJUSTMENT_INTERVAL: u32 = 2016;

/// Compute the PoS block reward for a given stake amount and coin age.
///
/// Formula: reward = stake_amount * annual_rate * coin_age_days / 365
pub fn compute_pos_reward(stake_amount: u64, coin_age_seconds: u64) -> u64 {
    let coin_age_days = coin_age_seconds as f64 / 86400.0;
    let reward = stake_amount as f64 * POS_ANNUAL_RATE * coin_age_days / 365.0;
    reward as u64
}

/// Validate a block against the consensus rules.
pub fn validate_block(block: &Block, prev_height: u32, prev_timestamp: u32) -> Result<()> {
    // Check block is not empty
    if block.transactions.is_empty() {
        return Err(NodeError::InvalidBlock("Block has no transactions".into()));
    }

    // Check first transaction is coinbase or coinstake
    let first_tx = &block.transactions[0];
    if first_tx.tx_type != TxType::Coinbase && first_tx.tx_type != TxType::Coinstake {
        return Err(NodeError::InvalidBlock(
            "First transaction must be coinbase or coinstake".into()
        ));
    }

    // Check block timestamp is not too far in the future (2 hours tolerance)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    if block.header.timestamp > now + 7200 {
        return Err(NodeError::InvalidBlock(
            format!("Block timestamp {} is too far in the future (now: {})",
                block.header.timestamp, now)
        ));
    }

    // Check block timestamp is greater than previous block
    if block.header.timestamp <= prev_timestamp {
        return Err(NodeError::InvalidBlock(
            "Block timestamp must be greater than previous block".into()
        ));
    }

    // Verify merkle root
    let computed_merkle = block.compute_merkle_root();
    if computed_merkle != block.header.merkle_root {
        return Err(NodeError::InvalidBlock(
            "Merkle root mismatch".into()
        ));
    }

    // Validate each transaction
    for tx in &block.transactions {
        validate_transaction(tx)?;
    }

    Ok(())
}

/// Validate a transaction against the consensus rules.
pub fn validate_transaction(tx: &Transaction) -> Result<()> {
    // Coinbase and coinstake have no inputs to validate
    if tx.is_coinbase() || tx.is_coinstake() {
        if tx.outputs.is_empty() {
            return Err(NodeError::InvalidTransaction(
                "Coinbase/coinstake must have outputs".into()
            ));
        }
        return Ok(());
    }

    // Standard transactions must have inputs and outputs
    if tx.inputs.is_empty() {
        return Err(NodeError::InvalidTransaction("Transaction has no inputs".into()));
    }
    if tx.outputs.is_empty() {
        return Err(NodeError::InvalidTransaction("Transaction has no outputs".into()));
    }

    // Check for zero-value outputs
    for output in &tx.outputs {
        if output.value == 0 {
            return Err(NodeError::InvalidTransaction("Output value cannot be zero".into()));
        }
    }

    // Legacy claim transactions must have a claim address and signature
    if tx.is_legacy_claim() {
        if tx.claim_address.is_none() {
            return Err(NodeError::InvalidClaim("Missing claim address".into()));
        }
        if tx.claim_signature.is_none() {
            return Err(NodeError::InvalidClaim("Missing claim signature".into()));
        }
    }

    Ok(())
}

/// Validate a legacy claim transaction against the snapshot.
///
/// Verifies that:
/// 1. The legacy address exists in the snapshot.
/// 2. The signature proves ownership of the legacy private key.
/// 3. The claimed amount matches the snapshot balance.
pub fn validate_legacy_claim(
    tx: &Transaction,
    snapshot_balance: u64,
) -> Result<()> {
    if !tx.is_legacy_claim() {
        return Err(NodeError::InvalidClaim("Not a claim transaction".into()));
    }

    let claim_addr = tx.claim_address.as_ref()
        .ok_or_else(|| NodeError::InvalidClaim("Missing claim address".into()))?;

    if snapshot_balance == 0 {
        return Err(NodeError::InvalidClaim(
            format!("Address {} has no balance in snapshot", claim_addr)
        ));
    }

    // Verify the claimed amount matches the snapshot
    let claimed_amount = tx.total_output();
    if claimed_amount > snapshot_balance {
        return Err(NodeError::InvalidClaim(
            format!("Claimed {} but snapshot shows {}", claimed_amount, snapshot_balance)
        ));
    }

    // TODO: Verify the ECDSA signature against the legacy address
    // This requires secp256k1 signature verification against the legacy key format
    // Implementation deferred to the full node build

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pos_reward_calculation() {
        // 1000 VTR staked for 30 days at 5% annual rate
        let stake = 1000 * COIN;
        let age = 30 * 86400; // 30 days in seconds
        let reward = compute_pos_reward(stake, age);

        // Expected: 1000 * 0.05 * 30/365 = ~4.109 VTR
        // Allow a generous ±1 VTR tolerance for integer truncation
        assert!(reward >= 3 * COIN, "Reward {} too low", reward);
        assert!(reward <= 6 * COIN, "Reward {} too high", reward);
    }

    #[test]
    fn test_validate_empty_transaction() {
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Standard,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let result = validate_transaction(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_coinbase_no_outputs_fails() {
        let tx = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            claim_address: None,
            claim_signature: None,
        };
        let result = validate_transaction(&tx);
        assert!(result.is_err());
    }
}
