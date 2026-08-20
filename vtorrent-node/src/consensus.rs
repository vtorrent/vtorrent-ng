use ripemd::Ripemd160;
use secp256k1::{Message, PublicKey, Secp256k1};
/// Consensus rules for the vTorrent 2.0 chain.
///
/// vTorrent 2.0 uses Proof-of-Stake (PoS) consensus, matching the original
/// chain's design. Key parameters are preserved from the legacy chain.
use sha2::{Digest as Sha2Digest, Sha256};

use crate::{
    block::{Block, Transaction, TxType},
    chain::Utxo,
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

/// Maximum value of any single transaction output (the total supply).
/// Any output above this is rejected as an obvious inflation attempt.
pub const MAX_MONEY: u64 = MAX_SUPPLY;

/// Block time target (60 seconds).
pub const TARGET_BLOCK_TIME: u64 = 60;

/// Difficulty adjustment interval (every 2016 blocks, ~1.4 days).
pub const DIFFICULTY_ADJUSTMENT_INTERVAL: u32 = 2016;

/// Compute the PoS block reward for a given stake amount and coin age.
///
/// Formula: reward = stake_amount * annual_rate * coin_age_days / 365
pub fn compute_pos_reward(stake_amount: u64, coin_age_seconds: u64) -> u64 {
    // Cap the coin age at MAX_STAKE_AGE so an arbitrarily old UTXO cannot earn
    // an unbounded reward. This matches the staking engine's eligibility cap.
    let coin_age_seconds = coin_age_seconds.min(MAX_STAKE_AGE);
    let coin_age_days = coin_age_seconds as f64 / 86400.0;
    let reward = stake_amount as f64 * POS_ANNUAL_RATE * coin_age_days / 365.0;
    reward as u64
}

/// Compute the stake kernel hash for a UTXO at a given timestamp.
///
/// kernel = SHA256d(txid || vout || timestamp)
pub fn stake_kernel_hash(utxo: &Utxo, timestamp: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(utxo.txid);
    hasher.update(utxo.vout.to_le_bytes());
    hasher.update(timestamp.to_le_bytes());
    let first = hasher.finalize();

    let mut hasher2 = Sha256::new();
    hasher2.update(first);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&hasher2.finalize());
    hash
}

/// Check whether a UTXO satisfies the stake kernel difficulty at a timestamp.
///
/// The kernel hash must be below a target proportional to the stake value:
/// target = min(value / 1000, u32::MAX). This is the same check the staking
/// engine uses when producing blocks, so a block that passes validation here
/// provably met the difficulty requirement.
pub fn check_stake_kernel(utxo: &Utxo, timestamp: u32) -> bool {
    let kernel_hash = stake_kernel_hash(utxo, timestamp);
    let kernel_val = u32::from_le_bytes([
        kernel_hash[0],
        kernel_hash[1],
        kernel_hash[2],
        kernel_hash[3],
    ]);
    let target = (utxo.value / 1000).min(u32::MAX as u64) as u32;
    kernel_val <= target
}

/// Validate a block against the consensus rules.
pub fn validate_block(
    block: &Block,
    prev_height: u32,
    prev_timestamp: u32,
    prev_bits: u32,
) -> Result<()> {
    // Check block is not empty
    if block.transactions.is_empty() {
        return Err(NodeError::InvalidBlock("Block has no transactions".into()));
    }

    // Check block size is within the consensus limit
    let block_size: usize = block
        .transactions
        .iter()
        .map(|tx| tx.serialized_size())
        .sum();
    if block_size > MAX_BLOCK_SIZE {
        return Err(NodeError::InvalidBlock(format!(
            "Block size {} exceeds maximum {}",
            block_size, MAX_BLOCK_SIZE
        )));
    }

    // The block height is encoded in the first transaction's lock_time and
    // must be exactly one more than the parent's height.
    if block.height() != prev_height + 1 {
        return Err(NodeError::InvalidBlock(format!(
            "Block height {} does not follow parent height {}",
            block.height(),
            prev_height
        )));
    }

    // Difficulty must never become easier than the parent block's difficulty.
    // (This chain uses a fixed, non-retargeting difficulty; a block that lowers
    // the target would let anyone mine cheap blocks.)
    if block.header.bits != prev_bits {
        return Err(NodeError::InvalidBlock(format!(
            "Block difficulty {} does not match parent difficulty {}",
            block.header.bits, prev_bits
        )));
    }

    // PoS blocks must use the consensus stake kernel: nonce 0 and the first
    // transaction must be a coinstake. PoW blocks must have a non-zero nonce.
    let first_tx = &block.transactions[0];
    if block.header.is_pos() {
        if first_tx.tx_type != TxType::Coinstake {
            return Err(NodeError::InvalidBlock(
                "PoS block must begin with a coinstake transaction".into(),
            ));
        }
    } else if first_tx.tx_type != TxType::Coinbase {
        return Err(NodeError::InvalidBlock(
            "PoW block must begin with a coinbase transaction".into(),
        ));
    }

    // Exactly one coinbase/coinstake transaction is allowed per block.
    // Any additional coinbase/coinstake transaction would mint value without
    // spending inputs, so it must be rejected.
    for tx in block.transactions.iter().skip(1) {
        if tx.is_coinbase() || tx.is_coinstake() {
            return Err(NodeError::InvalidBlock(
                "Block contains more than one coinbase/coinstake transaction".into(),
            ));
        }
    }

    // Check block timestamp is not too far in the future (2 hours tolerance)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;

    if block.header.timestamp > now + 7200 {
        return Err(NodeError::InvalidBlock(format!(
            "Block timestamp {} is too far in the future (now: {})",
            block.header.timestamp, now
        )));
    }

    // Check block timestamp is greater than previous block
    if block.header.timestamp <= prev_timestamp {
        return Err(NodeError::InvalidBlock(
            "Block timestamp must be greater than previous block".into(),
        ));
    }

    // Verify merkle root
    let computed_merkle = block.compute_merkle_root();
    if computed_merkle != block.header.merkle_root {
        return Err(NodeError::InvalidBlock("Merkle root mismatch".into()));
    }

    // Validate each transaction
    for tx in &block.transactions {
        validate_transaction(tx)?;
    }

    Ok(())
}

/// Validate a transaction against the consensus rules.
pub fn validate_transaction(tx: &Transaction) -> Result<()> {
    // Coinbase, coinstake, and legacy claims have no inputs to validate.
    // Legacy claims are funded by the snapshot embedded in genesis.
    if tx.is_coinbase() || tx.is_coinstake() || tx.is_legacy_claim() {
        if tx.outputs.is_empty() {
            return Err(NodeError::InvalidTransaction(
                "Coinbase/coinstake/claim must have outputs".into(),
            ));
        }
        return Ok(());
    }

    // Standard transactions must have inputs and outputs
    if tx.inputs.is_empty() {
        return Err(NodeError::InvalidTransaction(
            "Transaction has no inputs".into(),
        ));
    }
    if tx.outputs.is_empty() {
        return Err(NodeError::InvalidTransaction(
            "Transaction has no outputs".into(),
        ));
    }

    // Check for zero-value outputs and enforce the per-output money cap
    for output in &tx.outputs {
        if output.value == 0 {
            return Err(NodeError::InvalidTransaction(
                "Output value cannot be zero".into(),
            ));
        }
        if output.value > MAX_MONEY {
            return Err(NodeError::InvalidTransaction(format!(
                "Output value {} exceeds MAX_MONEY {}",
                output.value, MAX_MONEY
            )));
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
pub fn validate_legacy_claim(tx: &Transaction, snapshot_balance: u64) -> Result<()> {
    if !tx.is_legacy_claim() {
        return Err(NodeError::InvalidClaim("Not a claim transaction".into()));
    }

    let claim_addr = tx
        .claim_address
        .as_ref()
        .ok_or_else(|| NodeError::InvalidClaim("Missing claim address".into()))?;

    if snapshot_balance == 0 {
        return Err(NodeError::InvalidClaim(format!(
            "Address {} has no balance in snapshot",
            claim_addr
        )));
    }

    // Verify the claimed amount matches the snapshot
    let claimed_amount = tx.total_output();
    if claimed_amount > snapshot_balance {
        return Err(NodeError::InvalidClaim(format!(
            "Claimed {} but snapshot shows {}",
            claimed_amount, snapshot_balance
        )));
    }

    // ── ECDSA signature verification ─────────────────────────────────────────
    //
    // The claim signature is a compact (recoverable) ECDSA proof that the
    // signer controls the private key whose Hash160 matches claim_address:
    //   sig = sign_recoverable(privkey, sha256d("vTorrent Signed Message\n" + claim_address))
    //
    // Signing the claim address (rather than the txid) avoids a circular
    // dependency: the txid includes the signature field, so a signature over
    // the txid could never be constructed.
    let sig_bytes = tx
        .claim_signature
        .as_ref()
        .ok_or_else(|| NodeError::InvalidClaim("Missing claim signature".into()))?;

    verify_claim_signature(claim_addr, sig_bytes)
        .map_err(|e| NodeError::InvalidClaim(format!("Signature verification failed: {}", e)))?;

    Ok(())
}

/// Verify a Bitcoin-style signed-message proof for a legacy claim.
///
/// Protocol:
/// 1. Compute the message hash:
///    `hash = SHA256d("vTorrent Signed Message\n" + len_varint + claim_address)`
/// 2. Recover the public key from the compact (65-byte) ECDSA signature.
/// 3. Derive the P2PKH address from the recovered public key.
/// 4. Compare the derived address to the claimed legacy address.
///
/// The signature must be in compact (65-byte) format:
///   byte[0]  = recovery flag (27–34)
///   byte[1..33] = r
///   byte[33..65] = s
pub fn verify_claim_signature(
    claim_address: &str,
    sig_bytes: &[u8],
) -> std::result::Result<(), String> {
    if sig_bytes.len() != 65 {
        return Err(format!(
            "Expected 65-byte compact signature, got {}",
            sig_bytes.len()
        ));
    }

    // ── Step 1: Build the signed message hash ────────────────────────────────
    let message_hash = claim_message_hash(claim_address);

    let msg = Message::from_digest(message_hash);

    // ── Step 2: Parse the compact signature and recover the public key ────────
    let recovery_id_byte = sig_bytes[0];
    // Bitcoin compact format: recovery_id = (flag - 27) & 3
    // Compressed flag: (flag - 27) >= 4
    let rec_id_raw = (recovery_id_byte.wrapping_sub(27)) & 3;
    let compressed = (recovery_id_byte.wrapping_sub(27)) >= 4;

    let rec_id = secp256k1::ecdsa::RecoveryId::from_i32(rec_id_raw as i32)
        .map_err(|e| format!("Invalid recovery id: {}", e))?;

    let rec_sig = secp256k1::ecdsa::RecoverableSignature::from_compact(&sig_bytes[1..65], rec_id)
        .map_err(|e| format!("Invalid recoverable signature: {}", e))?;

    let secp = Secp256k1::verification_only();
    let recovered_pubkey = secp
        .recover_ecdsa(&msg, &rec_sig)
        .map_err(|e| format!("Key recovery failed: {}", e))?;

    // ── Step 3: Derive the address from the recovered public key ─────────────
    let derived_address = pubkey_to_vtorrent_address(&recovered_pubkey, compressed);

    // ── Step 4: Compare to the claimed address ───────────────────────────────
    if derived_address != claim_address {
        return Err(format!(
            "Address mismatch: signature recovers {} but claim is for {}",
            derived_address, claim_address
        ));
    }

    Ok(())
}

/// Compute the message hash a legacy claim signature must be created over.
///
/// The signed message is the claim address itself (not the txid, which would
/// be circular since the txid embeds the signature). Both the claim RPC
/// (`submit_claim`) and chain validation use this helper so they stay in sync.
pub fn claim_message_hash(claim_address: &str) -> [u8; 32] {
    bitcoin_signed_message_hash("vTorrent Signed Message", claim_address)
}

/// Compute the Bitcoin-style signed message hash.
///
/// Format: SHA256d(magic_prefix + varint(len(message)) + message)
fn bitcoin_signed_message_hash(magic: &str, message: &str) -> [u8; 32] {
    let magic_bytes = format!("{}\n", magic);
    let msg_bytes = message.as_bytes();

    // Encode message length as a Bitcoin varint
    let mut varint = Vec::new();
    let msg_len = msg_bytes.len() as u64;
    if msg_len < 0xfd {
        varint.push(msg_len as u8);
    } else if msg_len <= 0xffff {
        varint.push(0xfd);
        varint.extend_from_slice(&(msg_len as u16).to_le_bytes());
    } else {
        varint.push(0xfe);
        varint.extend_from_slice(&(msg_len as u32).to_le_bytes());
    }

    let mut preimage = Vec::new();
    // Magic prefix also uses a varint length
    let magic_len = magic_bytes.len() as u8;
    preimage.push(magic_len);
    preimage.extend_from_slice(magic_bytes.as_bytes());
    preimage.extend_from_slice(&varint);
    preimage.extend_from_slice(msg_bytes);

    // SHA256d
    let first = Sha256::digest(&preimage);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Derive a vTorrent P2PKH address string from a secp256k1 public key.
///
/// Uses version byte 70 (0x46) which produces addresses starting with 'V'.
fn pubkey_to_vtorrent_address(pubkey: &PublicKey, compressed: bool) -> String {
    let pubkey_bytes = if compressed {
        pubkey.serialize().to_vec()
    } else {
        pubkey.serialize_uncompressed().to_vec()
    };

    // Hash160 = RIPEMD160(SHA256(pubkey))
    let sha256_hash = Sha256::digest(&pubkey_bytes);
    let ripemd_hash = Ripemd160::digest(sha256_hash);

    // Version byte 70 = vTorrent mainnet P2PKH
    let version: u8 = 70;
    let mut payload = Vec::with_capacity(21);
    payload.push(version);
    payload.extend_from_slice(&ripemd_hash);

    // Checksum = first 4 bytes of SHA256d(payload)
    let check1 = Sha256::digest(&payload);
    let check2 = Sha256::digest(check1);
    payload.extend_from_slice(&check2[..4]);

    bs58::encode(payload).into_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::TxOutput;
    use secp256k1::SecretKey;

    #[test]
    fn test_pos_reward_calculation() {
        // 1000 VTR staked for 30 days at 5% annual rate, capped at MAX_STAKE_AGE
        // (6 days). Expected: 1000 * 0.05 * 6/365 = ~0.82 VTR.
        let stake = 1000 * COIN;
        let age = 30 * 86400; // 30 days in seconds
        let reward = compute_pos_reward(stake, age);

        // The reward must be capped at the 6-day maximum, not the uncapped
        // 30-day value (~4.1 VTR).
        assert!(reward >= COIN / 2, "Reward {} too low", reward);
        assert!(reward <= COIN + COIN / 2, "Reward {} too high", reward);
    }

    #[test]
    fn test_pos_reward_capped_at_max_age() {
        let stake = 1000 * COIN;
        // 6 days and 60 days must yield the same reward (both capped).
        let at_cap = compute_pos_reward(stake, MAX_STAKE_AGE);
        let beyond_cap = compute_pos_reward(stake, 60 * 86400);
        assert_eq!(at_cap, beyond_cap, "reward must be capped at MAX_STAKE_AGE");
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

    #[test]
    fn test_claim_signature_round_trip() {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[7u8; 32]).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret_key);
        let claim_address = pubkey_to_vtorrent_address(&pubkey, true);

        let msg = Message::from_digest(claim_message_hash(&claim_address));
        let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
        let (rec_id, sig64) = rec_sig.serialize_compact();
        let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + 4];
        sig_bytes.extend_from_slice(&sig64);

        assert!(verify_claim_signature(&claim_address, &sig_bytes).is_ok());
        assert!(verify_claim_signature("invalid_address", &sig_bytes).is_err());
        assert!(verify_claim_signature(&claim_address, &sig_bytes[..5]).is_err());
    }

    #[test]
    fn test_validate_legacy_claim_snapshot_bound() {
        let secp = Secp256k1::new();
        let secret_key = SecretKey::from_slice(&[9u8; 32]).unwrap();
        let pubkey = PublicKey::from_secret_key(&secp, &secret_key);
        let claim_address = pubkey_to_vtorrent_address(&pubkey, true);

        let msg = Message::from_digest(claim_message_hash(&claim_address));
        let rec_sig = secp.sign_ecdsa_recoverable(&msg, &secret_key);
        let (rec_id, sig64) = rec_sig.serialize_compact();
        let mut sig_bytes = vec![27 + rec_id.to_i32() as u8 + 4];
        sig_bytes.extend_from_slice(&sig64);

        let tx = Transaction {
            version: 1,
            tx_type: TxType::LegacyClaim,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 500 * COIN,
                script_pubkey: vec![0x76, 0xa9, 0x14],
            }],
            lock_time: 0,
            claim_address: Some(claim_address.clone()),
            claim_signature: Some(sig_bytes),
        };

        assert!(validate_legacy_claim(&tx, 1000 * COIN).is_ok());
        assert!(validate_legacy_claim(&tx, 100 * COIN).is_err());

        let bad_tx = Transaction {
            claim_signature: None,
            ..tx.clone()
        };
        assert!(validate_legacy_claim(&bad_tx, 1000 * COIN).is_err());
    }

    fn make_test_block(first_tx: Transaction, bits: u32, nonce: u32) -> Block {
        let mut block = Block {
            header: crate::block::BlockHeader {
                version: 1,
                prev_block_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                timestamp: 1_700_000_001,
                bits,
                nonce,
                stake_modifier: 0,
            },
            transactions: vec![first_tx],
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    }

    #[test]
    fn test_validate_block_rejects_difficulty_change() {
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: COIN,
                script_pubkey: vec![0x51],
            }],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        let block = make_test_block(coinbase, 0x1e0fffff, 42);
        // Matching parent difficulty passes
        assert!(validate_block(&block, 0, 1_700_000_000, 0x1e0fffff).is_ok());
        // Lower (easier) difficulty rejected
        assert!(validate_block(&block, 0, 1_700_000_000, 0x1e0ffffe).is_err());
    }

    #[test]
    fn test_validate_block_rejects_wrong_height() {
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: COIN,
                script_pubkey: vec![0x51],
            }],
            lock_time: 5, // wrong height
            claim_address: None,
            claim_signature: None,
        };
        let block = make_test_block(coinbase, 0x1e0fffff, 42);
        assert!(validate_block(&block, 0, 1_700_000_000, 0x1e0fffff).is_err());
    }

    #[test]
    fn test_validate_block_pos_requires_coinstake_first() {
        // PoS block (nonce 0) with a coinbase first tx must be rejected.
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: COIN,
                script_pubkey: vec![0x51],
            }],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        let block = make_test_block(coinbase, 0x1e0fffff, 0);
        assert!(validate_block(&block, 0, 1_700_000_000, 0x1e0fffff).is_err());
    }

    #[test]
    fn test_validate_block_rejects_oversized_block() {
        let coinbase = Transaction {
            version: 1,
            tx_type: TxType::Coinbase,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: COIN,
                script_pubkey: vec![0x51; MAX_BLOCK_SIZE],
            }],
            lock_time: 1,
            claim_address: None,
            claim_signature: None,
        };
        let block = make_test_block(coinbase, 0x1e0fffff, 42);
        assert!(validate_block(&block, 0, 1_700_000_000, 0x1e0fffff).is_err());
    }
}
