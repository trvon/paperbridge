use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::ops::Not;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.openalex.org";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct OpenAlexClient {
    client: Client,
    base_url: String,
    mailto: Option<String>,
    api_key: Option<String>,
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
            api_key: None,
        }
    }

    /// Send `api_key` on every request. Optional; a key raises the OpenAlex
    /// daily budget over unkeyed (`mailto`-only) traffic.
    pub fn with_api_key(mut self, api_key: Option<String>) -> Self {
        self.api_key = api_key.filter(|k| k.trim().is_empty().not());
        self
    }

    /// Append `mailto` and `api_key` query parameters to a URL that already has a `?`.
    fn push_auth_params(&self, url: &mut String) {
        if let Some(email) = self.mailto.as_deref() {
            url.push_str(&format!("&mailto={}", urlencoding::encode(email)));
        }
        if let Some(key) = self.api_key.as_deref() {
            url.push_str(&format!("&api_key={}", urlencoding::encode(key)));
        }
    }

    /// Strip the API key from error text; reqwest errors embed the full URL.
    fn redact(&self, error: ZoteroMcpError) -> ZoteroMcpError {
        let Some(key) = self.api_key.as_deref() else {
            return error;
        };
        let encoded = urlencoding::encode(key).into_owned();
        let secrets = [key, encoded.as_str()];
        match error {
            ZoteroMcpError::Http(message) => {
                ZoteroMcpError::Http(crate::error::sanitize_message(&message, &secrets))
            }
            ZoteroMcpError::Serde(message) => {
                ZoteroMcpError::Serde(crate::error::sanitize_message(&message, &secrets))
            }
            ZoteroMcpError::Api { status, message } => ZoteroMcpError::Api {
                status,
                message: crate::error::sanitize_message(&message, &secrets),
            },
            other => other,
        }
    }

    /// OpenAlex `is_retracted` for a DOI. `Ok(None)` when OpenAlex has no such work.
    pub async fn is_retracted(&self, doi: &str) -> Result<Option<bool>> {
        self.is_retracted_inner(doi)
            .await
            .map_err(|error| self.redact(error))
    }

    async fn is_retracted_inner(&self, doi: &str) -> Result<Option<bool>> {
        let encoded = urlencoding::encode(doi.trim());
        let mut url = format!("{}/works/doi:{encoded}?select=is_retracted", self.base_url);
        self.push_auth_params(&mut url);

        // Single attempt outside the request router: this is a best-effort
        // side check on resolve/open, so it must not retry or set the shared
        // OpenAlex cooldown that `search_papers` also waits on.
        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("OpenAlex work lookup failed for DOI '{}'", doi.trim()),
            });
        }
        let raw: RawRetractionFlag = response.json().await?;
        Ok(raw.is_retracted)
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        self.search_inner(query, limit)
            .await
            .map_err(|error| self.redact(error))
    }

    async fn search_inner(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "OpenAlex search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let mut url = format!("{}/works?search={encoded}&per_page={limit}", self.base_url);
        self.push_auth_params(&mut url);

        let response = send_with_retry("openalex", self.client.get(&url)).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("OpenAlex API error at {}/works", self.base_url),
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
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawRetractionFlag {
    is_retracted: Option<bool>,
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
        hit_id: None,
        truncation: None,
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
        cache: None,
        relevance_score: None,
        ids: None,
        match_info: None,
        access: None,
        work_type: None,
        isbn: None,
        next: Vec::new(),
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

    const KEY: &str = "oa-secret-key-123";

    #[tokio::test]
    async fn is_retracted_sends_api_key_and_mailto() {
        use wiremock::matchers::{method, path_regex, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/works/doi:10\.1016(/|%2F)s0140-6736"))
            .and(query_param("select", "is_retracted"))
            .and(query_param("api_key", KEY))
            .and(query_param("mailto", "me@example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"is_retracted": true})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = OpenAlexClient::new(Some(&server.uri()), Some("me@example.com".into()))
            .with_api_key(Some(KEY.into()));
        let flag = client
            .is_retracted("10.1016/s0140-6736(97)11096-0")
            .await
            .unwrap();
        assert_eq!(flag, Some(true));
    }

    #[tokio::test]
    async fn is_retracted_returns_none_for_unknown_doi() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = OpenAlexClient::new(Some(&server.uri()), None);
        assert_eq!(client.is_retracted("10.9/none").await.unwrap(), None);
    }

    #[tokio::test]
    async fn search_sends_api_key_when_configured() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("api_key", KEY))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = OpenAlexClient::new(Some(&server.uri()), None).with_api_key(Some(KEY.into()));
        assert!(client.search("q", 1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn errors_never_contain_the_api_key() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = OpenAlexClient::new(Some(&server.uri()), None).with_api_key(Some(KEY.into()));
        let http_err = client.search("q", 1).await.unwrap_err();
        let lookup_err = client.is_retracted("10.1/x").await.unwrap_err();

        // Connection refused: reqwest's error text embeds the full request URL.
        let offline =
            OpenAlexClient::new(Some("http://127.0.0.1:9"), None).with_api_key(Some(KEY.into()));
        let transport_err = offline.is_retracted("10.1/x").await.unwrap_err();
        assert!(transport_err.to_string().contains("api_key=<redacted>"));

        for err in [http_err, lookup_err, transport_err] {
            let text = format!("{err} {err:?}");
            assert!(!text.contains(KEY), "key leaked: {text}");
        }
        assert!(!format!("{client:?}").contains(KEY));
        assert!(format!("{client:?}").contains("has_api_key: true"));
    }

    /// Keys that need percent-encoding must be redacted in both spellings.
    const ODD_KEY: &str = "k+y/=&secret";

    #[tokio::test]
    async fn status_errors_are_built_without_the_key() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string(KEY))
            .mount(&server)
            .await;
        let client = OpenAlexClient::new(Some(&server.uri()), None).with_api_key(Some(KEY.into()));
        // Unredacted inner errors: the message itself must not carry the URL query.
        for err in [
            client.search_inner("q", 1).await.unwrap_err(),
            client.is_retracted_inner("10.1/x").await.unwrap_err(),
        ] {
            assert!(matches!(err, ZoteroMcpError::Api { status: 500, .. }));
            assert!(!format!("{err} {err:?}").contains(KEY));
        }
    }

    #[tokio::test]
    async fn decode_errors_redact_the_key_on_both_paths() {
        use wiremock::matchers::{method, path_regex, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/works"))
            .and(query_param("api_key", ODD_KEY))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .expect(4)
            .mount(&server)
            .await;
        let client =
            OpenAlexClient::new(Some(&server.uri()), None).with_api_key(Some(ODD_KEY.into()));
        let encoded = urlencoding::encode(ODD_KEY).into_owned();

        // reqwest decode errors embed the request URL, so redaction is load-bearing.
        let raw = client.is_retracted_inner("10.1/x").await.unwrap_err();
        assert!(raw.to_string().contains(&encoded), "{raw}");

        for err in [
            client.search("q", 1).await.unwrap_err(),
            client.is_retracted("10.1/x").await.unwrap_err(),
            client.search("q", 1).await.unwrap_err(),
        ] {
            let text = format!("{err} {err:?}");
            assert!(!text.contains(ODD_KEY), "{text}");
            assert!(!text.contains(&encoded), "{text}");
        }
    }

    #[test]
    fn redact_covers_every_message_variant() {
        let client = OpenAlexClient::new(None, None).with_api_key(Some(ODD_KEY.into()));
        let encoded = urlencoding::encode(ODD_KEY).into_owned();
        let msg = format!("raw {ODD_KEY} enc {encoded} q ?api_key={encoded}");
        for err in [
            ZoteroMcpError::Http(msg.clone()),
            ZoteroMcpError::Serde(msg.clone()),
            ZoteroMcpError::Api {
                status: 500,
                message: msg.clone(),
            },
        ] {
            let text = format!("{:?}", client.redact(err));
            assert!(!text.contains(ODD_KEY), "{text}");
            assert!(!text.contains(&encoded), "{text}");
        }
    }

    #[test]
    fn blank_api_key_is_ignored() {
        let client = OpenAlexClient::new(None, None).with_api_key(Some("  ".into()));
        assert!(client.api_key.is_none());
    }
}
