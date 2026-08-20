//! BIP32/BIP84 key derivation and native SegWit address generation.

use crate::error::{BtcError, Result};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{Address, CompressedPublicKey, Network, NetworkKind};
use std::str::FromStr;

/// Derive the BIP84 native SegWit address for the given index.
pub fn derive_address(seed: &[u8; 64], index: u32, network: Network) -> Result<String> {
    let secp = Secp256k1::new();
    let xpriv =
        Xpriv::new_master(network, seed).map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let path = DerivationPath::from_str(&format!("m/84'/{}'/0'/0/{}", coin_type(network), index))
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let derived = xpriv
        .derive_priv(&secp, &path)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let pubkey =
        CompressedPublicKey::from_slice(&derived.private_key.public_key(&secp).serialize())
            .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let address = Address::p2wpkh(&pubkey, network);
    Ok(address.to_string())
}

/// Derive the private key (WIF) for the given index.
pub fn derive_wif(seed: &[u8; 64], index: u32, network: Network) -> Result<String> {
    let secp = Secp256k1::new();
    let xpriv =
        Xpriv::new_master(network, seed).map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let path = DerivationPath::from_str(&format!("m/84'/{}'/0'/0/{}", coin_type(network), index))
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let derived = xpriv
        .derive_priv(&secp, &path)
        .map_err(|e| BtcError::KeyDerivation(e.to_string()))?;
    let kind = match network {
        Network::Bitcoin => NetworkKind::Main,
        _ => NetworkKind::Test,
    };
    let key = bitcoin::PrivateKey::new(derived.private_key, kind);
    Ok(key.to_wif())
}

/// BIP44 coin type: 0' for mainnet, 1' for testnet/regtest.
fn coin_type(network: Network) -> u32 {
    match network {
        Network::Bitcoin => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_seed() -> [u8; 64] {
        [7u8; 64]
    }

    #[test]
    fn test_derive_address_is_bech32() {
        let addr = derive_address(&test_seed(), 0, Network::Bitcoin).unwrap();
        assert!(addr.starts_with("bc1q"), "got {}", addr);
    }

    #[test]
    fn test_derive_address_deterministic() {
        let a = derive_address(&test_seed(), 3, Network::Bitcoin).unwrap();
        let b = derive_address(&test_seed(), 3, Network::Bitcoin).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_derive_address_distinct_indices() {
        let a = derive_address(&test_seed(), 0, Network::Bitcoin).unwrap();
        let b = derive_address(&test_seed(), 1, Network::Bitcoin).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn test_derive_wif_roundtrip() {
        let wif = derive_wif(&test_seed(), 0, Network::Bitcoin).unwrap();
        let key = bitcoin::PrivateKey::from_wif(&wif).unwrap();
        assert!(key.compressed);
    }

    #[test]
    fn test_derive_address_regtest() {
        let addr = derive_address(&test_seed(), 0, Network::Regtest).unwrap();
        assert!(addr.starts_with("bcrt1q"), "got {}", addr);
    }
}
