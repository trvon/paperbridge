use thiserror::Error;

pub type Result<T> = std::result::Result<T, ZoteroMcpError>;

#[derive(Debug, Error)]
pub enum ZoteroMcpError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("missing required configuration: {0}")]
    MissingConfig(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("http request failed: {0}")]
    Http(String),

    #[error("zotero api error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("serialization error: {0}")]
    Serde(String),
}

impl From<reqwest::Error> for ZoteroMcpError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value.to_string())
    }
}

impl From<serde_json::Error> for ZoteroMcpError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value.to_string())
    }
}

impl From<toml::de::Error> for ZoteroMcpError {
    fn from(value: toml::de::Error) -> Self {
        Self::Config(value.to_string())
    }
}

impl From<toml::ser::Error> for ZoteroMcpError {
    fn from(value: toml::ser::Error) -> Self {
        Self::Config(value.to_string())
    }
}
