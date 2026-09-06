use vtorrent_node::{
    atomic_swap::{Htlc, SwapOrder},
    chain::{Chain, Utxo},
};

/// Time reserved for the taker to claim VTR after the maker reveals the secret on BTC.
pub const SWAP_CLAIM_SAFETY_MARGIN: u32 = 6 * 3600;
/// Minimum remaining BTC funding/claim window.
pub const MIN_BTC_SWAP_WINDOW: u32 = vtorrent_btc::sync::MIN_BTC_CLAIM_WINDOW;
/// Required VTR funding depth before the taker commits BTC.
pub const VTR_SWAP_CONFIRMATIONS: u32 = 6;

/// Contract terms checked against the local confirmed VTR chain.
pub struct VerifiedSwapFunding {
    pub(crate) order_id: String,
    pub(crate) hash_lock: [u8; 32],
    pub(crate) maker_btc_address: String,
    pub(crate) btc_amount: u64,
    pub(crate) btc_expiry: u32,
}

impl VerifiedSwapFunding {
    pub fn btc_expiry(&self) -> u32 {
        self.btc_expiry
    }
}

/// Choose a BTC refund deadline before the VTR refund deadline.
pub fn btc_swap_expiry(vtr_expiry: u32, now: u64) -> Result<u32, String> {
    let now = u32::try_from(now).map_err(|_| "Swap clock exceeds u32::MAX")?;
    if now < 500_000_000 {
        return Err("Swap clock must be a Unix timestamp, not a block height".into());
    }
    let latest = vtr_expiry
        .checked_sub(SWAP_CLAIM_SAFETY_MARGIN)
        .ok_or("VTR expiry cannot accommodate the claim safety margin")?;
    let minimum = now
        .checked_add(MIN_BTC_SWAP_WINDOW)
        .ok_or("BTC swap expiry overflow")?;
    if latest < minimum {
        return Err("VTR expiry is too close to fund BTC safely".into());
    }
    Ok(latest.min(now.saturating_add(vtorrent_btc::htlc::DEFAULT_HTLC_LOCKTIME)))
}

/// Verify the exact unspent VTR output and its confirmation depth before BTC funding.
pub fn verify_vtr_swap_funding(
    chain: &Chain,
    order: &SwapOrder,
    now: u64,
) -> Result<VerifiedSwapFunding, String> {
    let funding_txid = order.funding_txid.ok_or("Order has no VTR funding txid")?;
    let utxo = chain
        .get_utxo(&funding_txid, 0)
        .ok_or("VTR funding output is unconfirmed, missing, or already spent")?;
    verify_output(order, utxo, chain.best_height(), now)
}

fn verify_output(
    order: &SwapOrder,
    utxo: &Utxo,
    tip_height: u32,
    now: u64,
) -> Result<VerifiedSwapFunding, String> {
    if order.target_asset != "BTC"
        || order.target_amount <= vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS
        || order.vtr_amount <= vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS
    {
        return Err("Swap amounts must exceed each chain's claim/refund fee".into());
    }
    let hash_lock = order.hash_lock.ok_or("Order has no hash lock")?;
    let taker = order
        .taker_address
        .as_ref()
        .ok_or("Order has no taker address")?;
    let maker_btc_address = order
        .maker_btc_address
        .clone()
        .ok_or("Order has no maker BTC address")?;
    let htlc = Htlc::with_expiry(
        hash_lock,
        taker.clone(),
        order.maker_address.clone(),
        order.expiry,
        order.vtr_amount,
    )
    .map_err(|e| e.to_string())?;
    if Some(utxo.txid) != order.funding_txid
        || utxo.vout != 0
        || utxo.value != order.vtr_amount
        || utxo.script_pubkey != htlc.build_script().map_err(|e| e.to_string())?
    {
        return Err("Confirmed VTR funding output does not match the order contract".into());
    }
    let confirmations = tip_height
        .checked_sub(utxo.height)
        .map(|depth| u64::from(depth) + 1)
        .unwrap_or(0);
    if confirmations < u64::from(VTR_SWAP_CONFIRMATIONS) {
        return Err(format!(
            "VTR funding requires {VTR_SWAP_CONFIRMATIONS} confirmations; found {confirmations}"
        ));
    }
    Ok(VerifiedSwapFunding {
        order_id: hex::encode(order.order_id),
        hash_lock,
        maker_btc_address,
        btc_amount: order.target_amount,
        btc_expiry: btc_swap_expiry(order.expiry, now)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u32 = 1_800_000_000;

    fn funded_order() -> (SwapOrder, Utxo) {
        let mut order = SwapOrder::new(
            "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k".into(),
            100_000_000,
            "BTC".into(),
            100_000,
            48 * 3600,
        );
        order.expiry = NOW + 48 * 3600;
        order.hash_lock = Some([42; 32]);
        order.funding_txid = Some([7; 32]);
        order.taker_address = Some("VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh".into());
        order.maker_btc_address = Some("bcrt1q50rtrmj2f8vl9tem8qpfw36ylw5jg9j2dkku3w".into());
        let htlc = Htlc::with_expiry(
            order.hash_lock.unwrap(),
            order.taker_address.clone().unwrap(),
            order.maker_address.clone(),
            order.expiry,
            order.vtr_amount,
        )
        .unwrap();
        let utxo = Utxo {
            txid: [7; 32],
            vout: 0,
            value: order.vtr_amount,
            script_pubkey: htlc.build_script().unwrap(),
            height: 100,
            timestamp: NOW - 600,
        };
        (order, utxo)
    }

    #[test]
    fn expiry_preserves_margin_and_caps_btc_window() {
        assert_eq!(
            btc_swap_expiry(NOW + 48 * 3600, NOW.into()).unwrap(),
            NOW + 42 * 3600
        );
        assert_eq!(
            btc_swap_expiry(NOW + 7 * 24 * 3600, NOW.into()).unwrap(),
            NOW + 48 * 3600
        );
        let boundary = NOW + SWAP_CLAIM_SAFETY_MARGIN + MIN_BTC_SWAP_WINDOW;
        assert_eq!(
            btc_swap_expiry(boundary, NOW.into()).unwrap(),
            NOW + MIN_BTC_SWAP_WINDOW
        );
        assert!(btc_swap_expiry(boundary - 1, NOW.into()).is_err());
        for (expiry, now) in [
            (NOW, NOW.into()),
            (u32::MAX, u64::MAX),
            (u32::MAX, u32::MAX.into()),
            (NOW, 0),
        ] {
            assert!(btc_swap_expiry(expiry, now).is_err());
        }
    }

    #[test]
    fn verifies_exact_contract_and_confirmation_boundary() {
        let (order, utxo) = funded_order();
        assert!(verify_output(&order, &utxo, 105, NOW.into()).is_ok());
        assert!(verify_output(&order, &utxo, 104, NOW.into()).is_err());
        assert!(verify_output(&order, &utxo, 99, NOW.into()).is_err());
        for field in 0..4 {
            let mut wrong = utxo.clone();
            match field {
                0 => wrong.value -= 1,
                1 => wrong.script_pubkey[0] ^= 1,
                2 => wrong.vout = 1,
                _ => wrong.txid = [8; 32],
            }
            assert!(verify_output(&order, &wrong, 105, NOW.into()).is_err());
        }
        let mut changed_order = order.clone();
        changed_order.expiry += 1;
        assert!(verify_output(&changed_order, &utxo, 105, NOW.into()).is_err());
        let mut dust_order = order.clone();
        dust_order.target_amount = vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS;
        assert!(verify_output(&dust_order, &utxo, 105, NOW.into()).is_err());
    }

    #[test]
    fn rejects_claimed_funded_flag_without_chain_output() {
        let (order, _) = funded_order();
        let chain = Chain::new_regtest().unwrap();
        assert!(verify_vtr_swap_funding(&chain, &order, NOW.into()).is_err());
    }
}
