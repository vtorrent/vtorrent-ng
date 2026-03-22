use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpvError {
    #[error("Invalid Merkle proof: {0}")]
    InvalidMerkleProof(String),

    #[error("Block header validation failed: {0}")]
    HeaderValidation(String),

    #[error("Chain height mismatch: expected {expected}, got {got}")]
    HeightMismatch { expected: u32, got: u32 },

    #[error("Unknown parent block: {0}")]
    UnknownParent(String),

    #[error("Filter decode error: {0}")]
    FilterDecode(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SpvError>;
