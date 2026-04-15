use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.core.ac.uk/v3";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct CoreClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl CoreClient {
    pub fn new(base_url: Option<&str>, api_key: String) -> Self {
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
            api_key,
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "CORE search query must not be empty".to_string(),
            ));
        }

        let url = format!("{}/search/works", self.base_url);
        let body = serde_json::json!({"q": trimmed, "limit": limit});

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("CORE API error at {url}"),
            });
        }

        let raw: RawCoreResponse = response.json().await?;
        Ok(raw
            .results
            .unwrap_or_default()
            .into_iter()
            .map(convert_work)
            .collect())
    }
}

impl std::fmt::Debug for CoreClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawCoreResponse {
    results: Option<Vec<RawCoreWork>>,
}

#[derive(Debug, Deserialize)]
struct RawCoreWork {
    title: Option<String>,
    doi: Option<String>,
    #[serde(rename = "yearPublished")]
    year_published: Option<u32>,
    #[serde(default)]
    authors: Vec<RawCoreAuthor>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(rename = "downloadUrl")]
    download_url: Option<String>,
    #[serde(rename = "fullTextLink")]
    full_text_link: Option<String>,
    publisher: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCoreAuthor {
    name: Option<String>,
}

fn convert_work(w: RawCoreWork) -> PaperHit {
    let authors = w
        .authors
        .into_iter()
        .filter_map(|a| a.name)
        .filter(|n| !n.is_empty())
        .collect();

    let pdf = w.download_url.or(w.full_text_link);

    PaperHit {
        source: PaperSource::Core,
        title: w.title.unwrap_or_default(),
        authors,
        year: w.year_published.map(|y| y.to_string()),
        doi: w.doi,
        arxiv_id: None,
        pmid: None,
        abstract_note: w.abstract_text,
        url: None,
        pdf_url: pdf.clone(),
        oa_pdf_url: pdf,
        venue: w.publisher,
        citation_count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_work_extracts_fields() {
        let json = serde_json::json!({
            "title": "CORE Paper",
            "doi": "10.1/core",
            "yearPublished": 2023,
            "authors": [{"name": "Alice"}, {"name": "Bob"}],
            "abstract": "Abs.",
            "downloadUrl": "https://core.ac.uk/x.pdf",
            "publisher": "Springer"
        });
        let w: RawCoreWork = serde_json::from_value(json).unwrap();
        let hit = convert_work(w);
        assert_eq!(hit.title, "CORE Paper");
        assert_eq!(hit.authors, vec!["Alice", "Bob"]);
        assert_eq!(hit.doi.as_deref(), Some("10.1/core"));
        assert_eq!(hit.year.as_deref(), Some("2023"));
        assert_eq!(hit.pdf_url.as_deref(), Some("https://core.ac.uk/x.pdf"));
        assert_eq!(hit.oa_pdf_url.as_deref(), Some("https://core.ac.uk/x.pdf"));
        assert_eq!(hit.venue.as_deref(), Some("Springer"));
        assert_eq!(hit.source, PaperSource::Core);
    }

    #[test]
    fn convert_work_handles_missing_fields() {
        let json = serde_json::json!({"title": "Minimal"});
        let w: RawCoreWork = serde_json::from_value(json).unwrap();
        let hit = convert_work(w);
        assert_eq!(hit.title, "Minimal");
        assert!(hit.authors.is_empty());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = CoreClient::new(None, "key".to_string());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_posts_to_search_works() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "results": [{
                "title": "CORE Mock",
                "doi": "10.1/core",
                "yearPublished": 2024,
                "authors": [{"name": "Jane"}]
            }]
        });
        Mock::given(method("POST"))
            .and(path("/search/works"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = CoreClient::new(Some(&server.uri()), "secret".to_string());
        let hits = client.search("graphs", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "CORE Mock");
    }

    #[tokio::test]
    async fn search_surfaces_auth_error_on_401() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let client = CoreClient::new(Some(&server.uri()), "bad".to_string());
        let err = client.search("q", 5).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 401),
            other => panic!("expected 401 Api error, got {other:?}"),
        }
    }
}
