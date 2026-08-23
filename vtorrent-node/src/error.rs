use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Block validation failed: {0}")]
    InvalidBlock(String),

    #[error("Transaction validation failed: {0}")]
    InvalidTransaction(String),

    /// A transaction was rejected by node relay policy (e.g. fee below the
    /// relay floor). The transaction may be valid; the peer should not be
    /// penalized for relaying it.
    #[error("Transaction rejected by relay policy: {0}")]
    PolicyRejected(String),

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

    #[error("Invalid address: {0}")]
    InvalidAddress(String),
}

pub type Result<T> = std::result::Result<T, NodeError>;
