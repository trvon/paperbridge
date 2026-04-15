use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openalex.org";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct OpenAlexClient {
    client: Client,
    base_url: String,
    mailto: Option<String>,
}

impl OpenAlexClient {
    pub fn new(base_url: Option<&str>, mailto: Option<String>) -> Self {
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
            mailto,
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "OpenAlex search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let mut url = format!("{}/works?search={encoded}&per_page={limit}", self.base_url);
        if let Some(email) = self.mailto.as_deref() {
            url.push_str(&format!("&mailto={}", urlencoding::encode(email)));
        }

        let response = send_with_retry(self.client.get(&url)).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("OpenAlex API error at {url}"),
            });
        }

        let raw: RawOpenAlexResponse = response.json().await?;
        Ok(raw.results.into_iter().map(convert_work).collect())
    }
}

impl std::fmt::Debug for OpenAlexClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAlexClient")
            .field("base_url", &self.base_url)
            .field("has_mailto", &self.mailto.is_some())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexResponse {
    #[serde(default)]
    results: Vec<RawOpenAlexWork>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexWork {
    title: Option<String>,
    doi: Option<String>,
    publication_year: Option<u32>,
    #[serde(default)]
    authorships: Vec<RawOpenAlexAuthorship>,
    abstract_inverted_index: Option<serde_json::Map<String, serde_json::Value>>,
    primary_location: Option<RawOpenAlexLocation>,
    best_oa_location: Option<RawOpenAlexLocation>,
    cited_by_count: Option<u32>,
    ids: Option<RawOpenAlexIds>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexAuthorship {
    author: Option<RawOpenAlexAuthor>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexAuthor {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexLocation {
    pdf_url: Option<String>,
    landing_page_url: Option<String>,
    source: Option<RawOpenAlexSource>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexSource {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawOpenAlexIds {
    pmid: Option<String>,
}

fn convert_work(w: RawOpenAlexWork) -> PaperHit {
    let authors = w
        .authorships
        .into_iter()
        .filter_map(|a| a.author.and_then(|au| au.display_name))
        .filter(|n| !n.is_empty())
        .collect();

    let abstract_note = w.abstract_inverted_index.map(reconstruct_abstract);

    let venue = w
        .primary_location
        .as_ref()
        .and_then(|loc| loc.source.as_ref().and_then(|s| s.display_name.clone()));

    let url = w
        .primary_location
        .as_ref()
        .and_then(|loc| loc.landing_page_url.clone());

    let pdf_url = w
        .primary_location
        .as_ref()
        .and_then(|loc| loc.pdf_url.clone());

    let oa_pdf_url = w.best_oa_location.and_then(|loc| loc.pdf_url);

    let pmid = w.ids.and_then(|ids| ids.pmid).and_then(|raw| {
        raw.rsplit('/')
            .next()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    });

    PaperHit {
        source: PaperSource::OpenAlex,
        title: w.title.unwrap_or_default(),
        authors,
        year: w.publication_year.map(|y| y.to_string()),
        doi: w.doi.map(strip_doi_prefix),
        arxiv_id: None,
        pmid,
        abstract_note,
        url,
        pdf_url,
        oa_pdf_url,
        venue,
        citation_count: w.cited_by_count,
    }
}

fn strip_doi_prefix(doi: String) -> String {
    doi.strip_prefix("https://doi.org/")
        .or_else(|| doi.strip_prefix("http://doi.org/"))
        .map(|s| s.to_string())
        .unwrap_or(doi)
}

fn reconstruct_abstract(map: serde_json::Map<String, serde_json::Value>) -> String {
    let mut positions: Vec<(usize, &str)> = Vec::new();
    for (word, val) in &map {
        if let Some(arr) = val.as_array() {
            for v in arr {
                if let Some(pos) = v.as_u64() {
                    positions.push((pos as usize, word.as_str()));
                }
            }
        }
    }
    positions.sort_by_key(|(pos, _)| *pos);
    positions
        .into_iter()
        .map(|(_, w)| w)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_work_extracts_fields() {
        let json = serde_json::json!({
            "title": "Sample OpenAlex Work",
            "doi": "https://doi.org/10.1/abc",
            "publication_year": 2024,
            "authorships": [
                {"author": {"display_name": "Alice"}},
                {"author": {"display_name": "Bob"}}
            ],
            "abstract_inverted_index": {
                "Hello": [0],
                "world": [1]
            },
            "primary_location": {
                "pdf_url": "https://example.com/paper.pdf",
                "landing_page_url": "https://example.com/paper",
                "source": {"display_name": "ExampleVenue"}
            },
            "best_oa_location": {
                "pdf_url": "https://oa.example.com/paper.pdf"
            },
            "cited_by_count": 7,
            "ids": {"pmid": "https://pubmed.ncbi.nlm.nih.gov/12345"}
        });
        let work: RawOpenAlexWork = serde_json::from_value(json).unwrap();
        let hit = convert_work(work);
        assert_eq!(hit.title, "Sample OpenAlex Work");
        assert_eq!(hit.doi.as_deref(), Some("10.1/abc"));
        assert_eq!(hit.year.as_deref(), Some("2024"));
        assert_eq!(hit.authors, vec!["Alice", "Bob"]);
        assert_eq!(hit.abstract_note.as_deref(), Some("Hello world"));
        assert_eq!(hit.venue.as_deref(), Some("ExampleVenue"));
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://example.com/paper.pdf")
        );
        assert_eq!(
            hit.oa_pdf_url.as_deref(),
            Some("https://oa.example.com/paper.pdf")
        );
        assert_eq!(hit.citation_count, Some(7));
        assert_eq!(hit.pmid.as_deref(), Some("12345"));
        assert_eq!(hit.source, PaperSource::OpenAlex);
    }

    #[test]
    fn convert_work_handles_missing_fields() {
        let json = serde_json::json!({"title": "Minimal"});
        let work: RawOpenAlexWork = serde_json::from_value(json).unwrap();
        let hit = convert_work(work);
        assert_eq!(hit.title, "Minimal");
        assert!(hit.authors.is_empty());
        assert!(hit.doi.is_none());
        assert!(hit.year.is_none());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = OpenAlexClient::new(None, None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_works_endpoint() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "results": [{
                "title": "Mock OpenAlex Paper",
                "doi": "https://doi.org/10.1/oa",
                "publication_year": 2023,
                "authorships": [{"author": {"display_name": "Jane Doe"}}],
                "primary_location": {"source": {"display_name": "VenueX"}}
            }]
        });
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("search", "quantum"))
            .and(query_param("per_page", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = OpenAlexClient::new(Some(&server.uri()), None);
        let hits = client.search("quantum", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Mock OpenAlex Paper");
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/oa"));
        assert_eq!(hits[0].venue.as_deref(), Some("VenueX"));
    }

    #[tokio::test]
    async fn search_retries_on_429_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{"title": "Retried"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = OpenAlexClient::new(Some(&server.uri()), None);
        let hits = client.search("q", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Retried");
    }

    #[tokio::test]
    async fn search_gives_up_after_persistent_429() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(2)
            .mount(&server)
            .await;

        let client = OpenAlexClient::new(Some(&server.uri()), None);
        let err = client.search("q", 5).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 429),
            other => panic!("expected 429 Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_handles_malformed_json() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
            .mount(&server)
            .await;

        let client = OpenAlexClient::new(Some(&server.uri()), None);
        let err = client.search("q", 5).await.unwrap_err();
        assert!(
            matches!(err, ZoteroMcpError::Http(_) | ZoteroMcpError::Serde(_)),
            "expected decode/serde error, got {err:?}"
        );
    }
}
