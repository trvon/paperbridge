use crate::error::{Result, ZoteroMcpError};

pub fn ensure_secure_transport(url: &str) -> Result<()> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ZoteroMcpError::Config(format!("invalid URL '{url}': {e}")))?;

    if parsed.scheme() == "https" {
        return Ok(());
    }

    if parsed.scheme() == "http"
        && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
    {
        return Ok(());
    }

    Err(ZoteroMcpError::Config(format!(
        "refusing to send API credentials over non-HTTPS URL '{url}' (only http://localhost is allowed for local API mode)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https() {
        ensure_secure_transport("https://api.zotero.org").unwrap();
    }

    #[test]
    fn accepts_localhost_http() {
        ensure_secure_transport("http://127.0.0.1:23119/api").unwrap();
        ensure_secure_transport("http://localhost:23119/api").unwrap();
    }

    #[test]
    fn rejects_plain_http_remote() {
        assert!(ensure_secure_transport("http://api.zotero.org").is_err());
    }

    #[test]
    fn rejects_malformed_url() {
        assert!(ensure_secure_transport("not a url").is_err());
    }
}
