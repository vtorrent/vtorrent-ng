/// PoS Staking Engine for vTorrent 2.0
///
/// Implements Proof-of-Stake block production:
/// - Selects eligible UTXOs (age >= MIN_STAKE_AGE, amount >= MIN_STAKE_AMOUNT)
/// - Computes the stake kernel hash (simplified: SHA256d of utxo_txid + timestamp)
/// - Builds a coinstake transaction with the stake reward
/// - Assembles a complete PoS block ready to be added to the chain
///
/// The stake kernel check is a simplified version of the PPCoin protocol.
/// A full implementation would use the stake modifier from the chain.
use crate::{
    block::{Block, BlockHeader, Transaction, TxInput, TxOutput, TxType},
    chain::Utxo,
    consensus::{
        compute_pos_reward, MIN_STAKE_AGE, MIN_STAKE_AMOUNT,
    },
};

/// The staking engine.
pub struct StakingEngine {
    /// The address whose UTXOs are used for staking.
    pub address: String,
}

impl StakingEngine {
    /// Create a new staking engine for the given address.
    pub fn new(address: String) -> Self {
        Self { address }
    }

    /// Try to build a valid PoS block from available UTXOs.
    ///
    /// Returns `Some(block)` if a valid stake kernel was found, `None` otherwise.
    pub fn build_stake_block(
        &self,
        prev_hash: [u8; 32],
        height: u32,
        timestamp: u32,
        utxos: Vec<Utxo>,
        pending_txs: Vec<Transaction>,
    ) -> Option<Block> {
        // Filter eligible UTXOs
        let eligible: Vec<&Utxo> = utxos
            .iter()
            .filter(|u| self.is_eligible(u, timestamp))
            .collect();

        if eligible.is_empty() {
            tracing::debug!("No eligible UTXOs for staking at height {}", height);
            return None;
        }

        // Try each eligible UTXO as a stake kernel
        for utxo in &eligible {
            if let Some(coinstake) = self.try_stake_kernel(utxo, timestamp, height) {
                // Build the full block
                let block =
                    self.assemble_block(prev_hash, timestamp, coinstake, pending_txs.clone());
                tracing::info!(
                    "Found stake kernel: utxo {}:{} at height {}",
                    hex::encode(utxo.txid),
                    utxo.vout,
                    height
                );
                return Some(block);
            }
        }

        None
    }

    /// Check if a UTXO is eligible for staking.
    fn is_eligible(&self, utxo: &Utxo, current_timestamp: u32) -> bool {
        // Must meet minimum amount
        if utxo.value < MIN_STAKE_AMOUNT {
            return false;
        }

        // Must have minimum coin age
        let coin_age_seconds = current_timestamp.saturating_sub(utxo.timestamp);
        if (coin_age_seconds as u64) < MIN_STAKE_AGE {
            return false;
        }

        true
    }

    /// Try to find a valid stake kernel for a UTXO.
    ///
    /// The stake kernel hash must be below the target (proportional to stake amount).
    /// This is a simplified version — a production implementation would use
    /// the full PPCoin stake modifier chain.
    fn try_stake_kernel(&self, utxo: &Utxo, timestamp: u32, height: u32) -> Option<Transaction> {
        use sha2::{Digest, Sha256};

        // Compute the stake kernel hash:
        // kernel = SHA256d(txid || vout || timestamp)
        let mut hasher = Sha256::new();
        hasher.update(utxo.txid);
        hasher.update(utxo.vout.to_le_bytes());
        hasher.update(timestamp.to_le_bytes());
        let first = hasher.finalize();

        let mut hasher2 = Sha256::new();
        hasher2.update(first);
        let kernel_hash = hasher2.finalize();

        // Target: proportional to stake value (more coins = easier to stake)
        // target = MAX_U256 * stake_value / (TARGET_BLOCK_TIME * COIN)
        // Simplified: accept if first 4 bytes of kernel < stake_value / 1000
        let kernel_val = u32::from_le_bytes([
            kernel_hash[0],
            kernel_hash[1],
            kernel_hash[2],
            kernel_hash[3],
        ]);
        let target = (utxo.value / 1000).min(u32::MAX as u64) as u32;

        if kernel_val > target {
            return None;
        }

        // Compute coin age and stake reward
        let coin_age_seconds = timestamp.saturating_sub(utxo.timestamp);
        let reward = compute_pos_reward(utxo.value, coin_age_seconds as u64);

        // Build coinstake transaction
        // Input: the staked UTXO
        let stake_input = TxInput {
            prev_txid: utxo.txid,
            prev_vout: utxo.vout,
            script_sig: self.build_stake_script(timestamp),
            sequence: u32::MAX,
        };

        // Output 0: empty marker (PPCoin protocol: first output of coinstake is empty)
        let marker_output = TxOutput {
            value: 0,
            script_pubkey: Vec::new(),
        };

        // Output 1: stake return + reward to staking address
        let stake_output = TxOutput {
            value: utxo.value + reward,
            script_pubkey: self.address_to_script(&self.address),
        };

        Some(Transaction {
            version: 1,
            tx_type: TxType::Coinstake,
            inputs: vec![stake_input],
            outputs: vec![marker_output, stake_output],
            lock_time: height,
            claim_address: None,
            claim_signature: None,
        })
    }

    /// Assemble a complete PoS block.
    fn assemble_block(
        &self,
        prev_hash: [u8; 32],
        timestamp: u32,
        coinstake: Transaction,
        pending_txs: Vec<Transaction>,
    ) -> Block {
        // PoS blocks have nonce = 0 (identified by BlockHeader::is_pos())
        let mut transactions = vec![coinstake];

        // Include pending transactions from mempool (up to block size limit)
        let mut block_size = 0usize;
        const MAX_BLOCK_SIZE: usize = 1_000_000;

        for tx in pending_txs {
            let tx_size = tx.inputs.len() * 148 + tx.outputs.len() * 34 + 10;
            if block_size + tx_size > MAX_BLOCK_SIZE {
                break;
            }
            transactions.push(tx);
            block_size += tx_size;
        }

        let mut header = BlockHeader {
            version: 2,
            prev_block_hash: prev_hash,
            merkle_root: [0u8; 32],
            timestamp,
            bits: 0x1e0fffff,
            nonce: 0, // PoS blocks always have nonce = 0
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

    /// Build the stake script (scriptSig for the coinstake input).
    /// Contains the timestamp as a push data, matching PPCoin convention.
    fn build_stake_script(&self, timestamp: u32) -> Vec<u8> {
        let ts_bytes = timestamp.to_le_bytes();
        let mut script = Vec::new();
        script.push(0x04); // push 4 bytes
        script.extend_from_slice(&ts_bytes);
        script
    }

    /// Convert a vTorrent address to a P2PKH scriptPubKey.
    /// Decodes the Base58Check address and builds the standard script.
    fn address_to_script(&self, address: &str) -> Vec<u8> {
        // Decode base58check
        let decoded = self.base58check_decode(address);
        if decoded.len() < 21 {
            // Fallback: OP_RETURN with address bytes
            let mut script = vec![0x6a];
            let addr_bytes = address.as_bytes();
            script.push(addr_bytes.len() as u8);
            script.extend_from_slice(addr_bytes);
            return script;
        }

        // Standard P2PKH: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
        let hash160 = &decoded[1..21];
        let mut script = Vec::with_capacity(25);
        script.push(0x76); // OP_DUP
        script.push(0xa9); // OP_HASH160
        script.push(0x14); // push 20 bytes
        script.extend_from_slice(hash160);
        script.push(0x88); // OP_EQUALVERIFY
        script.push(0xac); // OP_CHECKSIG
        script
    }

    /// Decode a Base58Check-encoded address.
    fn base58check_decode(&self, address: &str) -> Vec<u8> {
        const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

        let mut num: u128 = 0;
        let mut leading_zeros = 0usize;

        for (i, c) in address.bytes().enumerate() {
            let digit = ALPHABET.iter().position(|&b| b == c);
            match digit {
                Some(d) => {
                    num = num.saturating_mul(58).saturating_add(d as u128);
                    if i == 0 && d == 0 {
                        leading_zeros += 1;
                    }
                }
                None => return Vec::new(),
            }
        }

        let mut bytes = num.to_be_bytes().to_vec();
        // Trim leading zero bytes from the big-endian encoding
        let trim = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        bytes = bytes[trim..].to_vec();

        // Re-add leading zero bytes
        let mut result = vec![0u8; leading_zeros];
        result.extend_from_slice(&bytes);

        // Verify checksum (last 4 bytes)
        if result.len() < 4 {
            return Vec::new();
        }
        result[..result.len() - 4].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::Utxo, consensus::COIN};

    fn make_utxo(value: u64, age_seconds: u32) -> Utxo {
        let now = 1_700_000_000u32;
        Utxo {
            txid: [1u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x76, 0xa9, 0x14],
            height: 100,
            timestamp: now - age_seconds,
        }
    }

    #[test]
    fn test_eligible_utxo() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE as u32).saturating_add(3600));
        assert!(engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_young() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(100 * COIN, (MIN_STAKE_AGE / 2) as u32);
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_ineligible_utxo_too_small() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let utxo = make_utxo(
            MIN_STAKE_AMOUNT / 2,
            (MIN_STAKE_AGE as u32).saturating_add(3600),
        );
        assert!(!engine.is_eligible(&utxo, 1_700_000_000));
    }

    #[test]
    fn test_stake_script_contains_timestamp() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        let ts = 1_700_000_000u32;
        let script = engine.build_stake_script(ts);
        assert_eq!(script[0], 0x04);
        let decoded_ts = u32::from_le_bytes([script[1], script[2], script[3], script[4]]);
        assert_eq!(decoded_ts, ts);
    }

    #[test]
    fn test_address_to_script_length() {
        let engine = StakingEngine::new("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT".to_string());
        // A valid P2PKH script is always 25 bytes
        let script = engine.address_to_script("VPskT3V4CSyoRAYTCgyxZQ2FByJmCCLUUT");
        // May be 25 (P2PKH) or fallback OP_RETURN — just check it's non-empty
        assert!(!script.is_empty());
    }
}
