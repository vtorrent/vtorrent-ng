use crate::error::{RpcError, RpcResult};
use vtorrent_node::atomic_swap::{Htlc, SwapOrder, SwapState, VtrRefundReplacement};

pub(crate) const MAX_REPLACEMENTS: usize = 16;

pub(crate) fn is_prefix(old: &[VtrRefundReplacement], new: &[VtrRefundReplacement]) -> bool {
    old.len() <= new.len()
        && old.iter().zip(new).all(|(a, b)| {
            a.replaces_txid == b.replaces_txid
                && a.total_fee_satoshis == b.total_fee_satoshis
                && a.approved_at == b.approved_at
                && a.transaction.txid() == b.transaction.txid()
        })
}

pub(crate) fn validate(order: &SwapOrder, swap: &SwapState) -> RpcResult<()> {
    let bad = || RpcError::BadRequest("Invalid VTR refund replacement lineage".into());
    if swap.vtr_refund_replacements.is_empty() {
        return Ok(());
    }
    if swap.vtr_refund_replacements.len() > MAX_REPLACEMENTS {
        return Err(bad());
    }
    let original = swap.vtr_refund_tx.as_ref().ok_or_else(bad)?;
    let funding = swap.vtr_funding_txid.ok_or_else(bad)?;
    if swap.order_id != order.order_id
        || order.hash_lock != Some(swap.hash_lock)
        || order.funding_txid != Some(funding)
        || swap.vtr_refund_txid != Some(original.txid())
        || original.outputs.len() != 1
        || original.inputs.len() != 1
    {
        return Err(bad());
    }
    let htlc = Htlc::with_expiry(
        swap.hash_lock,
        order.taker_address.clone().ok_or_else(bad)?,
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|_| bad())?;
    let mut parent = original.txid();
    let mut fee = order
        .vtr_amount
        .checked_sub(original.outputs[0].value)
        .ok_or_else(bad)?;
    for replacement in &swap.vtr_refund_replacements {
        let tx = &replacement.transaction;
        if replacement.replaces_txid != parent
            || replacement.total_fee_satoshis <= fee
            || replacement.approved_at < u64::from(order.expiry)
            || order
                .vtr_amount
                .checked_sub(replacement.total_fee_satoshis)
                .is_none_or(|v| v < 546)
            || tx.inputs.len() != 1
            || tx.inputs[0].script_sig.is_empty()
        {
            return Err(bad());
        }
        let mut expected = htlc
            .build_refund_tx_unsigned(funding, replacement.total_fee_satoshis)
            .map_err(|_| bad())?;
        expected.inputs[0].sequence = u32::MAX - 2;
        expected.inputs[0].script_sig = tx.inputs[0].script_sig.clone();
        if expected.txid() != tx.txid() {
            return Err(bad());
        }
        parent = tx.txid();
        fee = replacement.total_fee_satoshis;
    }
    Ok(())
}
