use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CourierError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid transfer state transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
    #[error("source changed after inventory: {0}")]
    SourceModified(PathBuf),
    #[error("path is not a file or directory: {0}")]
    InvalidSource(PathBuf),
    #[error("transfer not found: {0}")]
    TransferNotFound(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("system clock is earlier than Unix epoch")]
    InvalidSystemTime,
    #[error("path cannot be represented relative to source root: {0}")]
    InvalidRelativePath(PathBuf),
    #[error("could not traverse source path {path}: {message}")]
    SourceTraversal { path: PathBuf, message: String },
}

pub type Result<T> = std::result::Result<T, CourierError>;
