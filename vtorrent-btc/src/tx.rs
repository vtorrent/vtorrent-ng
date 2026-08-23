//! Bitcoin transaction building, signing, and serialization.
//!
//! Supports BIP69 lexicographic ordering, optional RBF signaling, and
//! per-input key lookup for multi-index wallets.

use crate::error::{BtcError, Result};
use crate::utxo::Utxo;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode::serialize;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::sighash::SighashCache;
use bitcoin::transaction::Version;
use bitcoin::{Address, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use std::str::FromStr;

/// BIP69: sequence value that signals RBF (Replace-By-Fee).
const RBF_SEQUENCE: Sequence = Sequence(0xFFFFFFFD);

/// Build and sign a P2WPKH transaction spending `inputs` to `destination`,
/// returning the change to `change_address`.
///
/// When `rbf` is true, inputs use `Sequence(0xFFFFFFFD)` to signal
/// replaceability.  Inputs and outputs are sorted per BIP69.
#[allow(clippy::too_many_arguments)]
pub fn build_and_sign(
    inputs: &[Utxo],
    destination: &str,
    amount_sats: u64,
    fee_sats: u64,
    change_address: &str,
    wif: &str,
    network: bitcoin::Network,
    rbf: bool,
) -> Result<Vec<u8>> {
    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let pubkey = {
        let secp = Secp256k1::new();
        key.public_key(&secp)
    };

    // Single-key lookup: all inputs must belong to the same address.
    let key_fn = |address: &str| -> Result<bitcoin::PrivateKey> {
        if address_matches_wif(address, wif, network)? {
            Ok(key)
        } else {
            Err(BtcError::InvalidAddress(format!(
                "UTXO address {} does not match the provided key",
                address
            )))
        }
    };

    build_and_sign_multi(
        inputs,
        destination,
        amount_sats,
        fee_sats,
        change_address,
        &key_fn,
        &pubkey,
        network,
        rbf,
    )
}

/// Build and sign a P2WPKH transaction with per-input key lookup.
///
/// `key_for_address` maps each input's address to the signing private key.
/// Each input's witness carries the compressed pubkey derived from its own
/// signing key (a mismatched pubkey makes the input invalid).
/// `common_pubkey` is accepted for API compatibility but unused.
///
/// Inputs and outputs are sorted per BIP69.  When `rbf` is true, inputs
/// use `Sequence(0xFFFFFFFD)`.
#[allow(clippy::too_many_arguments)]
pub fn build_and_sign_multi(
    inputs: &[Utxo],
    destination: &str,
    amount_sats: u64,
    fee_sats: u64,
    change_address: &str,
    key_for_address: &dyn Fn(&str) -> Result<bitcoin::PrivateKey>,
    _common_pubkey: &bitcoin::PublicKey,
    network: bitcoin::Network,
    rbf: bool,
) -> Result<Vec<u8>> {
    let secp = Secp256k1::new();

    let dest = Address::from_str(destination)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(network)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let change = Address::from_str(change_address)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
        .require_network(network)
        .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;

    let total_in: u64 = inputs.iter().map(|u| u.value).sum();
    let change_sats = total_in
        .checked_sub(amount_sats)
        .and_then(|v| v.checked_sub(fee_sats))
        .ok_or(BtcError::InsufficientFunds {
            available: total_in,
            required: amount_sats + fee_sats,
        })?;

    let sequence = if rbf { RBF_SEQUENCE } else { Sequence::MAX };

    // ── BIP69: sort inputs lexicographically by (txid, vout) ────────────
    let mut indexed_inputs: Vec<(usize, &Utxo)> = inputs.iter().enumerate().collect();
    indexed_inputs.sort_by(|a, b| a.1.txid.cmp(&b.1.txid).then(a.1.vout.cmp(&b.1.vout)));

    let tx_inputs: Vec<TxIn> = indexed_inputs
        .iter()
        .map(|(_, u)| {
            let txid =
                bitcoin::Txid::from_str(&u.txid).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
            Ok(TxIn {
                previous_output: OutPoint { txid, vout: u.vout },
                script_sig: ScriptBuf::new(),
                sequence,
                witness: Witness::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // ── Build outputs (before BIP69 sort) ───────────────────────────────
    let mut outputs = vec![TxOut {
        value: Amount::from_sat(amount_sats),
        script_pubkey: dest.script_pubkey(),
    }];
    if change_sats > 0 {
        outputs.push(TxOut {
            value: Amount::from_sat(change_sats),
            script_pubkey: change.script_pubkey(),
        });
    }

    // ── BIP69: sort outputs lexicographically by (value, script_pubkey) ─
    outputs.sort_by(|a, b| {
        a.value
            .cmp(&b.value)
            .then(a.script_pubkey.as_bytes().cmp(b.script_pubkey.as_bytes()))
    });

    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: tx_inputs,
        output: outputs,
    };

    // ── Sign each input ─────────────────────────────────────────────────
    // Build a map from original-index → key, then sign in BIP69 order.
    let mut witnesses: Vec<Witness> = Vec::with_capacity(tx.input.len());
    {
        let mut cache = SighashCache::new(&tx);
        for (tx_idx, (_, u)) in indexed_inputs.iter().enumerate() {
            let input_script = Address::from_str(&u.address)
                .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
                .require_network(network)
                .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
                .script_pubkey();
            let sighash = cache
                .p2wpkh_signature_hash(
                    tx_idx,
                    &input_script,
                    Amount::from_sat(u.value),
                    bitcoin::EcdsaSighashType::All,
                )
                .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
            let msg: bitcoin::secp256k1::Message = sighash.into();
            let key = key_for_address(&u.address)?;
            let sig = secp.sign_ecdsa(&msg, &key.inner);
            let mut sig_bytes = sig.serialize_der().to_vec();
            sig_bytes.push(bitcoin::EcdsaSighashType::All as u8);
            // The witness must carry the pubkey matching THIS input's signing
            // key — using a shared/common pubkey here makes any input signed
            // by a different key invalid (bad-witness-nonstandard).
            let input_pubkey = key.inner.public_key(&secp).serialize().to_vec();
            witnesses.push(Witness::from_slice(&[sig_bytes, input_pubkey]));
        }
    }
    for (i, witness) in witnesses.into_iter().enumerate() {
        tx.input[i].witness = witness;
    }

    Ok(serialize(&tx))
}

/// Check whether `address` matches the given WIF key on `network`.
fn address_matches_wif(address: &str, wif: &str, network: bitcoin::Network) -> Result<bool> {
    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let secp = Secp256k1::new();
    let pubkey = key.public_key(&secp);
    let compressed = bitcoin::CompressedPublicKey::from_slice(&pubkey.to_bytes())
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let derived = Address::p2wpkh(&compressed, network);
    Ok(address == derived.to_string())
}

/// Compute the txid (double-SHA256 of the serialized tx) as bytes.
pub fn txid_of(raw: &[u8]) -> [u8; 32] {
    use bitcoin::hashes::{sha256d, Hash};
    sha256d::Hash::hash(raw).to_byte_array()
}

/// Estimate the virtual size (vsize) of a P2WPKH transaction in bytes.
///
/// P2WPKH input weight: 4×41 (non-witness) + 108 (witness) = 272 WU → 68 vB.
/// P2WPKH output weight: 4×31 = 124 WU → 31 vB.
/// Overhead: version(4) + locktime(4) + vin_count(1) + vout_count(1) +
///          witness_marker(1) + witness_len(1) = 12 bytes → 48 WU → 12 vB.
pub fn estimate_vsize(input_count: usize, output_count: usize) -> u64 {
    let input_vsize = input_count as u64 * 68;
    let output_vsize = output_count as u64 * 31;
    let overhead = 12;
    input_vsize + output_vsize + overhead
}

// ─── PSBT (BIP174) ───────────────────────────────────────────────────────────

/// Create an unsigned PSBT (Partially Signed Bitcoin Transaction) from the
/// given UTXOs and outputs.  Returns the serialized PSBT bytes.
///
/// The PSBT can later be signed with `sign_psbt` and finalized with
/// `finalize_psbt` to produce a broadcastable raw transaction.
pub fn create_psbt(
    inputs: &[Utxo],
    outputs: &[(u64, Address)],
    network: bitcoin::Network,
    rbf: bool,
) -> Result<Vec<u8>> {
    use bitcoin::psbt::Psbt;

    let sequence = if rbf { RBF_SEQUENCE } else { Sequence::MAX };

    // BIP69 sort inputs
    let mut indexed: Vec<(usize, &Utxo)> = inputs.iter().enumerate().collect();
    indexed.sort_by(|a, b| a.1.txid.cmp(&b.1.txid).then(a.1.vout.cmp(&b.1.vout)));

    let tx_inputs: Vec<TxIn> = indexed
        .iter()
        .map(|(_, u)| {
            let txid =
                bitcoin::Txid::from_str(&u.txid).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
            Ok(TxIn {
                previous_output: OutPoint { txid, vout: u.vout },
                script_sig: ScriptBuf::new(),
                sequence,
                witness: Witness::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    // BIP69 sort outputs
    let mut sorted_outputs: Vec<(u64, &Address)> = outputs.iter().map(|(v, a)| (*v, a)).collect();
    sorted_outputs.sort_by(|a, b| {
        a.0.cmp(&b.0).then(
            a.1.script_pubkey()
                .as_bytes()
                .cmp(b.1.script_pubkey().as_bytes()),
        )
    });

    let tx_outputs: Vec<TxOut> = sorted_outputs
        .iter()
        .map(|(value, addr)| TxOut {
            value: Amount::from_sat(*value),
            script_pubkey: addr.script_pubkey(),
        })
        .collect();

    let unsigned_tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: tx_inputs,
        output: tx_outputs,
    };

    let mut psbt =
        Psbt::from_unsigned_tx(unsigned_tx).map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    // Populate per-input witness_utxo for signers.
    for (i, (_, u)) in indexed.iter().enumerate() {
        let input_addr = Address::from_str(&u.address)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?
            .require_network(network)
            .map_err(|e| BtcError::InvalidAddress(e.to_string()))?;
        let input = bitcoin::psbt::Input {
            witness_utxo: Some(bitcoin::TxOut {
                value: Amount::from_sat(u.value),
                script_pubkey: input_addr.script_pubkey(),
            }),
            ..Default::default()
        };
        psbt.inputs.insert(i, input);
    }

    Ok(psbt.serialize())
}

/// Sign a PSBT with the given WIF private key for all matching P2WPKH inputs.
/// Returns the updated PSBT bytes.
pub fn sign_psbt(psbt_bytes: &[u8], wif: &str, network: bitcoin::Network) -> Result<Vec<u8>> {
    use bitcoin::psbt::Psbt;

    let mut psbt = Psbt::deserialize(psbt_bytes).map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let secp = Secp256k1::new();
    let pubkey = key.public_key(&secp);
    let compressed = bitcoin::CompressedPublicKey::from_slice(&pubkey.to_bytes())
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let our_script = Address::p2wpkh(&compressed, network).script_pubkey();

    for (i, input) in psbt.inputs.iter_mut().enumerate() {
        if let Some(ref utxo) = input.witness_utxo {
            if utxo.script_pubkey == our_script {
                let mut sighash_cache = bitcoin::sighash::SighashCache::new(&psbt.unsigned_tx);
                let sighash = sighash_cache
                    .p2wpkh_signature_hash(
                        i,
                        &utxo.script_pubkey,
                        utxo.value,
                        bitcoin::EcdsaSighashType::All,
                    )
                    .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
                let msg = bitcoin::secp256k1::Message::from(sighash);
                let sig = secp.sign_ecdsa(&msg, &key.inner);
                input.partial_sigs.insert(
                    pubkey,
                    bitcoin::ecdsa::Signature {
                        signature: sig,
                        sighash_type: bitcoin::EcdsaSighashType::All,
                    },
                );
            }
        }
    }

    Ok(psbt.serialize())
}

/// Finalize a signed PSBT, extracting the raw transaction bytes.
///
/// For each P2WPKH input with exactly one partial signature, builds the final
/// scriptWitness (`<sig> <pubkey>`). `Psbt::extract_tx` alone does NOT convert
/// partial signatures into witnesses — skipping this step yields a transaction
/// with empty witnesses that every node rejects.
pub fn finalize_psbt(psbt_bytes: &[u8]) -> Result<Vec<u8>> {
    use bitcoin::psbt::Psbt;

    let mut psbt = Psbt::deserialize(psbt_bytes).map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    for input in psbt.inputs.iter_mut() {
        if input.final_script_witness.is_some() || input.final_script_sig.is_some() {
            continue; // already finalized
        }
        if input.partial_sigs.len() == 1 {
            let (pubkey, sig) = input.partial_sigs.iter().next().unwrap();
            let mut sig_bytes = sig.signature.serialize_der().to_vec();
            sig_bytes.push(sig.sighash_type as u8);
            let witness = Witness::from_slice(&[sig_bytes, pubkey.to_bytes()]);
            input.final_script_witness = Some(witness);
        }
    }

    let tx = psbt
        .extract_tx()
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;

    Ok(bitcoin::consensus::encode::serialize(&tx))
}

// ─── Taproot / P2TR ──────────────────────────────────────────────────────────

/// Create a P2TR (Taproot) address from a public key.
pub fn p2tr_address(pubkey: &bitcoin::PublicKey, network: bitcoin::Network) -> Result<Address> {
    let secp = Secp256k1::new();
    let (xonly, _parity) = pubkey.inner.x_only_public_key();
    Ok(Address::p2tr(&secp, xonly, None, network))
}

/// Create a P2TR address from a WIF private key.
pub fn p2tr_address_from_wif(wif: &str, network: bitcoin::Network) -> Result<Address> {
    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let secp = Secp256k1::new();
    let pubkey = key.public_key(&secp);
    p2tr_address(&pubkey, network)
}

/// Sign a 32-byte message using Schnorr (BIP340) with the given WIF key.
pub fn schnorr_sign(message: &[u8; 32], wif: &str) -> Result<[u8; 64]> {
    let key = bitcoin::PrivateKey::from_wif(wif).map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let secp = Secp256k1::new();
    let kp = key.inner.keypair(&secp);
    let msg = bitcoin::secp256k1::Message::from_digest_slice(message)
        .map_err(|e| BtcError::Bitcoin(e.to_string()))?;
    let sig = secp.sign_schnorr(&msg, &kp);
    Ok(sig.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SEED: [u8; 64] = [7u8; 64];

    fn test_addr(index: u32) -> String {
        crate::keys::derive_address(&TEST_SEED, index, bitcoin::Network::Bitcoin).unwrap()
    }

    fn test_utxo(txid: &str, vout: u32, value: u64, addr_index: u32) -> Utxo {
        Utxo {
            txid: txid.to_string(),
            vout,
            value,
            address: test_addr(addr_index),
            height: 100,
        }
    }

    fn test_wif(index: u32) -> String {
        crate::keys::derive_wif(&TEST_SEED, index, bitcoin::Network::Bitcoin).unwrap()
    }

    #[test]
    fn test_build_sign_serializes() {
        let wif = test_wif(0);
        let addr = test_addr(0);
        let inputs = vec![test_utxo(&"11".repeat(32), 0, 100_000, 0)];
        let raw = build_and_sign(
            &inputs,
            &addr,
            50_000,
            1_000,
            &addr,
            &wif,
            bitcoin::Network::Bitcoin,
            false,
        )
        .unwrap();
        assert!(!raw.is_empty());
    }

    #[test]
    fn test_build_sign_rbf() {
        let wif = test_wif(0);
        let addr = test_addr(0);
        let inputs = vec![test_utxo(&"22".repeat(32), 0, 100_000, 0)];
        let raw = build_and_sign(
            &inputs,
            &addr,
            50_000,
            1_000,
            &addr,
            &wif,
            bitcoin::Network::Bitcoin,
            true,
        )
        .unwrap();
        let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
        assert_eq!(tx.input[0].sequence, RBF_SEQUENCE);
    }

    #[test]
    fn test_insufficient_funds() {
        let wif = test_wif(0);
        let addr = test_addr(0);
        let inputs = vec![test_utxo(&"33".repeat(32), 0, 10_000, 0)];
        let result = build_and_sign(
            &inputs,
            &addr,
            50_000,
            1_000,
            &addr,
            &wif,
            bitcoin::Network::Bitcoin,
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_bip69_input_sorting() {
        let wif = test_wif(0);
        let addr = test_addr(0);
        let inputs = vec![
            test_utxo(&"bb".repeat(32), 0, 50_000, 0),
            test_utxo(&"aa".repeat(32), 0, 50_000, 0),
        ];
        let raw = build_and_sign(
            &inputs,
            &addr,
            50_000,
            1_000,
            &addr,
            &wif,
            bitcoin::Network::Bitcoin,
            false,
        )
        .unwrap();
        let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
        assert!(
            tx.input[0].previous_output.txid.to_string()
                < tx.input[1].previous_output.txid.to_string()
        );
    }

    #[test]
    fn test_bip69_output_sorting() {
        let wif = test_wif(0);
        let addr = test_addr(0);
        let inputs = vec![test_utxo(&"cc".repeat(32), 0, 200_000, 0)];
        let raw = build_and_sign(
            &inputs,
            &addr,
            50_000,
            1_000,
            &addr,
            &wif,
            bitcoin::Network::Bitcoin,
            false,
        )
        .unwrap();
        let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
        assert!(tx.output[0].value <= tx.output[1].value);
    }

    #[test]
    fn test_estimate_vsize() {
        let v = estimate_vsize(1, 2);
        assert!(v > 0);
        assert!((100..=200).contains(&v));
    }

    #[test]
    fn test_multi_index_signing() {
        let wif0 = test_wif(0);
        let wif1 = test_wif(1);
        let addr0 = test_addr(0);
        let addr1 = test_addr(1);
        let dest = test_addr(2);

        let inputs = vec![
            test_utxo(&"aa".repeat(32), 0, 50_000, 0),
            test_utxo(&"bb".repeat(32), 0, 50_000, 1),
        ];

        let key_for = |address: &str| -> Result<bitcoin::PrivateKey> {
            if *address == addr0 {
                Ok(bitcoin::PrivateKey::from_wif(&wif0).unwrap())
            } else if *address == addr1 {
                Ok(bitcoin::PrivateKey::from_wif(&wif1).unwrap())
            } else {
                Err(BtcError::InvalidAddress("no key".into()))
            }
        };

        let secp = Secp256k1::new();
        let key0 = bitcoin::PrivateKey::from_wif(&wif0).unwrap();
        let pubkey = key0.public_key(&secp);

        let raw = build_and_sign_multi(
            &inputs,
            &dest,
            80_000,
            1_000,
            &dest,
            &key_for,
            &pubkey,
            bitcoin::Network::Bitcoin,
            false,
        )
        .unwrap();
        assert!(!raw.is_empty());

        let tx: Transaction = bitcoin::consensus::encode::deserialize(&raw).unwrap();
        assert_eq!(tx.input.len(), 2);
    }
}
