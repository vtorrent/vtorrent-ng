//! Top-level Bitcoin wallet facade.

use crate::error::Result;
use crate::filters::FilterHeaderStore;
use crate::headers::HeaderChain;
use crate::keys::derive_address;
use crate::utxo::{Utxo, UtxoSet};
use bitcoin::Network;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;

/// A Bitcoin SPV wallet.
pub struct BtcWallet {
    seed: [u8; 64],
    network: Network,
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    filter_headers: Arc<Mutex<FilterHeaderStore>>,
    /// Optional path for persisting the UTXO set to disk.
    utxo_path: Option<PathBuf>,
    next_index: u32,
    synced: bool,
    /// Next block height to scan (incremental checkpoint).
    last_scanned_height: u32,
    /// Whether new transactions signal RBF (Replace-By-Fee).
    rbf_enabled: bool,
}

impl Drop for BtcWallet {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.seed.zeroize();
    }
}

impl BtcWallet {
    /// Create a wallet from a 64-byte BIP39 seed.
    pub fn new(seed: [u8; 64]) -> Self {
        Self::with_network(seed, Network::Bitcoin)
    }

    /// Create a wallet for a specific Bitcoin network (mainnet/regtest).
    pub fn with_network(seed: [u8; 64], network: Network) -> Self {
        Self {
            seed,
            network,
            headers: Arc::new(Mutex::new(HeaderChain::anchored(network))),
            utxos: Arc::new(Mutex::new(UtxoSet::new())),
            filter_headers: Arc::new(Mutex::new(FilterHeaderStore::default())),
            utxo_path: None,
            next_index: 0,
            synced: false,
            last_scanned_height: 0,
            rbf_enabled: true,
        }
    }

    /// Create a wallet with a UTXO persistence path.  If the file exists,
    /// the UTXO set is loaded from disk on construction.
    pub fn with_persistence(
        seed: [u8; 64],
        network: Network,
        utxo_path: PathBuf,
    ) -> std::io::Result<Self> {
        let utxos = UtxoSet::load(&utxo_path)?;
        let next_index = utxos.next_index();
        Ok(Self {
            seed,
            network,
            headers: Arc::new(Mutex::new(HeaderChain::anchored(network))),
            utxos: Arc::new(Mutex::new(utxos)),
            filter_headers: Arc::new(Mutex::new(FilterHeaderStore::default())),
            utxo_path: Some(utxo_path),
            next_index,
            synced: false,
            last_scanned_height: 0,
            rbf_enabled: true,
        })
    }

    /// Set or change the UTXO persistence path.  The current in-memory set
    /// is immediately saved to the new path.
    pub fn set_utxo_path(&mut self, path: PathBuf) -> std::io::Result<()> {
        self.utxo_path = Some(path.clone());
        self.utxos.lock().save(&path)
    }

    /// Save the current UTXO set to disk (if a persistence path is set).
    fn save_utxos(&self) {
        if let Some(ref path) = self.utxo_path {
            if let Err(e) = self.utxos.lock().save(path) {
                tracing::warn!("Failed to save UTXO set: {}", e);
            }
        }
    }

    /// The Bitcoin network this wallet operates on.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Derive the next unused receiving address.
    pub fn next_address(&mut self) -> Result<String> {
        let addr = derive_address(&self.seed, self.next_index, self.network)?;
        let next_index = self
            .next_index
            .checked_add(1)
            .ok_or_else(|| crate::error::BtcError::Bitcoin("BTC address index overflow".into()))?;
        {
            let mut set = self.utxos.lock();
            let previous = set.clone();
            set.set_next_index(next_index);
            if let Some(path) = &self.utxo_path {
                if let Err(error) = set.save(path) {
                    *set = previous;
                    return Err(crate::error::BtcError::Bitcoin(format!(
                        "Could not persist BTC address index: {error}"
                    )));
                }
            }
        }
        self.next_index = next_index;
        Ok(addr)
    }

    /// The current receiving address (without advancing).
    pub fn current_address(&self) -> Result<String> {
        derive_address(&self.seed, self.next_index, self.network)
    }

    /// Derive the WIF private key for the given index.
    pub fn derive_wif(&self, index: u32) -> Result<String> {
        crate::keys::derive_wif(&self.seed, index, self.network)
    }

    /// Find the signing key for an address issued by this wallet.
    pub fn derive_wif_for_address(&self, address: &str) -> Result<String> {
        for index in 0..=self.next_index {
            if derive_address(&self.seed, index, self.network)? == address {
                return self.derive_wif(index);
            }
        }
        Err(crate::error::BtcError::InvalidAddress(
            "Address does not belong to this BTC wallet".into(),
        ))
    }

    /// Derive a gap of addresses (indices 0..=gap) for scanning.
    ///
    /// The wallet must watch a range of derived addresses, not just the
    /// current one, so funds sent to any previously-issued address are still
    /// discovered.
    pub fn watch_addresses(&self, gap: u32) -> Result<Vec<String>> {
        let mut addrs = Vec::with_capacity(gap as usize + 1);
        for i in 0..=gap {
            addrs.push(derive_address(&self.seed, i, self.network)?);
        }
        Ok(addrs)
    }

    /// Total confirmed balance in satoshis.
    pub fn balance(&self) -> u64 {
        self.utxos.lock().total()
    }

    /// List all UTXOs.
    pub fn list_utxos(&self) -> Vec<Utxo> {
        self.utxos.lock().list().to_vec()
    }

    /// Exclude a swap funding input from selection and future rescans before broadcast.
    pub fn reserve_swap_input(
        &self,
        input: &Utxo,
        raw: &[u8],
        order_id: String,
        htlc: &crate::htlc::BtcHtlc,
    ) -> Result<()> {
        let transaction: bitcoin::Transaction = bitcoin::consensus::deserialize(raw)
            .map_err(|error| crate::error::BtcError::Bitcoin(error.to_string()))?;
        let mut set = self.utxos.lock();
        if !set.list().iter().any(|utxo| {
            utxo.txid == input.txid
                && utxo.vout == input.vout
                && utxo.value == input.value
                && utxo.address == input.address
        }) {
            return Err(crate::error::BtcError::Bitcoin(
                "BTC funding input is no longer available".into(),
            ));
        }
        let previous = set.clone();
        set.reserve(&input.txid, input.vout);
        set.record_swap_transaction(transaction.compute_txid().to_string(), raw.to_vec());
        set.record_swap_contract(crate::utxo::SwapContract {
            order_id,
            funding_txid: transaction.compute_txid().to_string(),
            hash_lock: htlc.hash_lock,
            recipient: htlc.recipient.clone(),
            refund_address: htlc.refund_address.clone(),
            expiry: htlc.expiry,
            amount: htlc.amount,
            refund_raw: None,
        });
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                *set = previous;
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC input reservation: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Signed swap transactions retained for reconciliation after restart.
    pub fn pending_swap_transactions(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        self.utxos.lock().pending_swap_transactions().clone()
    }

    pub fn swap_contract(&self, order_id: &str) -> Option<crate::utxo::SwapContract> {
        self.utxos.lock().swap_contract(order_id).cloned()
    }

    pub fn record_swap_refund(&self, order_id: &str, raw: &[u8]) -> Result<()> {
        let mut set = self.utxos.lock();
        let mut contract = set.swap_contract(order_id).cloned().ok_or_else(|| {
            crate::error::BtcError::Bitcoin("Persisted BTC swap contract missing".into())
        })?;
        let previous = set.clone();
        contract.refund_raw = Some(raw.to_vec());
        set.record_swap_contract(contract);
        if let Some(path) = &self.utxo_path {
            if let Err(error) = set.save(path) {
                *set = previous;
                return Err(crate::error::BtcError::Bitcoin(format!(
                    "Could not persist BTC refund: {error}"
                )));
            }
        }
        Ok(())
    }

    /// Best known header height.
    pub fn best_height(&self) -> u32 {
        self.headers.lock().best_height()
    }

    /// Freshly scan a swap contract, without trusting the persisted wallet UTXO cache.
    pub async fn verify_swap_funding(
        &self,
        htlc: &crate::htlc::BtcHtlc,
        funding_txid: [u8; 32],
        peers: &[std::net::SocketAddr],
        now: u64,
    ) -> Result<()> {
        if htlc.network != self.network {
            return Err(crate::error::BtcError::Sync(
                "BTC swap network mismatch".into(),
            ));
        }
        let sync = crate::sync::BtcSync::new(
            self.headers.clone(),
            Arc::new(Mutex::new(UtxoSet::new())),
            vec![htlc.address()?],
            self.network,
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            sync.verify_swap_funding(htlc, funding_txid, peers, now),
        )
        .await
        .map_err(|_| crate::error::BtcError::Sync("BTC contract verification timed out".into()))?
    }

    /// Add a header to the chain.
    pub fn add_header(&self, raw: &[u8], height: u32) -> Result<()> {
        self.headers.lock().add_header(raw, height)
    }

    /// Add a UTXO.
    pub fn add_utxo(&self, utxo: Utxo) {
        self.utxos.lock().add(utxo);
        self.save_utxos();
    }

    /// Whether the header chain has synced at least once.
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Mark the wallet as synced.
    pub fn mark_synced(&mut self) {
        self.synced = true;
    }

    /// Run a sync pass against a peer, updating headers and the synced flag.
    pub async fn sync(&mut self, peer: &mut crate::p2p::BtcPeer) -> Result<usize> {
        let sync = crate::sync::BtcSync::new_with_filter_headers(
            self.headers.clone(),
            self.utxos.clone(),
            self.filter_headers.clone(),
            self.watch_addresses(self.next_index)?,
            self.network,
        );
        let added = sync.sync_once(peer).await?;
        if added > 0 {
            self.synced = true;
        }
        Ok(added)
    }

    /// Scan blocks using BIP-158 compact block filters (the modern,
    /// privacy-preserving alternative to BIP-37, which most mainnet nodes
    /// disable). Returns the number of blocks scanned.
    pub async fn scan_utxos_bip158(
        &self,
        peer: &mut crate::p2p::BtcPeer,
        start_height: u32,
    ) -> Result<usize> {
        let sync = crate::sync::BtcSync::new_with_filter_headers(
            self.headers.clone(),
            self.utxos.clone(),
            self.filter_headers.clone(),
            self.watch_addresses(self.next_index)?,
            self.network,
        );
        sync.scan_utxos_bip158(peer, start_height).await
    }

    /// The next block height to scan.
    pub fn last_scanned_height(&self) -> u32 {
        self.last_scanned_height
    }

    /// Set the next block height to scan.
    pub fn set_last_scanned_height(&mut self, height: u32) {
        self.last_scanned_height = height;
    }

    /// Whether new transactions signal RBF.
    pub fn rbf_enabled(&self) -> bool {
        self.rbf_enabled
    }

    /// Enable or disable RBF signaling for new transactions.
    pub fn set_rbf_enabled(&mut self, enabled: bool) {
        self.rbf_enabled = enabled;
    }

    /// Send BTC to `to_address`, selecting UTXOs, signing, and removing
    /// spent UTXOs from the in-memory set.
    ///
    /// `fee_sats` is a flat fee in satoshis (default 1 000).
    /// Returns `(txid_hex, raw_tx_bytes)` — the caller should broadcast
    /// `raw_tx_bytes` to the network.
    ///
    /// Only works when all selected UTXOs belong to the same derived address.
    pub fn send_to(
        &mut self,
        to_address: &str,
        amount_sats: u64,
        fee_sats: u64,
    ) -> Result<(String, Vec<u8>, Vec<Utxo>)> {
        use crate::keys::derive_wif;
        use crate::tx::{build_and_sign, txid_of};

        let selected = {
            let u = self.utxos.lock();
            match u.select(amount_sats, fee_sats) {
                Some(s) => s,
                None => {
                    let available = u.total();
                    return Err(crate::error::BtcError::InsufficientFunds {
                        available,
                        required: amount_sats + fee_sats,
                    });
                }
            }
        };

        let change_address = self.current_address()?;

        // Find the WIF whose derived address matches the selected UTXOs.
        let mut wif = None;
        for idx in 0..=self.next_index {
            let addr = derive_address(&self.seed, idx, self.network)?;
            if selected.iter().any(|u| u.address == addr) {
                if wif.is_some() {
                    return Err(crate::error::BtcError::InvalidAddress(
                        "send_to requires all UTXOs to belong to the same address index".into(),
                    ));
                }
                wif = Some(derive_wif(&self.seed, idx, self.network)?);
            }
        }
        let wif = wif.ok_or_else(|| {
            crate::error::BtcError::InvalidAddress("no matching address for selected UTXOs".into())
        })?;

        let raw = build_and_sign(
            &selected,
            to_address,
            amount_sats,
            fee_sats,
            &change_address,
            &wif,
            self.network,
            self.rbf_enabled,
        )?;

        let txid = txid_of(&raw);
        let txid_hex = hex::encode(txid);

        // Remove spent UTXOs from the in-memory set immediately (prevents
        // double-spending within this session) and persist. If broadcasting
        // later fails, the caller must roll back via [`BtcWallet::restore_utxos`]
        // using the returned selection — otherwise the wallet would forget
        // spendable outputs for a tx that never made it onto the network.
        let mut set = self.utxos.lock();
        for u in &selected {
            set.remove(&u.txid, u.vout);
        }
        drop(set);
        self.save_utxos();

        tracing::info!(
            "BTC built: {} sats to {} (txid: {}, fee: {} sats)",
            amount_sats,
            to_address,
            txid_hex,
            fee_sats,
        );

        Ok((txid_hex, raw, selected))
    }

    /// Re-add previously spent UTXOs (e.g. when a broadcast failed) and
    /// persist the restored set.
    pub fn restore_utxos(&self, utxos: &[Utxo]) {
        {
            let mut set = self.utxos.lock();
            for u in utxos {
                set.add(u.clone());
            }
        }
        self.save_utxos();
    }

    /// Broadcast a raw transaction to the Bitcoin network via DNS seeds.
    pub async fn broadcast_raw(raw: &[u8]) -> Result<[u8; 32]> {
        crate::sync::broadcast_tx(raw).await
    }

    /// Estimate the fee in satoshis for a transaction with the given number
    /// of inputs and outputs, targeting `target_blocks` confirmation.
    ///
    /// Uses a simple heuristic: the peer's feefilter as a floor, then
    /// applies a multiplier based on how urgently the user wants confirmation.
    ///
    /// - `target_blocks = 1` → high priority (2× feefilter)
    /// - `target_blocks = 3` → medium priority (1× feefilter)
    /// - `target_blocks = 6` → low priority (0.5× feefilter, min 1 sat/vB)
    pub fn estimate_fee(
        input_count: usize,
        output_count: usize,
        feefilter_sats_per_kb: i64,
        target_blocks: u32,
    ) -> u64 {
        // Peer-supplied feefilter is untrusted: clamp to a sane range so a
        // hostile value (e.g. i64::MAX) cannot overflow the fee computation.
        let base_rate = if (1..=10_000_000).contains(&feefilter_sats_per_kb) {
            feefilter_sats_per_kb as u64
        } else {
            1_000 // 1 sat/byte default
        };

        let multiplier = match target_blocks {
            0..=1 => 2000, // urgent: 2×
            2..=3 => 1000, // standard: 1×
            _ => 500,      // economy: 0.5×
        };

        let vsize = crate::tx::estimate_vsize(input_count, output_count);
        // fee = rate × multiplier/1000 × vsize / 1000
        let fee = (base_rate * multiplier * vsize).div_ceil(1_000_000);
        fee.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_address_advances() {
        let mut w = BtcWallet::new([7u8; 64]);
        let a = w.next_address().unwrap();
        let b = w.next_address().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with("bc1q"));
    }

    #[test]
    fn test_balance_tracks_utxos() {
        let w = BtcWallet::new([7u8; 64]);
        w.add_utxo(Utxo {
            txid: "11".repeat(32),
            vout: 0,
            value: 5000,
            address: "bc1qtest".to_string(),
            height: 1,
        });
        assert_eq!(w.balance(), 5000);
    }

    #[test]
    fn test_best_height_default_zero() {
        let w = BtcWallet::new([7u8; 64]);
        assert_eq!(w.best_height(), 0);
    }

    #[test]
    fn test_synced_default_false() {
        let w = BtcWallet::new([7u8; 64]);
        assert!(!w.synced());
    }
}
