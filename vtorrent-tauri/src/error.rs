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

    #[error("BTC repair pending: {0}")]
    BtcRepairPending(String),

    #[error("BTC assisted recovery required: {0}")]
    BtcAssistedRecovery(String),

    #[error("BTC state missing: {0}")]
    BtcStateMissing(String),

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

impl From<vtorrent_rpc::error::RpcError> for TauriError {
    fn from(error: vtorrent_rpc::error::RpcError) -> Self {
        use vtorrent_rpc::error::RpcError;
        match error {
            RpcError::NotFound(message) => Self::NotFound(message),
            RpcError::BadRequest(message) => Self::InvalidInput(message),
            RpcError::Unauthorized(message) | RpcError::Forbidden(message) => {
                Self::Unauthorized(message)
            }
            RpcError::Internal(message) => Self::Internal(message),
            RpcError::WalletLocked => Self::WalletLocked,
            RpcError::NodeError(message) => Self::NodeError(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btc_recovery_errors_serialize_with_stable_type_tags() {
        for (error, tag) in [
            (
                TauriError::BtcRepairPending("pending".into()),
                "BtcRepairPending",
            ),
            (
                TauriError::BtcAssistedRecovery("conflict".into()),
                "BtcAssistedRecovery",
            ),
            (
                TauriError::BtcStateMissing("missing".into()),
                "BtcStateMissing",
            ),
        ] {
            let value = serde_json::to_value(error).unwrap();
            assert_eq!(value["type"], tag);
            assert!(value["message"].is_string());
        }
    }
}
