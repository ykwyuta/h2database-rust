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
}

pub type H2Result<T> = Result<T, H2Error>;
