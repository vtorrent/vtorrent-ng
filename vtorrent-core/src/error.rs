use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("Invalid private key bytes")]
    InvalidPrivateKey,

    #[error("Invalid public key bytes")]
    InvalidPublicKey,

    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Invalid WIF key: {0}")]
    InvalidWif(String),

    #[error("Checksum mismatch")]
    ChecksumMismatch,

    #[error("Secp256k1 error: {0}")]
    Secp256k1(#[from] secp256k1::Error),

    #[error("Hex decode error: {0}")]
    HexDecode(#[from] hex::FromHexError),
}

pub type Result<T> = std::result::Result<T, CoreError>;
