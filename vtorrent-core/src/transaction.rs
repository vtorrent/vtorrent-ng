use crate::utxo::OutPoint;
use serde::{Deserialize, Serialize};

/// A transaction input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxIn {
    pub previous_output: OutPoint,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

/// A transaction output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxOut {
    /// Value in satoshis.
    pub value: u64,
    /// The locking script (scriptPubKey).
    pub script_pubkey: Vec<u8>,
}

/// A transaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub version: i32,
    pub inputs: Vec<TxIn>,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
    /// Optional: timestamp for PoS transactions.
    pub time: Option<u32>,
}

impl Transaction {
    /// Create a new claim transaction.
    /// A claim transaction has no inputs (it's a special genesis-spend)
    /// and one output to the new address.
    pub fn new_claim(
        legacy_address: String,
        new_address_script: Vec<u8>,
        amount: u64,
        signature_proof: Vec<u8>,
    ) -> Self {
        // The "input" for a claim tx references a special claim outpoint
        // with the legacy address and signature proof in the scriptSig.
        let claim_input = TxIn {
            previous_output: OutPoint {
                txid: [0u8; 32], // Null txid signals a claim transaction
                vout: u32::MAX,  // Sentinel value
            },
            script_sig: {
                let mut sig = Vec::new();
                // Encode: [legacy_address_len][legacy_address][signature_proof]
                let addr_bytes = legacy_address.as_bytes();
                let addr_len =
                    u8::try_from(addr_bytes.len()).expect("legacy address exceeds 255 bytes");
                sig.push(addr_len);
                sig.extend_from_slice(addr_bytes);
                sig.extend_from_slice(&signature_proof);
                sig
            },
            sequence: u32::MAX,
        };

        Self {
            version: 2,
            inputs: vec![claim_input],
            outputs: vec![TxOut {
                value: amount,
                script_pubkey: new_address_script,
            }],
            lock_time: 0,
            time: None,
        }
    }
}
