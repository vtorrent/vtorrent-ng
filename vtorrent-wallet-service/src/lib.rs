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

/// Inputs for an incentive settlement payment.
///
/// A plain struct (rather than `vtorrent_torrent::payment::PaymentDue`) so
/// this crate does not depend on the torrent engine.
pub struct IncentivePayment {
    /// The peer's VTR address.
    pub peer_address: String,
    /// Amount owed in satoshis.
    pub amount_satoshis: u64,
}

/// Error type for incentive settlement.
#[derive(Debug, thiserror::Error)]
pub enum IncentivePaymentError {
    #[error("wallet not unlocked")]
    WalletLocked,
    #[error("change address not set")]
    NoChangeAddress,
    #[error("no UTXOs available for wallet address")]
    NoUtxos,
    #[error("tx build failed: {0}")]
    Build(#[from] vtorrent_wallet::error::WalletError),
    #[error("mempool rejected payment: {0}")]
    Mempool(String),
}

/// Build, admit, and broadcast a VTR payment for an incentive settlement.
///
/// Shared by the daemon's payment channel (and reusable by any frontend that
/// settles torrent incentives): reads the hot wallet state, builds the tx via
/// the single `build_payment` path, admits it to the mempool with a chain-
/// derived fee, and submits it for P2P broadcast. Returns the txid hex.
pub async fn build_incentive_payment(
    wallet_wif: &tokio::sync::RwLock<Option<String>>,
    wallet_change_address: &tokio::sync::RwLock<Option<String>>,
    chain: &tokio::sync::Mutex<vtorrent_node::chain::Chain>,
    mempool: &tokio::sync::Mutex<vtorrent_node::mempool::Mempool>,
    tx_submit: Option<&tokio::sync::mpsc::Sender<vtorrent_node::block::Transaction>>,
    payment: &IncentivePayment,
) -> Result<String, IncentivePaymentError> {
    let wif = wallet_wif
        .read()
        .await
        .clone()
        .ok_or(IncentivePaymentError::WalletLocked)?;
    let change_address = wallet_change_address
        .read()
        .await
        .clone()
        .ok_or(IncentivePaymentError::NoChangeAddress)?;

    let utxos: Vec<vtorrent_node::chain::Utxo> = {
        let chain = chain.lock().await;
        chain.get_utxos_for_address(&change_address)
    };
    if utxos.is_empty() {
        return Err(IncentivePaymentError::NoUtxos);
    }

    let fee_rate = {
        let mempool = mempool.lock().await;
        mempool.recommended_fee_rate().max(1)
    };

    let tx = build_payment(
        &utxos,
        &payment.peer_address,
        &change_address,
        payment.amount_satoshis,
        fee_rate,
        &wif,
    )?;

    let txid = hex::encode(tx.txid());
    {
        let chain = chain.lock().await;
        let mut mempool = mempool.lock().await;
        mempool
            .admit_with_chain_fee(&chain, tx.clone())
            .map_err(|e| IncentivePaymentError::Mempool(e.to_string()))?;
    }
    if let Some(sender) = tx_submit {
        let _ = sender.try_send(tx);
    }
    Ok(txid)
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

    #[tokio::test]
    async fn build_incentive_payment_errors_when_wallet_locked() {
        use vtorrent_node::chain::Chain;
        use vtorrent_node::mempool::Mempool;

        let chain = Chain::new().unwrap();
        let wif: tokio::sync::RwLock<Option<String>> = tokio::sync::RwLock::new(None);
        let change: tokio::sync::RwLock<Option<String>> = tokio::sync::RwLock::new(None);
        let chain = tokio::sync::Mutex::new(chain);
        let mempool = tokio::sync::Mutex::new(Mempool::new(100));

        let err = build_incentive_payment(
            &wif,
            &change,
            &chain,
            &mempool,
            None,
            &IncentivePayment {
                peer_address: "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh".into(),
                amount_satoshis: 1_000,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IncentivePaymentError::WalletLocked));
    }

    #[tokio::test]
    async fn build_incentive_payment_errors_when_no_utxos() {
        use vtorrent_node::chain::Chain;
        use vtorrent_node::mempool::Mempool;

        let chain = Chain::new().unwrap();
        let wif: tokio::sync::RwLock<Option<String>> = tokio::sync::RwLock::new(Some(
            "WKDp3QTHd1wVakAcMe3MgHo4zz791x3x34awrvUpY5ojoqPWdFfS".into(),
        ));
        let change: tokio::sync::RwLock<Option<String>> =
            tokio::sync::RwLock::new(Some("VDR9EJdwPbfqER4L8rSQ85bpyYAtn7Q41k".into()));
        let chain = tokio::sync::Mutex::new(chain);
        let mempool = tokio::sync::Mutex::new(Mempool::new(1000));

        let err = build_incentive_payment(
            &wif,
            &change,
            &chain,
            &mempool,
            None,
            &IncentivePayment {
                peer_address: "VQ2BZDB3MzX5CEKVCoFJpzqw4eisdEMJHh".into(),
                amount_satoshis: 1_000,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IncentivePaymentError::NoUtxos));
    }
}
