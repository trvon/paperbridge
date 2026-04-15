use crate::error::{Result, ZoteroMcpError};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.unpaywall.org/v2";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct UnpaywallClient {
    client: Client,
    base_url: String,
    email: String,
}

impl UnpaywallClient {
    pub fn new(base_url: Option<&str>, email: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
            email,
        }
    }

    pub async fn lookup(&self, doi: &str) -> Result<Option<String>> {
        let trimmed = doi.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "Unpaywall lookup requires a non-empty DOI".to_string(),
            ));
        }

        let encoded_email = urlencoding::encode(&self.email);
        let url = format!("{}/{trimmed}?email={encoded_email}", self.base_url);

        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Unpaywall API error at {url}"),
            });
        }

        let raw: RawUnpaywallResponse = response.json().await?;
        Ok(raw.best_oa_location.and_then(|loc| loc.url_for_pdf))
    }
}

impl std::fmt::Debug for UnpaywallClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnpaywallClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawUnpaywallResponse {
    best_oa_location: Option<RawUnpaywallLocation>,
}

#[derive(Debug, Deserialize)]
struct RawUnpaywallLocation {
    url_for_pdf: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_rejects_empty_doi() {
        let client = UnpaywallClient::new(None, "x@example.com".to_string());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.lookup("  ")).unwrap_err();
        assert!(err.to_string().contains("non-empty DOI"));
    }

    #[tokio::test]
    async fn lookup_returns_oa_pdf_url() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "best_oa_location": {"url_for_pdf": "https://oa.example/x.pdf"}
        });
        Mock::given(method("GET"))
            .and(path("/10.1/abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = UnpaywallClient::new(Some(&server.uri()), "x@example.com".to_string());
        let pdf = client.lookup("10.1/abc").await.unwrap();
        assert_eq!(pdf.as_deref(), Some("https://oa.example/x.pdf"));
    }

    #[tokio::test]
    async fn lookup_returns_none_for_404() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = UnpaywallClient::new(Some(&server.uri()), "x@example.com".to_string());
        let pdf = client.lookup("10.1/missing").await.unwrap();
        assert!(pdf.is_none());
    }
}
