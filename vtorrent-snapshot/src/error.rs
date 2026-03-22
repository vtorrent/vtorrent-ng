use thiserror::Error;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("LevelDB error: {0}")]
    LevelDb(String),

    #[error("Block parse error: {0}")]
    BlockParse(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Invalid UTXO data: {0}")]
    InvalidUtxo(String),

    #[error("Snapshot integrity check failed: expected {expected}, got {actual}")]
    IntegrityFailed { expected: String, actual: String },

    #[error("Blockchain data directory not found: {0}")]
    DataDirNotFound(String),

    #[error("No UTXOs found — is the blockchain fully synced?")]
    NoUtxosFound,
}

pub type Result<T> = std::result::Result<T, SnapshotError>;
