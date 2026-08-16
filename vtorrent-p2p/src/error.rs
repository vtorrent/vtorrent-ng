use thiserror::Error;

#[derive(Debug, Error)]
pub enum P2pError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Connection refused by peer: {0}")]
    ConnectionRefused(String),

    #[error("Anonymous transport error: {0}")]
    Transport(String),

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Message decode error: {0}")]
    Decode(String),

    #[error("Peer disconnected")]
    Disconnected,

    #[error("Too many peers (max: {0})")]
    TooManyPeers(usize),

    #[error("Peer is banned: {0}")]
    Banned(String),
}

pub type Result<T> = std::result::Result<T, P2pError>;
