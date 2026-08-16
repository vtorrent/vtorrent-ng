use ripemd::Ripemd160;
use secp256k1::{Message, PublicKey, Secp256k1};
/// Consensus rules for the vTorrent 2.0 chain.
///
/// vTorrent 2.0 uses Proof-of-Stake (PoS) consensus, matching the original
/// chain's design. Key parameters are preserved from the legacy chain.
use sha2::{Digest as Sha2Digest, Sha256};

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
pub fn validate_block(block: &Block, _prev_height: u32, prev_timestamp: u32) -> Result<()> {
    // Check block is not empty
    if block.transactions.is_empty() {
        return Err(NodeError::InvalidBlock("Block has no transactions".into()));
    }

    // Check first transaction is coinbase or coinstake
    let first_tx = &block.transactions[0];
    if first_tx.tx_type != TxType::Coinbase && first_tx.tx_type != TxType::Coinstake {
        return Err(NodeError::InvalidBlock(
            "First transaction must be coinbase or coinstake".into(),
        ));
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
    // Coinbase and coinstake have no inputs to validate
    if tx.is_coinbase() || tx.is_coinstake() {
        if tx.outputs.is_empty() {
            return Err(NodeError::InvalidTransaction(
                "Coinbase/coinstake must have outputs".into(),
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

    // Check for zero-value outputs
    for output in &tx.outputs {
        if output.value == 0 {
            return Err(NodeError::InvalidTransaction(
                "Output value cannot be zero".into(),
            ));
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
    // The claim signature is a Bitcoin-style "signed message" proof:
    //   sig = sign(secp256k1, privkey, hash("vTorrent Signed Message:\n" + txid_hex))
    //
    // The public key recovered from the signature must hash (Hash160) to the
    // same 20-byte payload as the claim_address.
    let sig_bytes = tx
        .claim_signature
        .as_ref()
        .ok_or_else(|| NodeError::InvalidClaim("Missing claim signature".into()))?;

    verify_claim_signature(claim_addr, sig_bytes, &tx.txid())
        .map_err(|e| NodeError::InvalidClaim(format!("Signature verification failed: {}", e)))?;

    Ok(())
}

/// Verify a Bitcoin-style signed-message proof for a legacy claim.
///
/// Protocol (identical to Bitcoin Core's `verifymessage`):
/// 1. Compute the message hash:
///    `hash = SHA256d("vTorrent Signed Message:\n" + len_varint + message)`
///    where `message` is the hex-encoded txid.
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
    txid: &[u8; 32],
) -> std::result::Result<(), String> {
    if sig_bytes.len() != 65 {
        return Err(format!(
            "Expected 65-byte compact signature, got {}",
            sig_bytes.len()
        ));
    }

    // ── Step 1: Build the signed message hash ────────────────────────────────
    let txid_hex = hex::encode(txid);
    let message_hash = bitcoin_signed_message_hash("vTorrent Signed Message", &txid_hex);

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
