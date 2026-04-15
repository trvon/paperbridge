use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.adsabs.harvard.edu/v1";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";
const FIELDS: &str = "title,author,year,doi,pub,abstract,bibcode,citation_count";

#[derive(Clone)]
pub struct AdsClient {
    client: Client,
    base_url: String,
    api_token: String,
}

impl AdsClient {
    pub fn new(base_url: Option<&str>, api_token: String) -> Self {
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
            api_token,
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "NASA ADS search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/search/query?q={encoded}&rows={limit}&fl={FIELDS}",
            self.base_url
        );

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("NASA ADS API error at {url}"),
            });
        }

        let raw: RawAdsResponse = response.json().await?;
        Ok(raw
            .response
            .map(|r| r.docs.into_iter().map(convert_doc).collect())
            .unwrap_or_default())
    }
}

impl std::fmt::Debug for AdsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdsClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawAdsResponse {
    response: Option<RawAdsBody>,
}

#[derive(Debug, Deserialize)]
struct RawAdsBody {
    #[serde(default)]
    docs: Vec<RawAdsDoc>,
}

#[derive(Debug, Deserialize)]
struct RawAdsDoc {
    #[serde(default)]
    title: Vec<String>,
    #[serde(default)]
    author: Vec<String>,
    year: Option<String>,
    #[serde(default)]
    doi: Vec<String>,
    #[serde(rename = "pub")]
    pub_field: Option<String>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    bibcode: Option<String>,
    citation_count: Option<u32>,
}

fn convert_doc(d: RawAdsDoc) -> PaperHit {
    let title = d.title.into_iter().next().unwrap_or_default();
    let doi = d.doi.into_iter().next();
    let url = d
        .bibcode
        .as_deref()
        .map(|b| format!("https://ui.adsabs.harvard.edu/abs/{b}"));

    PaperHit {
        source: PaperSource::Ads,
        title,
        authors: d.author,
        year: d.year,
        doi,
        arxiv_id: None,
        pmid: None,
        abstract_note: d.abstract_text,
        url,
        pdf_url: None,
        oa_pdf_url: None,
        venue: d.pub_field,
        citation_count: d.citation_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_doc_extracts_fields() {
        let json = serde_json::json!({
            "title": ["ADS Paper"],
            "author": ["Smith J", "Jones A"],
            "year": "2022",
            "doi": ["10.1/ads"],
            "pub": "Astrophysical Journal",
            "abstract": "An abstract.",
            "bibcode": "2022ApJ...1..1S",
            "citation_count": 5
        });
        let d: RawAdsDoc = serde_json::from_value(json).unwrap();
        let hit = convert_doc(d);
        assert_eq!(hit.title, "ADS Paper");
        assert_eq!(hit.authors, vec!["Smith J", "Jones A"]);
        assert_eq!(hit.year.as_deref(), Some("2022"));
        assert_eq!(hit.doi.as_deref(), Some("10.1/ads"));
        assert_eq!(hit.venue.as_deref(), Some("Astrophysical Journal"));
        assert_eq!(hit.citation_count, Some(5));
        assert_eq!(
            hit.url.as_deref(),
            Some("https://ui.adsabs.harvard.edu/abs/2022ApJ...1..1S")
        );
        assert_eq!(hit.source, PaperSource::Ads);
    }

    #[test]
    fn convert_doc_handles_missing_fields() {
        let json = serde_json::json!({});
        let d: RawAdsDoc = serde_json::from_value(json).unwrap();
        let hit = convert_doc(d);
        assert_eq!(hit.title, "");
        assert!(hit.authors.is_empty());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = AdsClient::new(None, "tok".to_string());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_query_endpoint() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "response": {
                "docs": [{
                    "title": ["ADS Mock"],
                    "author": ["Jane"],
                    "year": "2024",
                    "doi": ["10.1/ads"],
                    "bibcode": "2024X...1..1J"
                }]
            }
        });
        Mock::given(method("GET"))
            .and(path("/search/query"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = AdsClient::new(Some(&server.uri()), "token".to_string());
        let hits = client.search("supernova", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "ADS Mock");
    }

    #[tokio::test]
    async fn search_surfaces_auth_error_on_401() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let client = AdsClient::new(Some(&server.uri()), "bad".to_string());
        let err = client.search("q", 5).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 401),
            other => panic!("expected 401 Api error, got {other:?}"),
        }
    }
}
