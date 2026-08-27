use vtorrent_wallet::tx_builder::{TxBuilder, MIN_ABSOLUTE_FEE_SATS};

pub fn build_payment(
    utxos: &[vtorrent_node::chain::Utxo],
    recipient: &str,
    change_address: &str,
    amount_satoshis: u64,
    fee_rate: u64,
    wif: &str,
) -> Result<vtorrent_node::block::Transaction, vtorrent_wallet::error::WalletError> {
    TxBuilder::new()
        .recipient(recipient, amount_satoshis)
        .change_address(change_address)
        .fee_rate(fee_rate)
        .min_absolute_fee(MIN_ABSOLUTE_FEE_SATS)
        .sign_with_wif(wif)
        .build(utxos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtorrent_node::chain::Utxo;

    fn utxo(value: u64) -> Utxo {
        Utxo {
            txid: [9u8; 32],
            vout: 0,
            value,
            script_pubkey: vec![0x51],
            height: 1,
            timestamp: 0,
        }
    }

    #[test]
    fn build_payment_enforces_relay_floor() {
        let utxos = vec![utxo(50_000_000_000)];
        let tx = build_payment(
            &utxos,
            "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh",
            "VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k",
            100_000_000,
            1,
            "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS",
        )
        .unwrap();
        let fee = 50_000_000_000u64 - tx.outputs.iter().map(|o| o.value).sum::<u64>();
        assert!(
            fee >= MIN_ABSOLUTE_FEE_SATS,
            "fee {} below relay floor",
            fee
        );
    }
}
