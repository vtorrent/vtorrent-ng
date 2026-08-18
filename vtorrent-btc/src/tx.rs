//! Bitcoin transaction building, signing, and serialization.

use crate::error::{BtcError, Result};
use crate::utxo::Utxo;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::sighash::SighashCache;
use bitcoin::transaction::Version;
use bitcoin::{Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use std::str::FromStr;

/// Build and sign a P2WPKH transaction spending `inputs` to `destination`,
/// returning the change to `change_address`.
pub fn build_and_sign(
    inputs: &[Utxo],
    destination: &str,
    amount_sats: u64,
    fee_sats: u64,
    change_address: &str,
    wif: &str,
) -> Result<Vec<u8>> {
    let secp = Secp256k1::new();
    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    let dest = Address::from_str(destination)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(bitcoin::Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let change = Address::from_str(change_address)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(bitcoin::Network::Bitcoin)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let total_in: u64 = inputs.iter().map(|u| u.value).sum();
    let change_sats = total_in
        .checked_sub(amount_sats)
        .and_then(|v| v.checked_sub(fee_sats))
        .ok_or(BtcError::InsufficientFunds {
            available: total_in,
            required: amount_sats + fee_sats,
        })?;

    let mut tx_inputs = Vec::with_capacity(inputs.len());
    for u in inputs {
        let txid =
            bitcoin::Txid::from_str(&u.txid).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
        tx_inputs.push(TxIn {
            previous_output: OutPoint { txid, vout: u.vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        });
    }

    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: tx_inputs,
        output: vec![
            TxOut {
                value: Amount::from_sat(amount_sats),
                script_pubkey: dest.script_pubkey(),
            },
            TxOut {
                value: Amount::from_sat(change_sats),
                script_pubkey: change.script_pubkey(),
            },
        ],
    };

    let pubkey = key.public_key(&secp);
    let pubkey_bytes = pubkey.to_bytes();

    // Sign each input (P2WPKH), collecting witnesses before mutating the tx.
    let mut witnesses = Vec::with_capacity(inputs.len());
    {
        let mut cache = SighashCache::new(&tx);
        for (i, u) in inputs.iter().enumerate() {
            let sighash = cache
                .p2wpkh_signature_hash(
                    i,
                    &dest.script_pubkey(),
                    Amount::from_sat(u.value),
                    bitcoin::EcdsaSighashType::All,
                )
                .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
            let msg: bitcoin::secp256k1::Message = sighash.into();
            let sig = secp.sign_ecdsa(&msg, &key.inner);
            let mut sig_bytes = sig.serialize_der().to_vec();
            sig_bytes.push(bitcoin::EcdsaSighashType::All as u8);
            witnesses.push(Witness::from_slice(&[sig_bytes, pubkey_bytes.clone()]));
        }
    }
    for (i, witness) in witnesses.into_iter().enumerate() {
        tx.input[i].witness = witness;
    }

    Ok(serialize(&tx))
}

/// Compute the txid (double-SHA256 of the serialized tx) as bytes.
pub fn txid_of(raw: &[u8]) -> [u8; 32] {
    use bitcoin::hashes::{sha256d, Hash};
    sha256d::Hash::hash(raw).to_byte_array()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_sign_serializes() {
        let wif = crate::keys::derive_wif(&[7u8; 64], 0).unwrap();
        let inputs = vec![Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 100_000,
            address: "bc1qtest".to_string(),
            height: 100,
        }];
        let raw = build_and_sign(
            &inputs,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            50_000,
            1_000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            &wif,
        )
        .unwrap();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_insufficient_funds() {
        let wif = crate::keys::derive_wif(&[7u8; 64], 0).unwrap();
        let inputs = vec![Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 10_000,
            address: "bc1qtest".to_string(),
            height: 100,
        }];
        let result = build_and_sign(
            &inputs,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            50_000,
            1_000,
            "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
            &wif,
        );
        assert!(result.is_err());
    }
}
