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

// ─── BTC HTLC funding (shared by RPC and Tauri swap flows) ──────────────────

/// Build, sign, and broadcast a BTC HTLC funding transaction.
///
/// Shared by the RPC and Tauri `btc_fund` flows. Selects a UTXO from the
/// local BTC wallet, builds and signs the funding tx, and broadcasts it via
/// the provided broadcast hook (the caller decides whether to use the
/// daemon's configured peer or the default seed resolution).
///
/// Returns the internal (little-endian) funding txid.
/// Build, sign, and broadcast a BTC HTLC funding transaction.
///
/// Shared by the RPC and Tauri `btc_fund` flows. Selects a UTXO from the
/// local BTC wallet, builds and signs the funding tx, and broadcasts it via
/// the provided broadcast hook (the caller decides whether to use the
/// daemon's configured peer or the default seed resolution).
///
/// Returns the funding txid (internal byte order) and the HTLC expiry used.
pub async fn build_btc_htlc_funding(
    btc_wallet: &vtorrent_btc::wallet::BtcWallet,
    hash_lock: [u8; 32],
    maker_btc_address: &str,
    btc_refund_address: &str,
    btc_amount: u64,
    broadcast: impl AsyncFnOnce(&[u8]) -> Result<[u8; 32], String>,
) -> Result<([u8; 32], u32), String> {
    use vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS;

    let network = btc_wallet.network();
    let htlc = vtorrent_btc::htlc::BtcHtlc::new_with_network(
        hash_lock,
        maker_btc_address.to_string(),
        btc_refund_address.to_string(),
        vtorrent_btc::htlc::DEFAULT_HTLC_LOCKTIME,
        btc_amount,
        network,
    )
    .map_err(|e| format!("Unable to construct BTC HTLC: {}", e))?;
    let expiry = htlc.expiry;

    let funding_utxo = {
        let utxos = btc_wallet.list_utxos();
        let selected = utxo_select(&utxos, btc_amount, BTC_HTLC_FEE_SATOSHIS)
            .ok_or("Insufficient BTC funds")?;
        selected
            .into_iter()
            .max_by_key(|u| u.value)
            .ok_or("No BTC UTXO available")?
    };
    let funder_wif = btc_wallet.derive_wif(0).map_err(|e| e.to_string())?;
    let change_address = btc_wallet.current_address().map_err(|e| e.to_string())?;

    let funding_txid_bytes: [u8; 32] = {
        use bitcoin::hashes::Hash;
        funding_utxo
            .txid
            .parse::<bitcoin::Txid>()
            .map(|t| t.to_byte_array())
            .map_err(|e| format!("Invalid UTXO txid: {}", e))?
    };

    let unsigned = htlc
        .build_funding_tx(
            funding_txid_bytes,
            funding_utxo.vout,
            funding_utxo.value,
            BTC_HTLC_FEE_SATOSHIS,
            &change_address,
        )
        .map_err(|e| format!("Unable to build BTC funding tx: {}", e))?;
    let signed = htlc
        .sign_funding_tx(unsigned, funding_utxo.value, &funder_wif)
        .map_err(|e| format!("Unable to sign BTC funding tx: {}", e))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    broadcast(&raw).await?;
    Ok((txid, expiry))
}

/// Greedy single-UTXO selection: pick the smallest set of largest UTXOs
/// covering `amount + fee`. Returns `None` when funds are insufficient.
pub fn utxo_select(
    utxos: &[vtorrent_btc::utxo::Utxo],
    amount: u64,
    fee: u64,
) -> Option<Vec<vtorrent_btc::utxo::Utxo>> {
    let required = amount.checked_add(fee)?;
    let mut sorted: Vec<vtorrent_btc::utxo::Utxo> = utxos.to_vec();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    let mut selected = Vec::new();
    let mut sum = 0u64;
    for u in sorted {
        sum = sum.saturating_add(u.value);
        selected.push(u);
        if sum >= required {
            return Some(selected);
        }
    }
    None
}

#[cfg(test)]
mod btc_funding_tests {
    use super::*;

    fn btc_utxo(value: u64) -> vtorrent_btc::utxo::Utxo {
        vtorrent_btc::utxo::Utxo {
            txid: format!("{:064x}", value),
            vout: 0,
            value,
            address: String::new(),
            height: 800_000,
        }
    }

    #[test]
    fn utxo_select_prefers_largest_first() {
        let utxos = vec![btc_utxo(1_000), btc_utxo(50_000), btc_utxo(10_000)];
        let selected = utxo_select(&utxos, 20_000, 1_000).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].value, 50_000);
    }

    #[test]
    fn utxo_select_combines_when_single_utxo_insufficient() {
        let utxos = vec![btc_utxo(15_000), btc_utxo(10_000)];
        let selected = utxo_select(&utxos, 20_000, 1_000).unwrap();
        assert_eq!(selected.len(), 2);
        let total: u64 = selected.iter().map(|u| u.value).sum();
        assert!(total >= 21_000);
    }

    #[test]
    fn utxo_select_returns_none_when_insufficient() {
        let utxos = vec![btc_utxo(10_000), btc_utxo(5_000)];
        assert!(utxo_select(&utxos, 20_000, 1_000).is_none());
    }
}
// ─── VTR HTLC claim (shared by RPC and Tauri swap flows) ────────────────────

/// Build and sign a VTR HTLC claim transaction (taker reveals the preimage).
///
/// Shared by the RPC and Tauri `vtr_claim` flows. Reconstructs the HTLC
/// exactly as funded, builds the claim tx, and signs over the HTLC script
/// with the scriptSig `<sig> <pubkey> <preimage> OP_1`.
///
/// Returns the signed claim transaction; the caller admits it to the mempool
/// and updates swap state.
/// Parameters for a VTR HTLC claim.
pub struct VtrClaimParams<'a> {
    /// The HTLC hash lock.
    pub hash_lock: [u8; 32],
    /// The taker's VTR address (HTLC recipient / claimer).
    pub taker_address: &'a str,
    /// The maker's VTR address (HTLC refund address).
    pub maker_address: &'a str,
    /// The exact expiry the funding output was built with.
    pub expiry: u32,
    /// The swap amount in satoshis.
    pub vtr_amount: u64,
    /// The VTR funding txid (internal byte order).
    pub funding_txid: [u8; 32],
    /// The preimage revealing the hash lock.
    pub preimage: [u8; 32],
    /// The taker's WIF key for signing.
    pub taker_wif: &'a str,
}

/// Build and sign a VTR HTLC claim transaction (taker reveals the preimage).
///
/// Shared by the RPC and Tauri `vtr_claim` flows. Reconstructs the HTLC
/// exactly as funded, builds the claim tx, and signs over the HTLC script
/// with the scriptSig `<sig> <pubkey> <preimage> OP_1`.
///
/// Returns the signed claim transaction; the caller admits it to the mempool
/// and updates swap state.
pub fn build_vtr_htlc_claim(
    params: VtrClaimParams<'_>,
) -> Result<vtorrent_node::block::Transaction, String> {
    let VtrClaimParams {
        hash_lock,
        taker_address,
        maker_address,
        expiry,
        vtr_amount,
        funding_txid,
        preimage,
        taker_wif,
    } = params;
    use vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

    let htlc = vtorrent_node::atomic_swap::Htlc::with_expiry(
        hash_lock,
        taker_address.to_string(),
        maker_address.to_string(),
        expiry,
        vtr_amount,
    )
    .map_err(|e| format!("Unable to reconstruct HTLC: {}", e))?;

    let unsigned = htlc
        .build_claim_tx_unsigned(funding_txid, &preimage, VTR_HTLC_FEE_SATOSHIS)
        .map_err(|e| format!("Unable to build VTR claim tx: {}", e))?;

    let htlc_script = htlc
        .build_script()
        .map_err(|e| format!("Invalid HTLC addresses: {}", e))?;
    let (sig, pubkey) = sign_input_over_subscript(&unsigned, 0, &htlc_script, taker_wif)
        .map_err(|e| format!("Unable to sign VTR claim tx: {}", e))?;

    let mut script_sig = Vec::new();
    script_sig.push(sig.len() as u8);
    script_sig.extend_from_slice(&sig);
    script_sig.push(pubkey.len() as u8);
    script_sig.extend_from_slice(&pubkey);
    script_sig.push(0x20);
    script_sig.extend_from_slice(&preimage);
    script_sig.push(0x51); // OP_1

    let mut claim_tx = unsigned;
    claim_tx.inputs[0].script_sig = script_sig;
    Ok(claim_tx)
}

// ─── BTC HTLC claim (shared by RPC and Tauri swap flows) ────────────────────

/// Parameters for a BTC HTLC claim (maker reclaims BTC with the preimage).
pub struct BtcClaimParams<'a> {
    /// The BTC funding txid (internal byte order).
    pub funding_txid: [u8; 32],
    /// The preimage revealed by the taker.
    pub preimage: [u8; 32],
    /// The maker's BTC address (HTLC recipient).
    pub maker_btc_address: &'a str,
    /// The taker's BTC refund address embedded in the witness script.
    pub refund_address: &'a str,
    /// The exact expiry the funding output was built with.
    pub expiry: u32,
    /// The swap amount in satoshis.
    pub amount: u64,
    /// The BTC network the HTLC was funded on.
    pub network: bitcoin::Network,
}

/// Build and sign a BTC HTLC claim transaction.
///
/// Shared by the RPC and Tauri `btc_claim` flows. The maker's WIF is derived
/// from the wallet seed at index 0. Returns the raw serialized tx and its
/// internal txid; the caller broadcasts and updates swap state.
pub fn build_btc_htlc_claim(
    btc_wallet: &vtorrent_btc::wallet::BtcWallet,
    params: BtcClaimParams<'_>,
) -> Result<(Vec<u8>, [u8; 32]), String> {
    use vtorrent_node::atomic_swap::BTC_HTLC_FEE_SATOSHIS;

    let BtcClaimParams {
        funding_txid,
        preimage,
        maker_btc_address,
        refund_address,
        expiry,
        amount,
        network,
    } = params;

    let hash_lock = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(preimage);
        let d = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&d);
        out
    };

    let htlc = vtorrent_btc::htlc::BtcHtlc {
        hash_lock,
        recipient: maker_btc_address.to_string(),
        refund_address: refund_address.to_string(),
        expiry,
        amount,
        network,
    };

    let unsigned = htlc
        .build_claim_tx(funding_txid, &preimage, BTC_HTLC_FEE_SATOSHIS)
        .map_err(|e| format!("Unable to build BTC claim tx: {}", e))?;
    let maker_wif = btc_wallet.derive_wif(0).map_err(|e| e.to_string())?;
    let signed = htlc
        .sign_claim_tx(unsigned, &preimage, &maker_wif)
        .map_err(|e| format!("Unable to sign BTC claim tx: {}", e))?;
    let raw = bitcoin::consensus::encode::serialize(&signed);
    let txid = {
        use bitcoin::hashes::Hash;
        signed.compute_txid().to_byte_array()
    };
    Ok((raw, txid))
}

// ─── VTR HTLC refund (shared by RPC and Tauri swap flows) ───────────────────

/// Parameters for a VTR HTLC refund (maker reclaims VTR after expiry).
pub struct VtrRefundParams<'a> {
    /// The HTLC hash lock.
    pub hash_lock: [u8; 32],
    /// The taker's VTR address (HTLC recipient).
    pub taker_address: &'a str,
    /// The maker's VTR address (HTLC refund address / signer).
    pub maker_address: &'a str,
    /// The exact expiry the funding output was built with.
    pub expiry: u32,
    /// The swap amount in satoshis.
    pub vtr_amount: u64,
    /// The VTR funding txid (internal byte order).
    pub funding_txid: [u8; 32],
    /// The maker's WIF key for signing.
    pub maker_wif: &'a str,
}

/// Build and sign a VTR HTLC refund transaction (maker reclaims after expiry).
///
/// Shared by the RPC and Tauri `swap_refund` VTR legs. scriptSig is
/// `<sig> <pubkey> OP_0` (the false/timelock branch).
///
/// Returns the signed refund transaction.
pub fn build_vtr_htlc_refund(
    params: VtrRefundParams<'_>,
) -> Result<vtorrent_node::block::Transaction, String> {
    use vtorrent_node::atomic_swap::VTR_HTLC_FEE_SATOSHIS;
    use vtorrent_wallet::tx_builder::sign_input_over_subscript;

    let VtrRefundParams {
        hash_lock,
        taker_address,
        maker_address,
        expiry,
        vtr_amount,
        funding_txid,
        maker_wif,
    } = params;

    let htlc = vtorrent_node::atomic_swap::Htlc::with_expiry(
        hash_lock,
        taker_address.to_string(),
        maker_address.to_string(),
        expiry,
        vtr_amount,
    )
    .map_err(|e| format!("Unable to reconstruct HTLC: {}", e))?;

    let unsigned = htlc
        .build_refund_tx_unsigned(funding_txid, VTR_HTLC_FEE_SATOSHIS)
        .map_err(|e| format!("Unable to build VTR refund tx: {}", e))?;

    let htlc_script = htlc
        .build_script()
        .map_err(|e| format!("Invalid HTLC addresses: {}", e))?;
    let (sig, pubkey) = sign_input_over_subscript(&unsigned, 0, &htlc_script, maker_wif)
        .map_err(|e| format!("Unable to sign VTR refund tx: {}", e))?;

    let mut script_sig = Vec::new();
    script_sig.push(sig.len() as u8);
    script_sig.extend_from_slice(&sig);
    script_sig.push(pubkey.len() as u8);
    script_sig.extend_from_slice(&pubkey);
    script_sig.push(0x00); // OP_0

    let mut refund_tx = unsigned;
    refund_tx.inputs[0].script_sig = script_sig;
    Ok(refund_tx)
}
