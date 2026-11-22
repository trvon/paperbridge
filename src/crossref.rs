use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::hit_enrich::normalize_doi_str;
use crate::models::{CrossrefWork, PaperHit, PaperSource, WorkNotice};
use reqwest::Client;
use serde::Deserialize;
use std::ops::Not;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.crossref.org";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct CrossrefClient {
    client: Client,
    base_url: String,
}

impl CrossrefClient {
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

    /// Resolve a DOI. Tries the canonical form first, then (on 404 only)
    /// the legacy form, so a DOI that really ends in punctuation or holds
    /// `%2F`, and `doi:` hit_ids minted before the shared normalizer, still
    /// resolve.
    pub async fn resolve_doi(&self, doi: &str) -> Result<CrossrefWork> {
        let forms = crate::hit_enrich::doi_lookup_forms(doi);
        if forms.is_empty() || normalize_doi_str(doi).is_none() {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "'{}' does not look like a valid DOI",
                doi.trim()
            )));
        }
        let last = forms.len() - 1;
        for (index, form) in forms.iter().enumerate() {
            match self.resolve_form(form).await {
                Err(ZoteroMcpError::Api { status: 404, .. }) if index < last => continue,
                other => return other,
            }
        }
        Err(ZoteroMcpError::Api {
            status: 404,
            message: format!("DOI '{}' not found in Crossref", doi.trim()),
        })
    }

    async fn resolve_form(&self, trimmed: &str) -> Result<CrossrefWork> {
        let encoded = urlencoding::encode(trimmed);
        let url = format!("{}/works/{encoded}", self.base_url);

        let response = send_with_retry("crossref", self.client.get(&url)).await?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ZoteroMcpError::Api {
                status: 404,
                message: format!("DOI '{trimmed}' not found in Crossref"),
            });
        }
        if status.is_success().not() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Crossref API error: {body}"),
            });
        }

        let raw: RawCrossrefResponse = response.json().await?;
        Ok(convert_message(trimmed, raw.message))
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "Crossref search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        // Prefer bibliographic query for title-like phrases; falls back more
        // cleanly than bare `query=` for well-known paper titles.
        let words = trimmed.split_whitespace().count();
        let url = if words >= 3 {
            format!(
                "{}/works?query.bibliographic={encoded}&rows={limit}",
                self.base_url
            )
        } else {
            format!("{}/works?query={encoded}&rows={limit}", self.base_url)
        };

        let response = send_with_retry("crossref", self.client.get(&url)).await?;
        let status = response.status();
        if status.is_success().not() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Crossref search error: {body}"),
            });
        }

        let raw: RawCrossrefSearchResponse = response.json().await?;
        let items = raw.message.items.unwrap_or_default();
        Ok(items.into_iter().map(convert_message_to_hit).collect())
    }
}

impl std::fmt::Debug for CrossrefClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossrefClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

// --- Raw Crossref wire types (private) ---

#[derive(Debug, Deserialize)]
struct RawCrossrefResponse {
    message: RawCrossrefMessage,
}

#[derive(Debug, Deserialize)]
struct RawCrossrefMessage {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    title: Option<Vec<String>>,
    author: Option<Vec<RawAuthor>>,
    #[serde(rename = "container-title")]
    container_title: Option<Vec<String>>,
    #[serde(rename = "published-print")]
    published_print: Option<RawDateField>,
    #[serde(rename = "published-online")]
    published_online: Option<RawDateField>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
    publisher: Option<String>,
    #[serde(rename = "type")]
    work_type: Option<String>,
    resource: Option<RawResource>,
    /// Notices that update this work. `update-to` sits on the notice itself.
    /// Kept as raw JSON and read field by field: this struct is shared with
    /// search, so a malformed entry must not fail the whole message.
    #[serde(rename = "updated-by", default)]
    updated_by: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawResource {
    primary: Option<RawResourceLink>,
}

#[derive(Debug, Deserialize)]
struct RawResourceLink {
    #[serde(rename = "URL")]
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawAuthor {
    given: Option<String>,
    family: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDateField {
    #[serde(rename = "date-parts")]
    date_parts: Option<Vec<Vec<Option<u32>>>>,
}

#[derive(Debug, Deserialize)]
struct RawCrossrefSearchResponse {
    message: RawCrossrefSearchMessage,
}

#[derive(Debug, Deserialize)]
struct RawCrossrefSearchMessage {
    items: Option<Vec<RawCrossrefMessage>>,
}

// --- Conversion ---

fn convert_message(doi_input: &str, msg: RawCrossrefMessage) -> CrossrefWork {
    let doi = msg.doi.unwrap_or_else(|| doi_input.to_string());

    let title = msg.title.and_then(|v| v.into_iter().next());

    let authors = msg
        .author
        .unwrap_or_default()
        .into_iter()
        .map(format_author)
        .collect();

    let year = extract_year(msg.published_print.as_ref())
        .or_else(|| extract_year(msg.published_online.as_ref()));

    let journal = msg.container_title.and_then(|v| v.into_iter().next());

    let abstract_note = msg.abstract_text.map(|s| strip_xml_tags(&s));

    CrossrefWork {
        doi,
        title,
        authors,
        year,
        journal,
        abstract_note,
        url: msg.url,
        resource_url: msg
            .resource
            .and_then(|resource| resource.primary)
            .and_then(|link| link.url),
        publisher: msg.publisher,
        item_type: msg.work_type,
        oa_pdf_url: None,
        notices: convert_notices(msg.updated_by),
        openalex_only_retracted: None,
        notices_checked: Vec::new(),
        notices_failed: Vec::new(),
    }
}

/// Upper bound on notices per work; they bypass the `open_paper` metadata budget.
const MAX_NOTICES: usize = 32;
/// Upper bound on each notice string, in chars.
const MAX_NOTICE_FIELD_CHARS: usize = 256;

/// Raw `updated-by` entries inspected per work, before dedupe and the cap.
const MAX_RAW_NOTICE_ENTRIES: usize = 1024;

fn convert_notices(raw: Option<serde_json::Value>) -> Vec<WorkNotice> {
    let entries = match raw {
        Some(serde_json::Value::Array(entries)) => entries,
        _ => return Vec::new(),
    };
    let mut notices: Vec<WorkNotice> = Vec::new();
    for entry in entries.iter().take(MAX_RAW_NOTICE_ENTRIES) {
        let Some(notice_type) = notice_str(entry, "type").map(|t| normalize_notice_type(&t)) else {
            continue;
        };
        let notice = WorkNotice {
            notice_type,
            source: notice_str(entry, "source"),
            notice_doi: notice_str(entry, "DOI").map(|d| normalize_doi_str(&d).unwrap_or(d)),
            date: format_date(entry.get("updated")),
        };
        if notices.contains(&notice).not() {
            notices.push(notice);
        }
    }
    // Over the cap, move retraction-class first (stable) so truncation never
    // drops a retraction; otherwise keep Crossref's order.
    if notices.len() > MAX_NOTICES {
        notices.sort_by_key(|n| is_retraction_class(&n.notice_type).not());
        notices.truncate(MAX_NOTICES);
    }
    notices
}

/// Lowercase and use `_` separators: `Partial-Retraction` -> `partial_retraction`.
pub(crate) fn normalize_notice_type(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// Notice types that mean the work's findings were withdrawn.
pub(crate) fn is_retraction_class(notice_type: &str) -> bool {
    matches!(
        normalize_notice_type(notice_type).as_str(),
        "retraction" | "partial_retraction" | "withdrawal" | "removal"
    )
}

fn notice_str(entry: &serde_json::Value, key: &str) -> Option<String> {
    let value = entry.get(key)?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(MAX_NOTICE_FIELD_CHARS).collect())
}

fn format_date(updated: Option<&serde_json::Value>) -> Option<String> {
    let parts = updated?
        .get("date-parts")?
        .as_array()?
        .first()?
        .as_array()?;
    let mut iter = parts.iter().map_while(serde_json::Value::as_u64);
    let year = iter.next().filter(|y| *y <= 9999)?;
    let mut out = format!("{year:04}");
    // Month then day; stop at the first out-of-range part.
    for (part, max) in iter.zip([12, 31]) {
        if (1..=max).contains(&part).not() {
            break;
        }
        out.push_str(&format!("-{part:02}"));
    }
    Some(out)
}

fn convert_message_to_hit(msg: RawCrossrefMessage) -> PaperHit {
    let year = extract_year(msg.published_print.as_ref())
        .or_else(|| extract_year(msg.published_online.as_ref()));

    let title = msg
        .title
        .and_then(|v| v.into_iter().next())
        .unwrap_or_default();

    let authors = msg
        .author
        .unwrap_or_default()
        .into_iter()
        .map(format_author)
        .collect();

    let venue = msg.container_title.and_then(|v| v.into_iter().next());
    let abstract_note = msg.abstract_text.as_deref().map(strip_xml_tags);

    PaperHit {
        hit_id: None,
        truncation: None,
        source: PaperSource::Crossref,
        title,
        authors,
        year,
        doi: msg.doi,
        arxiv_id: None,
        pmid: None,
        abstract_note,
        url: msg.url,
        pdf_url: None,
        oa_pdf_url: None,
        venue,
        citation_count: None,
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

fn format_author(a: RawAuthor) -> String {
    if let Some(name) = a.name {
        return name;
    }
    match (a.family.as_deref(), a.given.as_deref()) {
        (Some(f), Some(g)) => format!("{f}, {g}"),
        (Some(f), None) => f.to_string(),
        (None, Some(g)) => g.to_string(),
        (None, None) => String::new(),
    }
}

fn extract_year(field: Option<&RawDateField>) -> Option<String> {
    field?
        .date_parts
        .as_ref()?
        .first()?
        .first()?
        .map(|y| y.to_string())
}

fn strip_xml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            inside_tag = true;
        } else if ch == '>' {
            inside_tag = false;
        } else if inside_tag.not() {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_xml_tags_removes_jats() {
        let input = "<jats:p>This is <jats:italic>italic</jats:italic> text.</jats:p>";
        assert_eq!(strip_xml_tags(input), "This is italic text.");
    }

    #[test]
    fn strip_xml_tags_passthrough_plain_text() {
        assert_eq!(strip_xml_tags("no tags here"), "no tags here");
    }

    #[test]
    fn format_author_full_name() {
        let a = RawAuthor {
            given: Some("Ed".to_string()),
            family: Some("Boyden".to_string()),
            name: None,
        };
        assert_eq!(format_author(a), "Boyden, Ed");
    }

    #[test]
    fn format_author_single_name() {
        let a = RawAuthor {
            given: None,
            family: None,
            name: Some("World Health Organization".to_string()),
        };
        assert_eq!(format_author(a), "World Health Organization");
    }

    #[test]
    fn format_author_family_only() {
        let a = RawAuthor {
            given: None,
            family: Some("Smith".to_string()),
            name: None,
        };
        assert_eq!(format_author(a), "Smith");
    }

    #[test]
    fn extract_year_from_date_parts() {
        let field = RawDateField {
            date_parts: Some(vec![vec![Some(2023), Some(8), Some(14)]]),
        };
        assert_eq!(extract_year(Some(&field)), Some("2023".to_string()));
    }

    #[test]
    fn extract_year_missing() {
        let field = RawDateField {
            date_parts: Some(vec![vec![]]),
        };
        assert_eq!(extract_year(Some(&field)), None);
        assert_eq!(extract_year(None), None);
    }

    #[test]
    fn convert_full_crossref_message() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1038/nature12373",
                "title": ["Optical control of mammalian endogenous transcription and epigenetic states"],
                "author": [
                    {"given": "Silvana", "family": "Konermann"},
                    {"given": "Mark D.", "family": "Brigham"}
                ],
                "container-title": ["Nature"],
                "published-print": {"date-parts": [[2013, 8, 22]]},
                "abstract": "<jats:p>A method is described.</jats:p>",
                "URL": "http://dx.doi.org/10.1038/nature12373",
                "publisher": "Springer Science and Business Media LLC",
                "type": "journal-article"
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1038/nature12373", raw.message);

        assert_eq!(work.doi, "10.1038/nature12373");
        assert_eq!(
            work.title.as_deref(),
            Some("Optical control of mammalian endogenous transcription and epigenetic states")
        );
        assert_eq!(work.authors, vec!["Konermann, Silvana", "Brigham, Mark D."]);
        assert_eq!(work.year.as_deref(), Some("2013"));
        assert_eq!(work.journal.as_deref(), Some("Nature"));
        assert_eq!(
            work.abstract_note.as_deref(),
            Some("A method is described.")
        );
        assert_eq!(
            work.url.as_deref(),
            Some("http://dx.doi.org/10.1038/nature12373")
        );
        assert_eq!(
            work.publisher.as_deref(),
            Some("Springer Science and Business Media LLC")
        );
        assert_eq!(work.item_type.as_deref(), Some("journal-article"));
    }

    #[test]
    fn convert_minimal_crossref_message() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1234/test",
                "type": "other"
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1234/test", raw.message);

        assert_eq!(work.doi, "10.1234/test");
        assert!(work.title.is_none());
        assert!(work.authors.is_empty());
        assert!(work.year.is_none());
        assert!(work.journal.is_none());
        assert!(work.abstract_note.is_none());
        assert!(work.url.is_none());
        assert!(work.publisher.is_none());
        assert_eq!(work.item_type.as_deref(), Some("other"));
    }

    #[test]
    fn resolve_doi_rejects_invalid_format() {
        let client = CrossrefClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.resolve_doi("not-a-doi")).unwrap_err();
        assert!(err.to_string().contains("does not look like a valid DOI"));
    }

    #[test]
    fn convert_message_uses_published_online_fallback() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1234/online-only",
                "published-online": {"date-parts": [[2024, 1, 15]]}
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1234/online-only", raw.message);
        assert_eq!(work.year.as_deref(), Some("2024"));
    }

    #[test]
    fn convert_message_to_hit_extracts_fields() {
        let json = serde_json::json!({
            "DOI": "10.1038/nature12373",
            "title": ["Sample Title"],
            "author": [{"given": "Alice", "family": "Smith"}],
            "container-title": ["Nature"],
            "published-print": {"date-parts": [[2013]]},
            "abstract": "<jats:p>Body.</jats:p>",
            "URL": "http://dx.doi.org/10.1038/nature12373"
        });
        let raw: RawCrossrefMessage = serde_json::from_value(json).unwrap();
        let hit = convert_message_to_hit(raw);

        assert_eq!(hit.source, PaperSource::Crossref);
        assert_eq!(hit.title, "Sample Title");
        assert_eq!(hit.authors, vec!["Smith, Alice"]);
        assert_eq!(hit.year.as_deref(), Some("2013"));
        assert_eq!(hit.doi.as_deref(), Some("10.1038/nature12373"));
        assert_eq!(hit.venue.as_deref(), Some("Nature"));
        assert_eq!(hit.abstract_note.as_deref(), Some("Body."));
        assert_eq!(
            hit.url.as_deref(),
            Some("http://dx.doi.org/10.1038/nature12373")
        );
        assert!(hit.arxiv_id.is_none());
        assert!(hit.pdf_url.is_none());
    }

    #[tokio::test]
    async fn search_hits_works_endpoint_and_parses_items() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "status": "ok",
            "message": {
                "items": [
                    {
                        "DOI": "10.1/a",
                        "title": ["Paper A"],
                        "author": [{"given": "Al", "family": "Alpha"}],
                        "container-title": ["Venue A"],
                        "published-print": {"date-parts": [[2022]]}
                    },
                    {
                        "DOI": "10.1/b",
                        "title": ["Paper B"]
                    }
                ]
            }
        });
        Mock::given(method("GET"))
            .and(path("/works"))
            .and(query_param("query", "quantum"))
            .and(query_param("rows", "5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        let hits = client.search("quantum", 5).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/a"));
        assert_eq!(hits[0].venue.as_deref(), Some("Venue A"));
        assert_eq!(hits[1].title, "Paper B");
    }

    #[tokio::test]
    async fn resolve_doi_retries_legacy_form_on_not_found() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works/10.5555%2Fseed"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/works/10.5555%2Fseed."))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "message": {"DOI": "10.5555/seed.", "title": ["Seed"]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = CrossrefClient::new(Some(&server.uri()));
        let work = client.resolve_doi("doi:10.5555/seed.").await.unwrap();
        assert_eq!(work.title.as_deref(), Some("Seed"));
        // A DOI with no legacy variant still reports not-found once.
        let err = client.resolve_doi("10.5555/other").await.unwrap_err();
        assert!(matches!(err, ZoteroMcpError::Api { status: 404, .. }));
    }

    #[tokio::test]
    async fn search_returns_api_error_on_non_success_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        let err = client.search("q", 3).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 500),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_handles_malformed_json() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not valid json"))
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        let err = client.search("q", 3).await.unwrap_err();
        match err {
            ZoteroMcpError::Http(message) => {
                assert!(message.to_lowercase().contains("response body"));
            }
            other => panic!("expected Http decode error, got {other:?}"),
        }
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
                "status": "ok",
                "message": {"items": [{"DOI": "10.1/after-retry", "title": ["Retried"]}]}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        let hits = client.search("q", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/after-retry"));
    }
    #[tokio::test]
    async fn resolve_doi_accepts_prefixed_and_resolver_forms() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/works/10\.1234(/|%2F)abc$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok",
                "message": {"title": ["Fixture"]}
            })))
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        for raw in [
            "10.1234/ABC",
            "doi:10.1234/abc",
            "https://doi.org/10.1234/abc",
            "http://dx.doi.org/10.1234/ABC",
        ] {
            let work = client.resolve_doi(raw).await.unwrap_or_else(|e| {
                panic!("{raw}: {e:?}");
            });
            assert_eq!(work.doi, "10.1234/abc", "{raw}");
        }
    }

    /// Shape copied from the live record for 10.1016/S0140-6736(97)11096-0.
    fn retracted_work_json() -> serde_json::Value {
        serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1016/s0140-6736(97)11096-0",
                "title": ["Ileal-lymphoid-nodular hyperplasia"],
                "updated-by": [
                    {
                        "DOI": "10.1016/s0140-6736(04)15715-2",
                        "type": "correction",
                        "label": "Correction",
                        "source": "retraction-watch",
                        "updated": {
                            "date-parts": [[2004, 3, 6]],
                            "date-time": "2004-03-06T00:00:00Z",
                            "timestamp": 1078531200000_i64
                        },
                        "record-id": "17269"
                    },
                    {
                        "DOI": "10.1016/S0140-6736(10)60175-4",
                        "type": "retraction",
                        "label": "Retraction",
                        "source": "retraction-watch",
                        "updated": {"date-parts": [[2010, 2, 6]]},
                        "record-id": "4036"
                    }
                ]
            }
        })
    }

    #[test]
    fn convert_message_maps_updated_by_to_notices() {
        let raw: RawCrossrefResponse = serde_json::from_value(retracted_work_json()).unwrap();
        let work = convert_message("10.1016/s0140-6736(97)11096-0", raw.message);
        assert_eq!(
            work.notices,
            vec![
                WorkNotice {
                    notice_type: "correction".into(),
                    source: Some("retraction-watch".into()),
                    notice_doi: Some("10.1016/s0140-6736(04)15715-2".into()),
                    date: Some("2004-03-06".into()),
                },
                WorkNotice {
                    notice_type: "retraction".into(),
                    source: Some("retraction-watch".into()),
                    notice_doi: Some("10.1016/s0140-6736(10)60175-4".into()),
                    date: Some("2010-02-06".into()),
                },
            ]
        );
        assert!(work.openalex_only_retracted.is_none());
    }

    #[test]
    fn convert_notices_is_lenient_and_dedupes() {
        let raw = serde_json::json!([
            {"type": "Expression_of_Concern", "source": "publisher", "updated": {"date-parts": [[2021, 7]]}},
            {"type": "expression_of_concern", "source": "publisher", "updated": {"date-parts": [[2021, 7]]}},
            {"DOI": "10.1/no-type", "source": "publisher"},
            {"type": "  "},
            {"type": "retraction", "updated": {"date-parts": [[2019]]}},
            {"type": "withdrawal", "updated": {"date-parts": [[]]}}
        ]);
        let notices = convert_notices(Some(raw));
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0].notice_type, "expression_of_concern");
        assert_eq!(notices[0].date.as_deref(), Some("2021-07"));
        assert!(notices[0].notice_doi.is_none());
        assert_eq!(notices[1].notice_type, "retraction");
        assert_eq!(notices[1].date.as_deref(), Some("2019"));
        assert!(notices[1].source.is_none());
        assert_eq!(notices[2].notice_type, "withdrawal");
        assert!(notices[2].date.is_none());
    }

    #[test]
    fn malformed_updated_by_never_fails_the_message() {
        for updated_by in [
            serde_json::json!("not-an-array"),
            serde_json::json!({"type": "retraction"}),
            serde_json::json!(42),
            serde_json::json!([
                "string-entry",
                {"type": 7, "source": ["x"], "DOI": {"a": 1}},
                {"type": "retraction", "updated": {"date-parts": "2020"}},
                {"type": "correction", "updated": {"date-parts": [["2020", 1]]}},
                {"type": "removal", "updated": {"date-parts": [[2020, 13, 40]]}}
            ]),
        ] {
            let json = serde_json::json!({
                "message": {"DOI": "10.1/odd", "title": ["Odd"], "updated-by": updated_by}
            });
            let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
            let work = convert_message("10.1/odd", raw.message);
            assert_eq!(work.title.as_deref(), Some("Odd"));
            for notice in &work.notices {
                assert!(!notice.notice_type.is_empty());
            }
        }
        let raw = serde_json::json!([
            {"type": 7},
            {"type": "retraction", "updated": {"date-parts": "2020"}},
            {"type": "correction", "updated": {"date-parts": [["2020", 1]]}},
            {"type": "removal", "updated": {"date-parts": [[2020, 13, 40]]}}
        ]);
        let notices = convert_notices(Some(raw));
        let types: Vec<_> = notices.iter().map(|n| n.notice_type.as_str()).collect();
        assert_eq!(types, ["retraction", "correction", "removal"]);
        assert!(notices[0].date.is_none());
        assert!(notices[1].date.is_none());
        assert_eq!(notices[2].date.as_deref(), Some("2020"));
    }

    #[test]
    fn search_survives_malformed_updated_by() {
        let json = serde_json::json!({
            "message": {"items": [
                {"DOI": "10.1/a", "title": ["A"], "updated-by": "garbage"},
                {"DOI": "10.1/b", "title": ["B"], "updated-by": [{"type": false}]}
            ]}
        });
        let raw: RawCrossrefSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(raw.message.items.unwrap().len(), 2);
    }

    #[test]
    fn notice_types_normalize_and_classify() {
        assert_eq!(
            normalize_notice_type(" Partial-Retraction "),
            "partial_retraction"
        );
        assert_eq!(
            normalize_notice_type("Expression of Concern"),
            "expression_of_concern"
        );
        for t in ["retraction", "Partial-Retraction", "withdrawal", "REMOVAL"] {
            assert!(is_retraction_class(t), "{t}");
        }
        for t in [
            "correction",
            "erratum",
            "expression_of_concern",
            "new_version",
        ] {
            assert!(!is_retraction_class(t), "{t}");
        }
    }

    #[test]
    fn notices_are_capped_in_count_and_length() {
        let long = "x".repeat(10_000);
        let entries: Vec<_> = (0..100)
            .map(|i| serde_json::json!({"type": format!("correction{i}"), "source": long}))
            .collect();
        let notices = convert_notices(Some(serde_json::Value::Array(entries)));
        assert_eq!(notices.len(), MAX_NOTICES);
        assert!(
            notices
                .iter()
                .all(|n| n.source.as_ref().unwrap().chars().count() == MAX_NOTICE_FIELD_CHARS)
        );
    }

    #[test]
    fn cap_keeps_retraction_listed_last() {
        let mut entries: Vec<_> = (0..40)
            .map(|i| serde_json::json!({"type": "correction", "DOI": format!("10.1/c{i}")}))
            .collect();
        entries.push(serde_json::json!({"type": "Retraction", "DOI": "10.1/r"}));
        let notices = convert_notices(Some(serde_json::Value::Array(entries)));
        assert_eq!(notices.len(), MAX_NOTICES);
        assert_eq!(notices[0].notice_type, "retraction");
        assert_eq!(notices[0].notice_doi.as_deref(), Some("10.1/r"));
        // Remaining order is preserved.
        assert_eq!(notices[1].notice_doi.as_deref(), Some("10.1/c0"));
    }

    #[test]
    fn work_without_updated_by_omits_notices_on_the_wire() {
        let raw: RawCrossrefResponse = serde_json::from_value(serde_json::json!({
            "message": {"DOI": "10.1/clean", "updated-by": null}
        }))
        .unwrap();
        let work = convert_message("10.1/clean", raw.message);
        assert!(work.notices.is_empty());
        let value = serde_json::to_value(&work).unwrap();
        assert!(value.get("notices").is_none());
        assert!(value.get("openalex_only_retracted").is_none());
    }

    #[tokio::test]
    async fn resolve_doi_returns_notices_from_mocked_crossref() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex(r"^/works/10\.1016"))
            .respond_with(ResponseTemplate::new(200).set_body_json(retracted_work_json()))
            .expect(1)
            .mount(&server)
            .await;

        let client = CrossrefClient::new(Some(&server.uri()));
        let work = client
            .resolve_doi("10.1016/S0140-6736(97)11096-0")
            .await
            .unwrap();
        let value = serde_json::to_value(&work).unwrap();
        assert_eq!(value["notices"][1]["type"], "retraction");
        assert_eq!(value["notices"][1]["source"], "retraction-watch");
        assert_eq!(
            value["notices"][1]["notice_doi"],
            "10.1016/s0140-6736(10)60175-4"
        );
        assert_eq!(value["notices"][1]["date"], "2010-02-06");
    }

    #[tokio::test]
    async fn resolve_doi_rejects_non_doi_without_request() {
        let client = CrossrefClient::new(Some("http://127.0.0.1:9"));
        let err = client.resolve_doi("https://example.com/10.1/x").await;
        assert!(matches!(err, Err(ZoteroMcpError::InvalidInput(_))));
    }
}
