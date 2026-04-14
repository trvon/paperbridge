use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.semanticscholar.org/graph/v1";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";
const FIELDS: &str =
    "title,authors,year,externalIds,abstract,url,openAccessPdf,venue,citationCount";

#[derive(Clone)]
pub struct SemanticScholarClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl SemanticScholarClient {
    pub fn new(base_url: Option<&str>, api_key: Option<String>) -> Self {
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
                "Semantic Scholar search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/paper/search?query={encoded}&limit={limit}&fields={FIELDS}",
            self.base_url
        );

        let mut req = self.client.get(&url);
        if let Some(key) = self.api_key.as_deref() {
            req = req.header("x-api-key", key);
        }

        let response = req.send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Semantic Scholar API error at {url}"),
            });
        }

        let raw: RawS2Response = response.json().await?;
        let hits = raw
            .data
            .unwrap_or_default()
            .into_iter()
            .map(convert_paper)
            .collect();
        Ok(hits)
    }
}

impl std::fmt::Debug for SemanticScholarClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticScholarClient")
            .field("base_url", &self.base_url)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawS2Response {
    data: Option<Vec<RawS2Paper>>,
}

#[derive(Debug, Deserialize)]
struct RawS2Paper {
    title: Option<String>,
    #[serde(default)]
    authors: Vec<RawS2Author>,
    year: Option<u32>,
    #[serde(rename = "externalIds")]
    external_ids: Option<RawS2ExternalIds>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    url: Option<String>,
    #[serde(rename = "openAccessPdf")]
    open_access_pdf: Option<RawS2OpenAccessPdf>,
    venue: Option<String>,
    #[serde(rename = "citationCount")]
    citation_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawS2Author {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawS2ExternalIds {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawS2OpenAccessPdf {
    url: Option<String>,
}

fn convert_paper(p: RawS2Paper) -> PaperHit {
    let authors = p
        .authors
        .into_iter()
        .filter_map(|a| a.name)
        .filter(|n| !n.is_empty())
        .collect();

    let (doi, arxiv_id) = match p.external_ids {
        Some(ids) => (ids.doi, ids.arxiv),
        None => (None, None),
    };

    PaperHit {
        source: PaperSource::SemanticScholar,
        title: p.title.unwrap_or_default(),
        authors,
        year: p.year.map(|y| y.to_string()),
        doi,
        arxiv_id,
        abstract_note: p.abstract_text,
        url: p.url,
        pdf_url: p.open_access_pdf.and_then(|o| o.url),
        venue: p.venue.filter(|v| !v.is_empty()),
        citation_count: p.citation_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_paper_extracts_all_fields() {
        let json = serde_json::json!({
            "title": "Attention Is All You Need",
            "authors": [
                {"name": "Ashish Vaswani"},
                {"name": "Noam Shazeer"}
            ],
            "year": 2017,
            "externalIds": {
                "DOI": "10.48550/arXiv.1706.03762",
                "ArXiv": "1706.03762"
            },
            "abstract": "Transformer paper abstract.",
            "url": "https://www.semanticscholar.org/paper/abc",
            "openAccessPdf": {"url": "https://arxiv.org/pdf/1706.03762.pdf"},
            "venue": "NeurIPS",
            "citationCount": 100000
        });
        let paper: RawS2Paper = serde_json::from_value(json).unwrap();
        let hit = convert_paper(paper);
        assert_eq!(hit.title, "Attention Is All You Need");
        assert_eq!(hit.authors, vec!["Ashish Vaswani", "Noam Shazeer"]);
        assert_eq!(hit.year.as_deref(), Some("2017"));
        assert_eq!(hit.doi.as_deref(), Some("10.48550/arXiv.1706.03762"));
        assert_eq!(hit.arxiv_id.as_deref(), Some("1706.03762"));
        assert_eq!(hit.venue.as_deref(), Some("NeurIPS"));
        assert_eq!(hit.citation_count, Some(100000));
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://arxiv.org/pdf/1706.03762.pdf")
        );
        assert_eq!(hit.source, PaperSource::SemanticScholar);
    }

    #[test]
    fn convert_paper_handles_missing_fields() {
        let json = serde_json::json!({ "title": "Minimal" });
        let paper: RawS2Paper = serde_json::from_value(json).unwrap();
        let hit = convert_paper(paper);
        assert_eq!(hit.title, "Minimal");
        assert!(hit.authors.is_empty());
        assert!(hit.year.is_none());
        assert!(hit.doi.is_none());
        assert!(hit.arxiv_id.is_none());
        assert!(hit.citation_count.is_none());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = SemanticScholarClient::new(None, None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_paper_search_endpoint() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "data": [
                {
                    "title": "S2 Mock",
                    "authors": [{"name": "Jane Doe"}],
                    "year": 2024,
                    "externalIds": {"DOI": "10.1/s2", "ArXiv": "2401.0001"},
                    "abstract": "Abstract.",
                    "url": "https://www.semanticscholar.org/paper/abc",
                    "openAccessPdf": {"url": "https://arxiv.org/pdf/2401.0001.pdf"},
                    "venue": "NeurIPS",
                    "citationCount": 42
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/paper/search"))
            .and(query_param("query", "quantum"))
            .and(query_param("limit", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = SemanticScholarClient::new(Some(&server.uri()), None);
        let hits = client.search("quantum", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "S2 Mock");
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/s2"));
        assert_eq!(hits[0].arxiv_id.as_deref(), Some("2401.0001"));
        assert_eq!(hits[0].citation_count, Some(42));
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

        let client = SemanticScholarClient::new(Some(&server.uri()), None);
        let err = client.search("q", 1).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 403),
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
