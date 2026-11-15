//! BTC claim replacement lineage.
//!
//! The claim reveals the preimage, so replacements are append-only: each one
//! supersedes the previous and pays a strictly higher fee. Recovery must never
//! accept a divergent chain or a raw transaction that no longer matches the
//! latest replacement.

use crate::error::{RpcError, RpcResult};
use bitcoin::hashes::Hash;
use vtorrent_node::atomic_swap::{BtcClaimReplacement, SwapState};

pub(crate) const MAX_REPLACEMENTS: usize = crate::btc_claim_bump::MAX_BTC_CLAIM_REPLACEMENTS;

pub(crate) fn is_prefix(old: &[BtcClaimReplacement], new: &[BtcClaimReplacement]) -> bool {
    old.len() <= new.len()
        && old.iter().zip(new).all(|(a, b)| {
            a.replaces_txid == b.replaces_txid
                && a.txid == b.txid
                && a.total_fee_satoshis == b.total_fee_satoshis
                && a.approved_at == b.approved_at
                && a.raw == b.raw
        })
}

/// Validate the replacement chain and that `btc_claim_raw` is its latest step.
pub(crate) fn validate(swap: &SwapState) -> RpcResult<()> {
    let bad = || RpcError::BadRequest("Invalid BTC claim replacement lineage".into());
    if swap.btc_claim_replacements.is_empty() {
        return Ok(());
    }
    if swap.btc_claim_replacements.len() > MAX_REPLACEMENTS {
        return Err(bad());
    }
    let funding = bitcoin::Txid::from_byte_array(swap.btc_funding_txid.ok_or_else(bad)?);
    let destination: bitcoin::Address<bitcoin::address::NetworkUnchecked> = swap
        .maker_btc_address
        .as_deref()
        .ok_or_else(bad)?
        .parse()
        .map_err(|_| bad())?;
    let destination_script = destination.assume_checked().script_pubkey();
    let recipient = swap.maker_btc_address.as_ref().ok_or_else(bad)?;
    let refund = swap.taker_btc_refund_address.as_ref().ok_or_else(bad)?;
    let htlc = [
        bitcoin::Network::Bitcoin,
        bitcoin::Network::Testnet,
        bitcoin::Network::Signet,
        bitcoin::Network::Regtest,
    ]
    .into_iter()
    .find_map(|network| {
        let htlc = vtorrent_btc::htlc::BtcHtlc {
            hash_lock: swap.hash_lock,
            recipient: recipient.clone(),
            refund_address: refund.clone(),
            expiry: swap.btc_expiry,
            amount: swap.btc_amount,
            network,
        };
        htlc.build_script().ok().map(|script| (htlc, script))
    })
    .ok_or_else(bad)?;
    let mut parent = swap.btc_claim_txid.ok_or_else(bad)?;
    let mut last_txid = None;
    let mut previous_fee = None;
    for replacement in &swap.btc_claim_replacements {
        if replacement.replaces_txid != parent {
            return Err(bad());
        }
        let tx: bitcoin::Transaction =
            bitcoin::consensus::deserialize(&replacement.raw).map_err(|_| bad())?;
        let txid = tx.compute_txid().to_byte_array();
        if txid != replacement.txid
            || tx.input.len() != 1
            || tx.input[0].previous_output.txid != funding
            || tx.input[0].previous_output.vout != 0
            || tx.output.len() != 1
            || tx.output[0].script_pubkey != destination_script
            || swap.btc_amount.checked_sub(tx.output[0].value.to_sat())
                != Some(replacement.total_fee_satoshis)
        {
            return Err(bad());
        }
        if previous_fee.is_some_and(|fee| replacement.total_fee_satoshis <= fee) {
            return Err(bad());
        }
        let witness = &tx.input[0].witness;
        if witness.len() != 5
            || witness.nth(3) != Some(&[1][..])
            || witness.nth(4) != Some(htlc.1.as_bytes())
            || witness.nth(2).is_none_or(|preimage| {
                preimage.len() != 32
                    || bitcoin::hashes::sha256::Hash::hash(preimage).to_byte_array()
                        != swap.hash_lock
                    || swap
                        .preimage
                        .is_some_and(|known| known.as_slice() != preimage)
            })
        {
            return Err(bad());
        }
        let public =
            bitcoin::PublicKey::from_slice(witness.nth(1).ok_or_else(bad)?).map_err(|_| bad())?;
        let public_hash = public.pubkey_hash().to_byte_array();
        let recipient: bitcoin::Address<bitcoin::address::NetworkUnchecked> =
            htlc.0.recipient.parse().map_err(|_| bad())?;
        let recipient = recipient.assume_checked();
        let expected_hash = recipient
            .pubkey_hash()
            .map(|h| h.to_byte_array())
            .or_else(|| {
                recipient
                    .witness_program()
                    .and_then(|program| program.program().as_bytes().try_into().ok())
            })
            .ok_or_else(bad)?;
        if public_hash != expected_hash {
            return Err(bad());
        }
        let signature = bitcoin::ecdsa::Signature::from_slice(witness.nth(0).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        if signature.sighash_type != bitcoin::EcdsaSighashType::All {
            return Err(bad());
        }
        let sighash = bitcoin::sighash::SighashCache::new(&tx)
            .p2wsh_signature_hash(
                0,
                &htlc.1,
                bitcoin::Amount::from_sat(swap.btc_amount),
                signature.sighash_type,
            )
            .map_err(|_| bad())?;
        bitcoin::secp256k1::Secp256k1::verification_only()
            .verify_ecdsa(&sighash.into(), &signature.signature, &public.inner)
            .map_err(|_| bad())?;
        previous_fee = Some(replacement.total_fee_satoshis);
        last_txid = Some(txid);
        parent = txid;
    }
    let raw: bitcoin::Transaction =
        bitcoin::consensus::deserialize(swap.btc_claim_raw.as_ref().ok_or_else(bad)?)
            .map_err(|_| bad())?;
    if last_txid != Some(raw.compute_txid().to_byte_array()) {
        return Err(bad());
    }
    if swap.btc_claim_raw.as_ref() != swap.btc_claim_replacements.last().map(|r| &r.raw) {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Transaction;

    fn htlc() -> vtorrent_btc::htlc::BtcHtlc {
        vtorrent_btc::htlc::BtcHtlc {
            hash_lock: bitcoin::hashes::sha256::Hash::hash(&[42; 32]).to_byte_array(),
            recipient: vtorrent_btc::keys::derive_address(&[7; 64], 0, bitcoin::Network::Bitcoin)
                .unwrap(),
            refund_address: vtorrent_btc::keys::derive_address(
                &[8; 64],
                0,
                bitcoin::Network::Bitcoin,
            )
            .unwrap(),
            expiry: 1_900_000_000,
            amount: 100_000,
            network: bitcoin::Network::Bitcoin,
        }
    }

    fn claim(funding: [u8; 32], fee: u64) -> (Vec<u8>, [u8; 32]) {
        let htlc = htlc();
        let wif = vtorrent_btc::keys::derive_wif(&[7; 64], 0, bitcoin::Network::Bitcoin).unwrap();
        let tx = htlc
            .sign_claim_tx(
                htlc.build_claim_tx(funding, &[42; 32], fee).unwrap(),
                &[42; 32],
                &wif,
            )
            .unwrap();
        (
            bitcoin::consensus::encode::serialize(&tx),
            tx.compute_txid().to_byte_array(),
        )
    }

    fn state_with_chain() -> SwapState {
        let funding = [3u8; 32];
        let parent = [4u8; 32];
        let (raw1, txid1) = claim(funding, 2_000);
        let (raw2, txid2) = claim(funding, 3_000);
        let mut swap = SwapState::new([1u8; 32], [2u8; 32]);
        swap.btc_amount = 100_000;
        swap.hash_lock = htlc().hash_lock;
        swap.preimage = Some([42; 32]);
        swap.maker_btc_address = Some(htlc().recipient);
        swap.taker_btc_refund_address = Some(htlc().refund_address);
        swap.btc_expiry = htlc().expiry;
        swap.btc_funding_txid = Some(funding);
        swap.btc_claim_txid = Some(parent);
        swap.btc_claim_replacements = vec![
            BtcClaimReplacement {
                replaces_txid: parent,
                txid: txid1,
                total_fee_satoshis: 2_000,
                approved_at: 10,
                raw: raw1,
            },
            BtcClaimReplacement {
                replaces_txid: txid1,
                txid: txid2,
                total_fee_satoshis: 3_000,
                approved_at: 11,
                raw: raw2.clone(),
            },
        ];
        swap.btc_claim_raw = Some(raw2);
        swap
    }

    #[test]
    fn accepts_a_consistent_chain() {
        validate(&state_with_chain()).unwrap();
    }

    #[test]
    fn rejects_a_stale_raw_claim() {
        let mut swap = state_with_chain();
        let (raw1, _) = claim([3u8; 32], 2_000);
        swap.btc_claim_raw = Some(raw1);
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_latest_claim_with_different_witness() {
        let mut swap = state_with_chain();
        let original = swap.btc_claim_raw.as_ref().unwrap();
        let mut altered: Transaction = bitcoin::consensus::deserialize(original).unwrap();
        altered.input[0].witness.push([42u8]);
        assert_eq!(
            altered.compute_txid().to_byte_array(),
            swap.btc_claim_replacements.last().unwrap().txid
        );
        swap.btc_claim_raw = Some(bitcoin::consensus::serialize(&altered));
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_a_broken_parent_link() {
        let mut swap = state_with_chain();
        swap.btc_claim_replacements[1].replaces_txid = [9u8; 32];
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_replacement_whose_recorded_fee_differs_from_output() {
        let mut swap = state_with_chain();
        swap.btc_claim_replacements[1].total_fee_satoshis = 2_500;
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_replacement_paying_a_different_destination() {
        let mut swap = state_with_chain();
        let last = swap.btc_claim_replacements.last_mut().unwrap();
        let mut tx: Transaction = bitcoin::consensus::deserialize(&last.raw).unwrap();
        tx.output[0].script_pubkey = bitcoin::ScriptBuf::new();
        last.txid = tx.compute_txid().to_byte_array();
        last.raw = bitcoin::consensus::serialize(&tx);
        swap.btc_claim_raw = Some(last.raw.clone());
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_consistent_lineage_with_invalid_claim_signature() {
        let mut swap = state_with_chain();
        let last = swap.btc_claim_replacements.last_mut().unwrap();
        let mut tx: Transaction = bitcoin::consensus::deserialize(&last.raw).unwrap();
        let mut witness: Vec<Vec<u8>> = tx.input[0]
            .witness
            .iter()
            .map(|item| item.to_vec())
            .collect();
        witness[0][5] ^= 1;
        tx.input[0].witness = bitcoin::Witness::from_slice(&witness);
        last.raw = bitcoin::consensus::serialize(&tx);
        swap.btc_claim_raw = Some(last.raw.clone());
        assert!(validate(&swap).is_err());
    }

    #[test]
    fn rejects_altered_claim_witness_components() {
        for index in 1..5 {
            let mut swap = state_with_chain();
            let last = swap.btc_claim_replacements.last_mut().unwrap();
            let mut tx: Transaction = bitcoin::consensus::deserialize(&last.raw).unwrap();
            let mut witness: Vec<Vec<u8>> = tx.input[0]
                .witness
                .iter()
                .map(|item| item.to_vec())
                .collect();
            witness[index][0] ^= 1;
            tx.input[0].witness = bitcoin::Witness::from_slice(&witness);
            last.raw = bitcoin::consensus::serialize(&tx);
            swap.btc_claim_raw = Some(last.raw.clone());
            assert!(validate(&swap).is_err(), "altered witness item {index}");
        }
    }

    #[test]
    fn rejects_wrong_recorded_funding_amount() {
        let mut swap = state_with_chain();
        swap.btc_amount += 1;
        for replacement in &mut swap.btc_claim_replacements {
            replacement.total_fee_satoshis += 1;
        }
        assert!(validate(&swap).is_err());
    }
}
