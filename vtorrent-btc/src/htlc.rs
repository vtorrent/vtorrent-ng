//! Bitcoin-side HTLC (P2WSH) for cross-chain atomic swaps.

use crate::error::{BtcError, Result};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::opcodes::all::{
    OP_CHECKSIG, OP_CLTV, OP_DROP, OP_DUP, OP_ELSE, OP_ENDIF, OP_EQUALVERIFY, OP_HASH160, OP_IF,
    OP_SHA256,
};
use bitcoin::script::Builder;
use bitcoin::transaction::Version;
use bitcoin::{Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use std::str::FromStr;

/// Default HTLC locktime: 48 hours in seconds.
pub const DEFAULT_HTLC_LOCKTIME: u32 = 48 * 3600;

/// A Bitcoin-side HTLC.
#[derive(Debug, Clone)]
pub struct BtcHtlc {
    pub hash_lock: [u8; 32],
    pub recipient: String,
    pub refund_address: String,
    pub expiry: u32,
    pub amount: u64,
}

/// Extract the 20-byte hash160 from a P2PKH or P2WPKH address.
fn address_hash160(address: &str) -> Result<[u8; 20]> {
    let addr = Address::from_str(address)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(bitcoin::Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;
    if let Some(h) = addr.pubkey_hash() {
        return Ok(h.to_byte_array());
    }
    if let Some(wp) = addr.witness_program() {
        if wp.version().to_num() == 0 && wp.program().len() == 20 {
            let mut out = [0u8; 20];
            out.copy_from_slice(wp.program().as_bytes());
            return Ok(out);
        }
    }
    Err(BtcError::InvalidAddress(
        "address is not P2PKH/P2WPKH".into(),
    ))
}

impl BtcHtlc {
    pub fn new(
        hash_lock: [u8; 32],
        recipient: String,
        refund_address: String,
        locktime_seconds: u32,
        amount: u64,
    ) -> Result<Self> {
        if amount == 0 {
            return Err(BtcError::Bitcoin("HTLC amount cannot be zero".into()));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        Ok(Self {
            hash_lock,
            recipient,
            refund_address,
            expiry: now + locktime_seconds,
            amount,
        })
    }

    /// Build the P2WSH witness script.
    pub fn build_script(&self) -> Result<ScriptBuf> {
        let recipient_hash = address_hash160(&self.recipient)?;
        let refund_hash = address_hash160(&self.refund_address)?;

        Ok(Builder::new()
            .push_opcode(OP_IF)
            .push_opcode(OP_SHA256)
            .push_slice(self.hash_lock)
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_DUP)
            .push_opcode(OP_HASH160)
            .push_slice(recipient_hash)
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ELSE)
            .push_int(self.expiry as i64)
            .push_opcode(OP_CLTV)
            .push_opcode(OP_DROP)
            .push_opcode(OP_DUP)
            .push_opcode(OP_HASH160)
            .push_slice(refund_hash)
            .push_opcode(OP_EQUALVERIFY)
            .push_opcode(OP_CHECKSIG)
            .push_opcode(OP_ENDIF)
            .into_script())
    }

    /// The P2WSH address for the funding output.
    pub fn address(&self) -> Result<String> {
        let script = self.build_script()?;
        let addr = Address::p2wsh(&script, bitcoin::Network::Bitcoin);
        Ok(addr.to_string())
    }

    /// Build the funding transaction (single input, P2WSH output + change).
    pub fn build_funding_tx(
        &self,
        input_txid: [u8; 32],
        input_vout: u32,
        input_value: u64,
        fee: u64,
        change_address: &str,
    ) -> Result<Transaction> {
        if input_value < self.amount + fee {
            return Err(BtcError::InsufficientFunds {
                available: input_value,
                required: self.amount + fee,
            });
        }
        let change = Address::from_str(change_address)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        let mut outputs = vec![TxOut {
            value: Amount::from_sat(self.amount),
            script_pubkey: self.build_script()?.to_p2wsh(),
        }];
        let change_sats = input_value - self.amount - fee;
        if change_sats > 0 {
            outputs.push(TxOut {
                value: Amount::from_sat(change_sats),
                script_pubkey: change.script_pubkey(),
            });
        }

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(input_txid),
                    vout: input_vout,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: outputs,
        })
    }

    /// Build the claim transaction (reveals the preimage).
    pub fn build_claim_tx(
        &self,
        funding_txid: [u8; 32],
        preimage: &[u8; 32],
        fee: u64,
    ) -> Result<Transaction> {
        let hash: [u8; 32] = sha256::Hash::hash(preimage).to_byte_array();
        if hash != self.hash_lock {
            return Err(BtcError::Bitcoin(
                "preimage does not match hash lock".into(),
            ));
        }
        let recipient = Address::from_str(&self.recipient)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(funding_txid),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(self.amount.saturating_sub(fee)),
                script_pubkey: recipient.script_pubkey(),
            }],
        })
    }

    /// Build the refund transaction (after expiry).
    pub fn build_refund_tx(&self, funding_txid: [u8; 32], fee: u64) -> Result<Transaction> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;
        if now < self.expiry {
            return Err(BtcError::Bitcoin("HTLC has not expired yet".into()));
        }
        let refund = Address::from_str(&self.refund_address)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(bitcoin::Network::Bitcoin)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

        Ok(Transaction {
            version: Version::TWO,
            lock_time: LockTime::from_consensus(self.expiry),
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(funding_txid),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                // BIP-65 rule 4: OP_CLTV fails when the input's nSequence is
                // MAX (0xffffffff). Use a non-final sequence so the CLTV
                // branch can be satisfied.
                sequence: Sequence::ENABLE_LOCKTIME_NO_RBF,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(self.amount.saturating_sub(fee)),
                script_pubkey: refund.script_pubkey(),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh";

    fn make_htlc() -> BtcHtlc {
        let preimage = [42u8; 32];
        let hash_lock: [u8; 32] = sha256::Hash::hash(&preimage).to_byte_array();
        BtcHtlc::new(
            hash_lock,
            ADDR.to_string(),
            ADDR.to_string(),
            DEFAULT_HTLC_LOCKTIME,
            100_000,
        )
        .unwrap()
    }

    #[test]
    fn test_script_structure() {
        let htlc = make_htlc();
        let script = htlc.build_script().unwrap();
        assert!(!script.is_empty());
        assert_eq!(script.as_bytes()[0], OP_IF.to_u8());
        assert_eq!(*script.as_bytes().last().unwrap(), OP_ENDIF.to_u8());
    }

    #[test]
    fn test_script_contains_hash_lock() {
        let htlc = make_htlc();
        let script = htlc.build_script().unwrap();
        let script_hex = hex::encode(script.as_bytes());
        let hash_hex = hex::encode(htlc.hash_lock);
        assert!(script_hex.contains(&hash_hex));
    }

    #[test]
    fn test_address_is_bech32() {
        let htlc = make_htlc();
        let addr = htlc.address().unwrap();
        assert!(addr.starts_with("bc1q"), "got {}", addr);
    }

    #[test]
    fn test_wrong_preimage_rejected() {
        let htlc = make_htlc();
        let result = htlc.build_claim_tx([0u8; 32], &[99u8; 32], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_refund_before_expiry_rejected() {
        let htlc = make_htlc();
        let result = htlc.build_refund_tx([0u8; 32], 1000);
        assert!(result.is_err());
    }

    #[test]
    fn test_funding_tx_insufficient() {
        let htlc = make_htlc();
        let result = htlc.build_funding_tx([0u8; 32], 0, 50_000, 1000, ADDR);
        assert!(result.is_err());
    }

    #[test]
    fn test_funding_tx_valid() {
        let htlc = make_htlc();
        let tx = htlc
            .build_funding_tx([1u8; 32], 0, 200_000, 10_000, ADDR)
            .unwrap();
        assert_eq!(tx.output[0].value, Amount::from_sat(100_000));
        assert_eq!(tx.output[1].value, Amount::from_sat(90_000));
    }
}
