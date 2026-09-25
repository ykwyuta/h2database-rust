use thiserror::Error;

#[derive(Error, Debug)]
pub enum H2Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Type error: {0}")]
    TypeError(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("SQL parse error: {0}")]
    SqlParse(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Catalog error: {0}")]
    Catalog(String),

    #[error("Lock conflict: {0}")]
    LockConflict(String),

    #[error("Query timeout: {0}")]
    QueryTimeout(String),

    #[error("Corrupted database file: {0}")]
    Corrupted(String),

    #[error("Read-only transaction: {0}")]
    ReadOnly(String),

    #[error("Replication error: {0}")]
    Replication(String),

    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Offset out of range: {0}")]
    OffsetOutOfRange(String),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}

pub type H2Result<T> = Result<T, H2Error>;
