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
        {
            return Err(bad());
        }
        if previous_fee.is_some_and(|fee| replacement.total_fee_satoshis <= fee) {
            return Err(bad());
        }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::absolute::LockTime;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, OutPoint, Sequence, Transaction, TxIn, TxOut, Witness};

    fn claim(funding: [u8; 32], fee: u64) -> (Vec<u8>, [u8; 32]) {
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array(funding),
                    vout: 0,
                },
                script_sig: Default::default(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(100_000 - fee),
                script_pubkey: Default::default(),
            }],
        };
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
    fn rejects_a_broken_parent_link() {
        let mut swap = state_with_chain();
        swap.btc_claim_replacements[1].replaces_txid = [9u8; 32];
        assert!(validate(&swap).is_err());
    }
}
