use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error, Serialize)]
#[serde(tag = "type", content = "message")]
pub enum TauriError {
    #[error("Wallet migration error: {0}")]
    Migration(String),

    #[error("Wallet error: {0}")]
    Wallet(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Wallet is locked")]
    WalletLocked,

    #[error("Wallet not initialized")]
    WalletNotInitialized,

    #[error("2FA verification failed")]
    TwoFAFailed,

    #[error("IO error: {0}")]
    Io(String),

    #[error("Node error: {0}")]
    NodeError(String),

    #[error("Torrent error: {0}")]
    Torrent(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<vtorrent_migrate::error::MigrateError> for TauriError {
    fn from(e: vtorrent_migrate::error::MigrateError) -> Self {
        TauriError::Migration(e.to_string())
    }
}

impl From<vtorrent_wallet::error::WalletError> for TauriError {
    fn from(e: vtorrent_wallet::error::WalletError) -> Self {
        TauriError::Wallet(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TauriError>;
