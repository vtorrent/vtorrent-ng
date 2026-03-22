use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Block validation failed: {0}")]
    InvalidBlock(String),

    #[error("Transaction validation failed: {0}")]
    InvalidTransaction(String),

    #[error("Chain error: {0}")]
    Chain(String),

    #[error("Genesis block mismatch")]
    GenesisMismatch,

    #[error("Insufficient funds: need {need}, have {have}")]
    InsufficientFunds { need: u64, have: u64 },

    #[error("Double spend detected")]
    DoubleSpend,

    #[error("Claim already processed for address: {0}")]
    ClaimAlreadyProcessed(String),

    #[error("Invalid legacy claim: {0}")]
    InvalidClaim(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Atomic swap error: {0}")]
    AtomicSwap(String),
}

pub type Result<T> = std::result::Result<T, NodeError>;
