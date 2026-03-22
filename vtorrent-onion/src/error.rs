use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnionError {
    #[error("Tor SOCKS5 proxy not available at {addr}: {source}")]
    TorUnavailable { addr: String, source: std::io::Error },

    #[error("I2P SAM bridge not available at {addr}: {source}")]
    I2pUnavailable { addr: String, source: std::io::Error },

    #[error("SOCKS5 connection failed: {0}")]
    Socks5Error(String),

    #[error("I2P SAM protocol error: {0}")]
    SamError(String),

    #[error("Invalid onion address: {0}")]
    InvalidOnionAddr(String),

    #[error("Invalid I2P destination: {0}")]
    InvalidI2pDest(String),

    #[error("Hidden service creation failed: {0}")]
    HiddenServiceError(String),

    #[error("Transport not configured: {0}")]
    NotConfigured(String),

    #[error("Connection timeout after {0}s")]
    Timeout(u64),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("All transports failed: {0}")]
    AllFailed(String),
}

pub type Result<T> = std::result::Result<T, OnionError>;
