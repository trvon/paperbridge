use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PaperseedError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("path is not a file: {0}")]
    NotAFile(PathBuf),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("paper not found: {0}")]
    PaperNotFound(String),

    #[error("license policy blocks this action: {reason}")]
    PolicyBlocked { reason: String },
}

pub type Result<T> = std::result::Result<T, PaperseedError>;
