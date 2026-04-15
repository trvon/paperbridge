use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://huggingface.co/api";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct HuggingFaceClient {
    client: Client,
    base_url: String,
    token: Option<String>,
}

impl HuggingFaceClient {
    pub fn new(base_url: Option<&str>, token: Option<String>) -> Self {
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
            token,
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "HuggingFace search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!("{}/papers/search?q={encoded}", self.base_url);

        let mut req = self.client.get(&url);
        if let Some(token) = self.token.as_deref() {
            req = req.bearer_auth(token);
        }

        let response = send_with_retry(req).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("HuggingFace API error at {url}"),
            });
        }

        let raw: Vec<RawHfPaperEntry> = response.json().await?;
        let hits = raw
            .into_iter()
            .take(limit as usize)
            .map(convert_entry)
            .collect();
        Ok(hits)
    }
}

impl std::fmt::Debug for HuggingFaceClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HuggingFaceClient")
            .field("base_url", &self.base_url)
            .field("has_token", &self.token.is_some())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawHfPaperEntry {
    paper: RawHfPaper,
}

#[derive(Debug, Deserialize)]
struct RawHfPaper {
    id: Option<String>,
    title: Option<String>,
    #[serde(default)]
    authors: Vec<RawHfAuthor>,
    #[serde(rename = "publishedAt")]
    published_at: Option<String>,
    summary: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawHfAuthor {
    name: Option<String>,
}

fn convert_entry(entry: RawHfPaperEntry) -> PaperHit {
    let paper = entry.paper;
    let authors = paper
        .authors
        .into_iter()
        .filter_map(|a| a.name)
        .filter(|n| !n.is_empty())
        .collect();

    let year = paper.published_at.as_deref().and_then(|p| {
        p.split('-').next().and_then(|s| {
            if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
                Some(s.to_string())
            } else {
                None
            }
        })
    });

    let url = paper
        .id
        .as_ref()
        .map(|id| format!("https://huggingface.co/papers/{id}"));
    let pdf_url = paper
        .id
        .as_ref()
        .map(|id| format!("https://arxiv.org/pdf/{id}"));

    PaperHit {
        source: PaperSource::HuggingFace,
        title: paper.title.unwrap_or_default(),
        authors,
        year,
        doi: None,
        arxiv_id: paper.id,
        pmid: None,
        abstract_note: paper.summary,
        url,
        pdf_url,
        oa_pdf_url: None,
        venue: Some("HuggingFace Papers".to_string()),
        citation_count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_entry_extracts_fields() {
        let json = serde_json::json!({
            "paper": {
                "id": "2301.00001",
                "title": "Sample HF Paper",
                "authors": [
                    {"name": "Alice Smith"},
                    {"name": "Bob Jones"}
                ],
                "publishedAt": "2023-01-02T10:00:00Z",
                "summary": "HF abstract."
            }
        });
        let entry: RawHfPaperEntry = serde_json::from_value(json).unwrap();
        let hit = convert_entry(entry);
        assert_eq!(hit.title, "Sample HF Paper");
        assert_eq!(hit.authors, vec!["Alice Smith", "Bob Jones"]);
        assert_eq!(hit.year.as_deref(), Some("2023"));
        assert_eq!(hit.arxiv_id.as_deref(), Some("2301.00001"));
        assert_eq!(hit.abstract_note.as_deref(), Some("HF abstract."));
        assert_eq!(
            hit.url.as_deref(),
            Some("https://huggingface.co/papers/2301.00001")
        );
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://arxiv.org/pdf/2301.00001")
        );
        assert_eq!(hit.source, PaperSource::HuggingFace);
    }

    #[test]
    fn convert_entry_handles_missing_fields() {
        let json = serde_json::json!({ "paper": { "title": "Only Title" } });
        let entry: RawHfPaperEntry = serde_json::from_value(json).unwrap();
        let hit = convert_entry(entry);
        assert_eq!(hit.title, "Only Title");
        assert!(hit.authors.is_empty());
        assert!(hit.year.is_none());
        assert!(hit.arxiv_id.is_none());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = HuggingFaceClient::new(None, None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_papers_search_and_parses() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!([
            {
                "paper": {
                    "id": "2401.12345",
                    "title": "HF Mock",
                    "authors": [{"name": "Jane Doe"}],
                    "publishedAt": "2024-01-05T10:00:00Z",
                    "summary": "Abstract."
                }
            }
        ]);
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .and(query_param("q", "quantum"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = HuggingFaceClient::new(Some(&server.uri()), None);
        let hits = client.search("quantum", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "HF Mock");
        assert_eq!(hits[0].arxiv_id.as_deref(), Some("2401.12345"));
    }

    #[tokio::test]
    async fn search_slices_to_limit() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!([
            {"paper": {"id": "1", "title": "A"}},
            {"paper": {"id": "2", "title": "B"}},
            {"paper": {"id": "3", "title": "C"}}
        ]);
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = HuggingFaceClient::new(Some(&server.uri()), None);
        let hits = client.search("q", 2).await.unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn search_returns_api_error_on_non_success_status() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = HuggingFaceClient::new(Some(&server.uri()), None);
        let err = client.search("q", 1).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 403),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_retries_on_429_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"paper": {"id": "2401.99999", "title": "Retried HF"}}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = HuggingFaceClient::new(Some(&server.uri()), None);
        let hits = client.search("q", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Retried HF");
    }
}
