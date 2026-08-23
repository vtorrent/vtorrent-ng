use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A BIP39 mnemonic phrase, zeroized on drop.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct Mnemonic {
    words: zeroize::Zeroizing<String>,
}

/// HD account metadata stored in the wallet.
///
/// The `Debug` impl redacts the mnemonic so the seed phrase is never logged.
/// The mnemonic is stored in a `Zeroizing<String>` to ensure it is zeroed on
/// drop, preventing seed phrase recovery from freed heap memory.
#[derive(Clone, Serialize, Deserialize)]
pub struct HdAccount {
    /// The BIP39 mnemonic phrase (space-separated words).
    pub mnemonic: zeroize::Zeroizing<String>,
    /// Word count (12 or 24).
    pub word_count: usize,
    /// Unix timestamp when HD was enabled.
    pub created_at: u64,
}

impl std::fmt::Debug for HdAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HdAccount")
            .field("mnemonic", &"[REDACTED]")
            .field("word_count", &self.word_count)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl Mnemonic {
    /// Generate a new 24-word mnemonic.
    pub fn generate() -> crate::error::Result<Self> {
        use bip39::{Language, Mnemonic as Bip39Mnemonic};
        let m = Bip39Mnemonic::generate_in(Language::English, 24)
            .map_err(|e| crate::error::WalletError::MnemonicError(e.to_string()))?;
        Ok(Self {
            words: zeroize::Zeroizing::new(m.to_string()),
        })
    }

    /// Parse a mnemonic from a phrase string.
    pub fn from_phrase(phrase: &str) -> crate::error::Result<Self> {
        use bip39::Mnemonic as Bip39Mnemonic;
        Bip39Mnemonic::parse_in_normalized(bip39::Language::English, phrase)
            .map_err(|e| crate::error::WalletError::MnemonicError(e.to_string()))?;
        Ok(Self {
            words: zeroize::Zeroizing::new(phrase.to_string()),
        })
    }

    /// The mnemonic phrase as a string.
    pub fn phrase(&self) -> &str {
        &self.words
    }

    /// Derive the 64-byte BIP39 seed (empty passphrase).
    pub fn to_seed(&self) -> crate::error::Result<[u8; 64]> {
        use bip39::Mnemonic as Bip39Mnemonic;
        let m = Bip39Mnemonic::parse_in_normalized(bip39::Language::English, &self.words)
            .map_err(|e| crate::error::WalletError::MnemonicError(e.to_string()))?;
        Ok(m.to_seed(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_24_words() {
        let m = Mnemonic::generate().unwrap();
        assert_eq!(m.phrase().split_whitespace().count(), 24);
    }

    #[test]
    fn test_seed_is_64_bytes() {
        let m = Mnemonic::generate().unwrap();
        assert_eq!(m.to_seed().unwrap().len(), 64);
    }

    #[test]
    fn test_roundtrip_phrase() {
        let m = Mnemonic::generate().unwrap();
        let parsed = Mnemonic::from_phrase(m.phrase()).unwrap();
        assert_eq!(parsed.phrase(), m.phrase());
    }
}
