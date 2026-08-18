//! Top-level Bitcoin wallet facade.

use crate::error::Result;
use crate::headers::HeaderChain;
use crate::keys::derive_address;
use crate::utxo::{Utxo, UtxoSet};
use std::sync::{Arc, Mutex};

/// A Bitcoin SPV wallet.
pub struct BtcWallet {
    seed: [u8; 64],
    headers: Arc<Mutex<HeaderChain>>,
    utxos: Arc<Mutex<UtxoSet>>,
    next_index: u32,
}

impl BtcWallet {
    /// Create a wallet from a 64-byte BIP39 seed.
    pub fn new(seed: [u8; 64]) -> Self {
        Self {
            seed,
            headers: Arc::new(Mutex::new(HeaderChain::new())),
            utxos: Arc::new(Mutex::new(UtxoSet::new())),
            next_index: 0,
        }
    }

    /// Derive the next unused receiving address.
    pub fn next_address(&mut self) -> Result<String> {
        let addr = derive_address(&self.seed, self.next_index)?;
        self.next_index += 1;
        Ok(addr)
    }

    /// The current receiving address (without advancing).
    pub fn current_address(&self) -> Result<String> {
        derive_address(&self.seed, self.next_index)
    }

    /// Derive the WIF private key for the given index.
    pub fn derive_wif(&self, index: u32) -> Result<String> {
        crate::keys::derive_wif(&self.seed, index)
    }

    /// Total confirmed balance in satoshis.
    pub fn balance(&self) -> u64 {
        self.utxos.lock().unwrap().total()
    }

    /// List all UTXOs.
    pub fn list_utxos(&self) -> Vec<Utxo> {
        self.utxos.lock().unwrap().list().to_vec()
    }

    /// Best known header height.
    pub fn best_height(&self) -> u32 {
        self.headers.lock().unwrap().best_height()
    }

    /// Add a header to the chain.
    pub fn add_header(&self, raw: &[u8], height: u32) -> Result<()> {
        self.headers.lock().unwrap().add_header(raw, height)
    }

    /// Add a UTXO.
    pub fn add_utxo(&self, utxo: Utxo) {
        self.utxos.lock().unwrap().add(utxo);
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
}
