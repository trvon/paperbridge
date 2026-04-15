use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api2.openreview.net";
const PDF_HOST: &str = "https://openreview.net";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct OpenReviewClient {
    client: Client,
    base_url: String,
    pdf_host: String,
}

impl OpenReviewClient {
    pub fn new(base_url: Option<&str>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build reqwest client");

        let base = base_url
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        // For tests, point pdf_host at the same mock server when override given.
        let pdf_host = if base_url.is_some() {
            base.clone()
        } else {
            PDF_HOST.to_string()
        };

        Self {
            client,
            base_url: base,
            pdf_host,
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "OpenReview search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/notes/search?term={encoded}&limit={limit}&type=all",
            self.base_url
        );

        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("OpenReview API error at {url}"),
            });
        }

        let raw: RawOrResponse = response.json().await?;
        Ok(raw
            .notes
            .into_iter()
            .map(|n| convert_note(n, &self.pdf_host))
            .collect())
    }
}

impl std::fmt::Debug for OpenReviewClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenReviewClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawOrResponse {
    #[serde(default)]
    notes: Vec<RawOrNote>,
}

#[derive(Debug, Deserialize)]
struct RawOrNote {
    id: Option<String>,
    content: Option<RawOrContent>,
}

#[derive(Debug, Deserialize)]
struct RawOrContent {
    title: Option<RawOrValue<String>>,
    authors: Option<RawOrValue<Vec<String>>>,
    #[serde(rename = "abstract")]
    abstract_text: Option<RawOrValue<String>>,
    pdf: Option<RawOrValue<String>>,
    venue: Option<RawOrValue<String>>,
}

#[derive(Debug, Deserialize)]
struct RawOrValue<T> {
    value: Option<T>,
}

fn convert_note(n: RawOrNote, pdf_host: &str) -> PaperHit {
    let id = n.id.clone();
    let content = n.content.unwrap_or(RawOrContent {
        title: None,
        authors: None,
        abstract_text: None,
        pdf: None,
        venue: None,
    });

    let title = content.title.and_then(|v| v.value).unwrap_or_default();
    let authors = content
        .authors
        .and_then(|v| v.value)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !a.is_empty())
        .collect();
    let abstract_note = content.abstract_text.and_then(|v| v.value);
    let venue = content.venue.and_then(|v| v.value);

    let pdf_url = content
        .pdf
        .and_then(|v| v.value)
        .map(|p| absolutize_pdf(&p, pdf_host));
    let url = id.as_ref().map(|i| format!("{pdf_host}/forum?id={i}"));

    PaperHit {
        source: PaperSource::OpenReview,
        title,
        authors,
        year: None,
        doi: None,
        arxiv_id: None,
        pmid: None,
        abstract_note,
        url,
        pdf_url: pdf_url.clone(),
        oa_pdf_url: pdf_url,
        venue,
        citation_count: None,
    }
}

fn absolutize_pdf(p: &str, host: &str) -> String {
    if p.starts_with("http://") || p.starts_with("https://") {
        p.to_string()
    } else if let Some(stripped) = p.strip_prefix('/') {
        format!("{host}/{stripped}")
    } else {
        format!("{host}/{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_note_extracts_fields() {
        let json = serde_json::json!({
            "id": "abc123",
            "content": {
                "title": {"value": "OpenReview Paper"},
                "authors": {"value": ["Alice", "Bob"]},
                "abstract": {"value": "An abstract."},
                "pdf": {"value": "/pdf?id=abc123"},
                "venue": {"value": "ICLR 2024"}
            }
        });
        let n: RawOrNote = serde_json::from_value(json).unwrap();
        let hit = convert_note(n, "https://openreview.net");
        assert_eq!(hit.title, "OpenReview Paper");
        assert_eq!(hit.authors, vec!["Alice", "Bob"]);
        assert_eq!(hit.venue.as_deref(), Some("ICLR 2024"));
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://openreview.net/pdf?id=abc123")
        );
        assert_eq!(
            hit.url.as_deref(),
            Some("https://openreview.net/forum?id=abc123")
        );
        assert_eq!(hit.source, PaperSource::OpenReview);
    }

    #[test]
    fn convert_note_handles_missing_fields() {
        let json = serde_json::json!({"id": "x"});
        let n: RawOrNote = serde_json::from_value(json).unwrap();
        let hit = convert_note(n, "https://openreview.net");
        assert_eq!(hit.title, "");
        assert!(hit.authors.is_empty());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = OpenReviewClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_notes_search() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "notes": [{
                "id": "p1",
                "content": {
                    "title": {"value": "OR Mock"},
                    "authors": {"value": ["Jane"]},
                    "pdf": {"value": "/pdf?id=p1"}
                }
            }]
        });
        Mock::given(method("GET"))
            .and(path("/notes/search"))
            .and(query_param("term", "transformers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = OpenReviewClient::new(Some(&server.uri()));
        let hits = client.search("transformers", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "OR Mock");
        assert!(
            hits[0]
                .pdf_url
                .as_deref()
                .map(|u| u.ends_with("/pdf?id=p1"))
                .unwrap_or(false)
        );
    }
}
