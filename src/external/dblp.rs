use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://dblp.org";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct DblpClient {
    client: Client,
    base_url: String,
}

impl DblpClient {
    pub fn new(base_url: Option<&str>) -> Self {
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
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "DBLP search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/search/publ/api?q={encoded}&format=json&h={limit}",
            self.base_url
        );

        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("DBLP API error at {url}"),
            });
        }

        let raw: RawDblpResponse = response.json().await?;
        Ok(raw
            .result
            .hits
            .and_then(|h| h.hit)
            .map(|hits| hits.into_iter().map(|h| convert_info(h.info)).collect())
            .unwrap_or_default())
    }
}

impl std::fmt::Debug for DblpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DblpClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawDblpResponse {
    result: RawDblpResult,
}

#[derive(Debug, Deserialize)]
struct RawDblpResult {
    hits: Option<RawDblpHits>,
}

#[derive(Debug, Deserialize)]
struct RawDblpHits {
    hit: Option<Vec<RawDblpHit>>,
}

#[derive(Debug, Deserialize)]
struct RawDblpHit {
    info: RawDblpInfo,
}

#[derive(Debug, Deserialize)]
struct RawDblpInfo {
    title: Option<String>,
    venue: Option<String>,
    year: Option<String>,
    doi: Option<String>,
    url: Option<String>,
    authors: Option<RawDblpAuthors>,
}

#[derive(Debug, Deserialize)]
struct RawDblpAuthors {
    author: Option<RawDblpAuthorField>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDblpAuthorField {
    One(RawDblpAuthor),
    Many(Vec<RawDblpAuthor>),
}

#[derive(Debug, Deserialize)]
struct RawDblpAuthor {
    text: Option<String>,
}

fn convert_info(info: RawDblpInfo) -> PaperHit {
    let authors: Vec<String> = info
        .authors
        .and_then(|a| a.author)
        .map(|field| match field {
            RawDblpAuthorField::One(a) => a.text.into_iter().collect(),
            RawDblpAuthorField::Many(v) => v.into_iter().filter_map(|a| a.text).collect(),
        })
        .unwrap_or_default();

    PaperHit {
        source: PaperSource::Dblp,
        title: info.title.unwrap_or_default(),
        authors,
        year: info.year,
        doi: info.doi,
        arxiv_id: None,
        pmid: None,
        abstract_note: None,
        url: info.url,
        pdf_url: None,
        oa_pdf_url: None,
        venue: info.venue,
        citation_count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_info_with_multiple_authors() {
        let json = serde_json::json!({
            "title": "DBLP Paper",
            "venue": "ICLR",
            "year": "2024",
            "doi": "10.1/dblp",
            "url": "https://dblp.org/rec/x",
            "authors": {
                "author": [
                    {"@pid": "1", "text": "Alice"},
                    {"@pid": "2", "text": "Bob"}
                ]
            }
        });
        let info: RawDblpInfo = serde_json::from_value(json).unwrap();
        let hit = convert_info(info);
        assert_eq!(hit.title, "DBLP Paper");
        assert_eq!(hit.authors, vec!["Alice", "Bob"]);
        assert_eq!(hit.venue.as_deref(), Some("ICLR"));
        assert_eq!(hit.year.as_deref(), Some("2024"));
        assert_eq!(hit.doi.as_deref(), Some("10.1/dblp"));
        assert_eq!(hit.source, PaperSource::Dblp);
    }

    #[test]
    fn convert_info_with_single_author() {
        let json = serde_json::json!({
            "title": "Solo",
            "authors": {"author": {"@pid": "1", "text": "Alice"}}
        });
        let info: RawDblpInfo = serde_json::from_value(json).unwrap();
        let hit = convert_info(info);
        assert_eq!(hit.authors, vec!["Alice"]);
    }

    #[test]
    fn convert_info_handles_missing_fields() {
        let json = serde_json::json!({"title": "Minimal"});
        let info: RawDblpInfo = serde_json::from_value(json).unwrap();
        let hit = convert_info(info);
        assert_eq!(hit.title, "Minimal");
        assert!(hit.authors.is_empty());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = DblpClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_publ_api() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "result": {
                "hits": {
                    "hit": [{
                        "info": {
                            "title": "DBLP Mock",
                            "venue": "NeurIPS",
                            "year": "2023",
                            "authors": {"author": [{"@pid": "1", "text": "Jane Doe"}]}
                        }
                    }]
                }
            }
        });
        Mock::given(method("GET"))
            .and(path("/search/publ/api"))
            .and(query_param("q", "transformers"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = DblpClient::new(Some(&server.uri()));
        let hits = client.search("transformers", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "DBLP Mock");
        assert_eq!(hits[0].venue.as_deref(), Some("NeurIPS"));
    }

    #[tokio::test]
    async fn search_handles_malformed_json() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<not json>"))
            .mount(&server)
            .await;

        let client = DblpClient::new(Some(&server.uri()));
        let err = client.search("q", 5).await.unwrap_err();
        assert!(
            matches!(err, ZoteroMcpError::Http(_) | ZoteroMcpError::Serde(_)),
            "expected decode error, got {err:?}"
        );
    }
}
