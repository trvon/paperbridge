use thiserror::Error;

pub type Result<T> = std::result::Result<T, ZoteroMcpError>;

pub const MAX_ERROR_BYTES: usize = 4096;

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct RecoveryAction {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// Shared CLI/MCP error contract. Recovery actions are safe reads, never writes.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct ErrorEnvelope {
    pub error: String,
    pub reason: String,
    #[serde(rename = "try")]
    pub suggestions: Vec<String>,
    pub retryable: bool,
    pub recovery: Vec<RecoveryAction>,
}

impl ErrorEnvelope {
    pub fn from_error(error: &ZoteroMcpError) -> Self {
        let (code, reason, retryable, suggestions) = match error {
            ZoteroMcpError::Config(reason) => (
                "configuration_error",
                reason.clone(),
                false,
                vec![
                    "paperbridge config doctor".into(),
                    "paperbridge config validate".into(),
                ],
            ),
            ZoteroMcpError::MissingConfig(key) => (
                "missing_configuration",
                format!("Required configuration is missing: {key}"),
                false,
                vec![
                    "paperbridge config doctor".into(),
                    "paperbridge config validate".into(),
                ],
            ),
            ZoteroMcpError::InvalidInput(reason) => (
                "invalid_input",
                reason.clone(),
                false,
                input_suggestions(reason),
            ),
            ZoteroMcpError::Http(reason) => (
                "http_error",
                reason.clone(),
                true,
                vec![
                    "paperbridge status".into(),
                    "paperbridge config doctor".into(),
                ],
            ),
            ZoteroMcpError::Api { status, message } => (
                "upstream_api_error",
                format!("Upstream returned HTTP {status}: {message}"),
                *status == 429 || *status >= 500,
                vec![
                    "paperbridge status".into(),
                    "paperbridge config doctor".into(),
                ],
            ),
            ZoteroMcpError::Serde(reason) => (
                "serialization_error",
                reason.clone(),
                false,
                vec!["paperbridge config doctor".into()],
            ),
        };
        Self {
            error: code.into(),
            reason: sanitize_message(&reason, &[]),
            suggestions,
            retryable,
            recovery: vec![RecoveryAction {
                tool: "backend_info".into(),
                arguments: serde_json::json!({}),
            }],
        }
    }
}

fn input_suggestions(reason: &str) -> Vec<String> {
    let commands: &[&str] = if reason.contains("Unknown config key") {
        &["paperbridge config get", "paperbridge config --help"]
    } else if reason.contains("JSON corpus export requires --json") {
        &["paperbridge paperseed corpus export --json"]
    } else if reason.contains("--format bibtex conflicts with --json") {
        &[
            "paperbridge paperseed corpus export --format bibtex",
            "paperbridge paperseed corpus export --json",
        ]
    } else if reason.contains("search query") || reason.contains("Search query") {
        &["paperbridge papers search --help"]
    } else if reason.contains("selector")
        || reason.contains("fulltext")
        || reason.contains("open_paper")
    {
        &[
            "paperbridge papers open --help",
            "paperbridge papers query --help",
        ]
    } else {
        &["paperbridge --help"]
    };
    commands.iter().map(|command| (*command).into()).collect()
}

/// Redact known credentials and sensitive URL parameters before bounding UTF-8 output.
pub fn sanitize_message(message: &str, secrets: &[&str]) -> String {
    const SECRET_KEYS: &[&str] = &[
        "PAPERBRIDGE_API_KEY",
        "PAPERBRIDGE_HF_TOKEN",
        "PAPERBRIDGE_SEMANTIC_SCHOLAR_API_KEY",
        "PAPERBRIDGE_CORE_API_KEY",
        "PAPERBRIDGE_ADS_API_TOKEN",
        "PAPERBRIDGE_NCBI_API_KEY",
        "PAPERBRIDGE_SCHOLARAPI_KEY",
        "ZOTERO_MCP_API_KEY",
        "HF_TOKEN",
        "SEMANTIC_SCHOLAR_API_KEY",
        "CORE_API_KEY",
        "ADS_API_TOKEN",
        "NCBI_API_KEY",
        "SCHOLARAPI_KEY",
    ];
    let environment: Vec<String> = SECRET_KEYS
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .collect();
    let mut result = message.to_string();
    for secret in secrets
        .iter()
        .copied()
        .chain(environment.iter().map(String::as_str))
        .filter(|s| !s.is_empty())
    {
        result = result.replace(secret, "<redacted>");
        result = result.replace(urlencoding::encode(secret).as_ref(), "<redacted>");
    }
    for key in [
        "api_key=",
        "apikey=",
        "access_token=",
        "token=",
        "password=",
        "secret=",
        "signature=",
        "key=",
    ] {
        let mut start = 0;
        while let Some(relative) = result[start..].to_ascii_lowercase().find(key) {
            let value_start = start + relative + key.len();
            let value_end = result[value_start..]
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '&' | '#' | '\"' | '\'' | '<' | '>')
                })
                .map_or(result.len(), |end| value_start + end);
            if value_end > value_start {
                result.replace_range(value_start..value_end, "<redacted>");
                start = value_start + "<redacted>".len();
            } else {
                start = value_start;
            }
        }
    }
    // URL userinfo can contain credentials even when no configured secret matches.
    let tokens: Vec<String> = result.split_whitespace().map(str::to_string).collect();
    for token in tokens {
        if let Some(at) = token.find("://").and_then(|scheme| {
            token[scheme + 3..]
                .find('@')
                .map(|at| (scheme + 3, scheme + 3 + at))
        }) {
            result = result.replace(&token[at.0..at.1], "<redacted>");
        }
    }
    if result.len() > MAX_ERROR_BYTES {
        let mut end = MAX_ERROR_BYTES - " [truncated]".len();
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str(" [truncated]");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_redacts_before_bounding_unicode_errors() {
        let message = format!(
            "https://user:pass@example.org/x?api_key=urlsecret&x=ok known-secret {}",
            "é".repeat(5000)
        );
        let safe = sanitize_message(&message, &["known-secret"]);
        for secret in ["known-secret", "urlsecret", "user:pass"] {
            assert!(!safe.contains(secret));
        }
        assert!(safe.len() <= MAX_ERROR_BYTES);
        assert!(safe.ends_with("[truncated]"));
    }

    #[test]
    fn recovery_is_structured_and_does_not_execute_writes() {
        let envelope = ErrorEnvelope::from_error(&ZoteroMcpError::Api {
            status: 429,
            message: "slow down".into(),
        });
        assert!(envelope.retryable);
        assert_eq!(envelope.recovery[0].tool, "backend_info");
        assert_eq!(envelope.recovery[0].arguments, serde_json::json!({}));
    }
}

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
