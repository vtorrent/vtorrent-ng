use thiserror::Error;

#[derive(Debug, Error)]
pub enum BtcError {
    #[error("Bitcoin error: {0}")]
    Bitcoin(String),

    #[error("Key derivation error: {0}")]
    KeyDerivation(String),

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Insufficient funds: available {available} sats, required {required} sats")]
    InsufficientFunds { available: u64, required: u64 },

    #[error("Not synced")]
    NotSynced,

    #[error("P2P error: {0}")]
    P2p(String),

    #[error("Wallet error: {0}")]
    Wallet(#[from] vtorrent_wallet::error::WalletError),
}

pub type Result<T> = std::result::Result<T, BtcError>;
