use thiserror::Error;

#[derive(Debug, Error)]
pub enum OverlayError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("STUN error: {0}")]
    Stun(String),

    #[error("Hole punch failed: {0}")]
    HolePunch(String),

    #[error("Rendezvous error: {0}")]
    Rendezvous(String),

    #[error("Encryption error: {0}")]
    Crypto(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Timeout")]
    Timeout,

    #[error("Peer not found: {0}")]
    PeerNotFound(String),

    #[error("Key file error: {0}")]
    KeyFile(String),
}

pub type Result<T> = std::result::Result<T, OverlayError>;
