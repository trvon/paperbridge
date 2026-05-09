use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode error: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("index schema mismatch: expected {expected}, found {actual}")]
    SchemaMismatch { expected: u32, actual: u32 },
}

pub type Result<T> = std::result::Result<T, IndexError>;
