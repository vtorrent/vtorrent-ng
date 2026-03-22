use thiserror::Error;

#[derive(Debug, Error)]
pub enum TorrentError {
    #[error("Invalid torrent file: {0}")]
    InvalidMetainfo(String),

    #[error("Tracker error: {0}")]
    TrackerError(String),

    #[error("Peer wire protocol error: {0}")]
    PeerWireError(String),

    #[error("Incentive error: {0}")]
    IncentiveError(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("Bencode decode error: {0}")]
    BencodeError(String),

    #[error("HTTP error: {0}")]
    HttpError(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Magnet link parse error: {0}")]
    MagnetError(String),
}

pub type Result<T> = std::result::Result<T, TorrentError>;

impl From<std::io::Error> for TorrentError {
    fn from(e: std::io::Error) -> Self {
        TorrentError::Io(e.to_string())
    }
}

impl From<reqwest::Error> for TorrentError {
    fn from(e: reqwest::Error) -> Self {
        TorrentError::HttpError(e.to_string())
    }
}
