use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://scholarapi.net/api/v1";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct ScholarApiClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl ScholarApiClient {
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
                "ScholarAPI search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!("{}/search?q={encoded}&limit={limit}", self.base_url);
        let response =
            send_with_retry(self.client.get(&url).header("X-API-Key", &self.api_key)).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("ScholarAPI error at {url}"),
            });
        }

        let raw: RawScholarApiResponse = response.json().await?;
        Ok(raw.results.into_iter().map(convert_work).collect())
    }
}

impl std::fmt::Debug for ScholarApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScholarApiClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawScholarApiResponse {
    #[serde(default)]
    results: Vec<RawScholarApiWork>,
}

#[derive(Debug, Deserialize)]
struct RawScholarApiWork {
    title: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    doi: Option<String>,
    #[serde(alias = "published_date", alias = "publication_date")]
    published_date: Option<String>,
    year: Option<u32>,
    #[serde(alias = "abstract", alias = "summary")]
    abstract_text: Option<String>,
    url: Option<String>,
    #[serde(alias = "pdf_url")]
    pdf_url: Option<String>,
    #[serde(alias = "oa_pdf_url")]
    oa_pdf_url: Option<String>,
    #[serde(alias = "journal", alias = "venue")]
    journal: Option<String>,
    #[serde(alias = "citation_count", alias = "cited_by_count")]
    citation_count: Option<u32>,
}

fn convert_work(w: RawScholarApiWork) -> PaperHit {
    let pdf_url = w.pdf_url.or_else(|| w.oa_pdf_url.clone());
    PaperHit {
        source: PaperSource::ScholarApi,
        title: w.title.unwrap_or_default(),
        authors: w.authors.into_iter().filter(|a| !a.is_empty()).collect(),
        year: w
            .year
            .map(|y| y.to_string())
            .or_else(|| year_from_date(w.published_date.as_deref())),
        doi: w.doi,
        arxiv_id: None,
        pmid: None,
        abstract_note: w.abstract_text,
        url: w.url,
        pdf_url,
        oa_pdf_url: w.oa_pdf_url,
        venue: w.journal,
        citation_count: w.citation_count,
    }
}

fn year_from_date(date: Option<&str>) -> Option<String> {
    date?
        .get(..4)
        .filter(|year| year.chars().all(|c| c.is_ascii_digit()))
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_work_extracts_fields() {
        let json = serde_json::json!({
            "title": "ScholarAPI Paper",
            "authors": ["Alice", "Bob"],
            "doi": "10.1/scholar",
            "published_date": "2023-09-14",
            "abstract": "Abs.",
            "url": "https://example.com/paper",
            "pdf_url": "https://example.com/paper.pdf",
            "journal": "Nature",
            "citation_count": 7
        });
        let w: RawScholarApiWork = serde_json::from_value(json).unwrap();
        let hit = convert_work(w);
        assert_eq!(hit.source, PaperSource::ScholarApi);
        assert_eq!(hit.title, "ScholarAPI Paper");
        assert_eq!(hit.authors, vec!["Alice", "Bob"]);
        assert_eq!(hit.year.as_deref(), Some("2023"));
        assert_eq!(hit.doi.as_deref(), Some("10.1/scholar"));
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://example.com/paper.pdf")
        );
        assert_eq!(hit.venue.as_deref(), Some("Nature"));
        assert_eq!(hit.citation_count, Some(7));
    }

    #[tokio::test]
    async fn search_sends_api_key_header() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "results": [{
                "title": "Mock ScholarAPI",
                "authors": ["Jane"],
                "published_date": "2024-01-02"
            }]
        });
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "graphs"))
            .and(query_param("limit", "5"))
            .and(header("x-api-key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = ScholarApiClient::new(Some(&server.uri()), "secret".to_string());
        let hits = client.search("graphs", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Mock ScholarAPI");
        assert_eq!(hits[0].year.as_deref(), Some("2024"));
    }
}
