use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Wallet locked")]
    WalletLocked,

    #[error("Node error: {0}")]
    NodeError(String),
}

impl IntoResponse for RpcError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            RpcError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            RpcError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            RpcError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg.clone()),
            RpcError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg.clone()),
            RpcError::WalletLocked => (StatusCode::FORBIDDEN, "Wallet is locked".into()),
            RpcError::Internal(msg) | RpcError::NodeError(msg) => {
                tracing::error!("Internal RPC error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".into(),
                )
            }
        };

        let body = Json(json!({
            "error": true,
            "message": message,
            "code": status.as_u16()
        }));

        (status, body).into_response()
    }
}

pub type RpcResult<T> = std::result::Result<T, RpcError>;

impl From<vtorrent_node::error::NodeError> for RpcError {
    fn from(e: vtorrent_node::error::NodeError) -> Self {
        tracing::error!("Node error: {}", e);
        RpcError::NodeError(format!("Node operation failed: {}", e))
    }
}

impl From<vtorrent_wallet::error::WalletError> for RpcError {
    fn from(e: vtorrent_wallet::error::WalletError) -> Self {
        match e {
            vtorrent_wallet::error::WalletError::WalletLocked => RpcError::WalletLocked,
            vtorrent_wallet::error::WalletError::IncorrectPassphrase => {
                RpcError::Unauthorized("Incorrect passphrase — wallet decryption failed".into())
            }
            other => {
                tracing::error!("Wallet error: {}", other);
                RpcError::Internal(format!("Wallet operation failed: {}", other))
            }
        }
    }
}

impl From<vtorrent_btc::error::BtcError> for RpcError {
    fn from(e: vtorrent_btc::error::BtcError) -> Self {
        tracing::error!("BTC error: {}", e);
        RpcError::Internal(format!("Bitcoin operation failed: {}", e))
    }
}
