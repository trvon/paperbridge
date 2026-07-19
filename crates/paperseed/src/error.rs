use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum PaperseedError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("path is not a file: {0}")]
    NotAFile(PathBuf),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("corrupt corpus database {path} was quarantined at {quarantine}: {source}")]
    CorruptCorpus {
        path: PathBuf,
        quarantine: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Unpaywall requires a real contact email; configure unpaywall_email")]
    MissingResolverEmail,

    #[error("paper not found: {0}")]
    PaperNotFound(String),

    #[error("paper identifier must not be empty")]
    EmptyPaperId,

    #[error("paper identifier '{input}' is ambiguous; candidates: {candidates}")]
    AmbiguousPaperId { input: String, candidates: String },

    #[error("stored file integrity check failed for {path}: expected {expected}, found {actual}")]
    IntegrityMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("license policy blocks this action: {reason}")]
    PolicyBlocked { reason: String },
}

pub type Result<T> = std::result::Result<T, PaperseedError>;
