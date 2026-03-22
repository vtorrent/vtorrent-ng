use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("redb error: {0}")]
    Redb(#[from] redb::Error),

    #[error("redb database error: {0}")]
    RedbDatabase(#[from] redb::DatabaseError),

    #[error("redb transaction error: {0}")]
    RedbTransaction(#[from] redb::TransactionError),

    #[error("redb table error: {0}")]
    RedbTable(#[from] redb::TableError),

    #[error("redb commit error: {0}")]
    RedbCommit(#[from] redb::CommitError),

    #[error("redb storage error: {0}")]
    RedbStorage(#[from] redb::StorageError),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("block not found: {0}")]
    BlockNotFound(String),

    #[error("store is corrupted: {0}")]
    Corrupted(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;
