use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct PubmedClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl PubmedClient {
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

    fn maybe_key(&self) -> String {
        match self.api_key.as_deref() {
            Some(k) => format!("&api_key={}", urlencoding::encode(k)),
            None => String::new(),
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "PubMed search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let key = self.maybe_key();
        let esearch_url = format!(
            "{}/esearch.fcgi?db=pubmed&term={encoded}&retmode=json&retmax={limit}{key}",
            self.base_url
        );

        let resp = send_with_retry(self.client.get(&esearch_url)).await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("PubMed esearch error at {esearch_url}"),
            });
        }
        let esearch: RawEsearchResponse = resp.json().await?;
        let ids = esearch.esearchresult.map(|r| r.idlist).unwrap_or_default();
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_csv = ids.join(",");
        let esummary_url = format!(
            "{}/esummary.fcgi?db=pubmed&id={id_csv}&retmode=json{key}",
            self.base_url
        );
        let resp2 = send_with_retry(self.client.get(&esummary_url)).await?;
        let status2 = resp2.status();
        if !status2.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status2.as_u16(),
                message: format!("PubMed esummary error at {esummary_url}"),
            });
        }
        let summary: RawEsummaryResponse = resp2.json().await?;
        let result = match summary.result {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };

        let order: Vec<String> = result.uids.clone().unwrap_or_else(|| ids.clone());

        let mut hits = Vec::new();
        for uid in &order {
            if let Some(serde_json::Value::Object(map)) = result.others.get(uid) {
                let doc: RawSummaryDoc =
                    serde_json::from_value(serde_json::Value::Object(map.clone()))
                        .unwrap_or_default();
                hits.push(convert_doc(uid.clone(), doc));
            }
        }
        Ok(hits)
    }
}

impl std::fmt::Debug for PubmedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PubmedClient")
            .field("base_url", &self.base_url)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawEsearchResponse {
    esearchresult: Option<RawEsearchResult>,
}

#[derive(Debug, Deserialize)]
struct RawEsearchResult {
    #[serde(default)]
    idlist: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawEsummaryResponse {
    result: Option<RawEsummaryResult>,
}

#[derive(Debug, Deserialize)]
struct RawEsummaryResult {
    uids: Option<Vec<String>>,
    #[serde(flatten)]
    others: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct RawSummaryDoc {
    title: Option<String>,
    #[serde(default)]
    authors: Vec<RawSummaryAuthor>,
    pubdate: Option<String>,
    fulljournalname: Option<String>,
    source: Option<String>,
    elocationid: Option<String>,
    #[serde(default)]
    articleids: Vec<RawArticleId>,
}

#[derive(Debug, Deserialize)]
struct RawSummaryAuthor {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawArticleId {
    idtype: Option<String>,
    value: Option<String>,
}

fn convert_doc(uid: String, d: RawSummaryDoc) -> PaperHit {
    let authors = d
        .authors
        .into_iter()
        .filter_map(|a| a.name)
        .filter(|n| !n.is_empty())
        .collect();

    let year = d.pubdate.as_deref().and_then(|p| {
        p.split_whitespace()
            .next()
            .filter(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()))
            .map(|s| s.to_string())
    });

    let mut doi: Option<String> = None;
    for a in &d.articleids {
        if a.idtype.as_deref() == Some("doi")
            && let Some(v) = a.value.as_deref()
            && !v.is_empty()
        {
            doi = Some(v.to_string());
            break;
        }
    }
    if doi.is_none()
        && let Some(eloc) = d.elocationid.as_deref()
        && let Some(stripped) = eloc.strip_prefix("doi: ")
    {
        doi = Some(stripped.to_string());
    }

    let venue = d.fulljournalname.or(d.source);
    let url = Some(format!("https://pubmed.ncbi.nlm.nih.gov/{uid}/"));

    PaperHit {
        source: PaperSource::Pubmed,
        title: d.title.unwrap_or_default(),
        authors,
        year,
        doi,
        arxiv_id: None,
        pmid: Some(uid),
        abstract_note: None,
        url,
        pdf_url: None,
        oa_pdf_url: None,
        venue,
        citation_count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_doc_extracts_fields() {
        let d = RawSummaryDoc {
            title: Some("PM Paper".to_string()),
            authors: vec![
                RawSummaryAuthor {
                    name: Some("Smith J".to_string()),
                },
                RawSummaryAuthor {
                    name: Some("Doe A".to_string()),
                },
            ],
            pubdate: Some("2024 Jan 5".to_string()),
            fulljournalname: Some("Nature".to_string()),
            source: None,
            elocationid: Some("doi: 10.1/pm".to_string()),
            articleids: vec![],
        };
        let hit = convert_doc("12345".to_string(), d);
        assert_eq!(hit.title, "PM Paper");
        assert_eq!(hit.authors, vec!["Smith J", "Doe A"]);
        assert_eq!(hit.year.as_deref(), Some("2024"));
        assert_eq!(hit.pmid.as_deref(), Some("12345"));
        assert_eq!(hit.doi.as_deref(), Some("10.1/pm"));
        assert_eq!(hit.venue.as_deref(), Some("Nature"));
        assert_eq!(
            hit.url.as_deref(),
            Some("https://pubmed.ncbi.nlm.nih.gov/12345/")
        );
        assert_eq!(hit.source, PaperSource::Pubmed);
    }

    #[test]
    fn convert_doc_prefers_articleids_doi() {
        let d = RawSummaryDoc {
            title: Some("P".to_string()),
            authors: vec![],
            pubdate: None,
            fulljournalname: None,
            source: None,
            elocationid: Some("doi: 10.1/old".to_string()),
            articleids: vec![RawArticleId {
                idtype: Some("doi".to_string()),
                value: Some("10.1/new".to_string()),
            }],
        };
        let hit = convert_doc("9".to_string(), d);
        assert_eq!(hit.doi.as_deref(), Some("10.1/new"));
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = PubmedClient::new(None, None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_does_two_step_esearch_then_esummary() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let esearch_body = serde_json::json!({
            "esearchresult": {"idlist": ["12345"]}
        });
        let esummary_body = serde_json::json!({
            "result": {
                "uids": ["12345"],
                "12345": {
                    "title": "PubMed Mock",
                    "authors": [{"name": "Jane"}],
                    "pubdate": "2024 Mar",
                    "fulljournalname": "Nature",
                    "articleids": [{"idtype": "doi", "value": "10.1/pm"}]
                }
            }
        });
        Mock::given(method("GET"))
            .and(path("/esearch.fcgi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(esearch_body))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/esummary.fcgi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(esummary_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = PubmedClient::new(Some(&server.uri()), None);
        let hits = client.search("crispr", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "PubMed Mock");
        assert_eq!(hits[0].pmid.as_deref(), Some("12345"));
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/pm"));
    }

    #[tokio::test]
    async fn search_returns_empty_when_no_ids() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/esearch.fcgi"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"esearchresult": {"idlist": []}})),
            )
            .mount(&server)
            .await;

        let client = PubmedClient::new(Some(&server.uri()), None);
        let hits = client.search("xyz", 5).await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn search_retries_on_429_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // esearch returns 429 once, then the id list.
        Mock::given(method("GET"))
            .and(path("/esearch.fcgi"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/esearch.fcgi"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"esearchresult": {"idlist": ["42"]}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/esummary.fcgi"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {
                    "uids": ["42"],
                    "42": {"title": "Retried PM"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = PubmedClient::new(Some(&server.uri()), None);
        let hits = client.search("q", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Retried PM");
    }

    #[tokio::test]
    async fn search_handles_malformed_json() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/esearch.fcgi"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&server)
            .await;

        let client = PubmedClient::new(Some(&server.uri()), None);
        let err = client.search("q", 1).await.unwrap_err();
        assert!(
            matches!(err, ZoteroMcpError::Http(_) | ZoteroMcpError::Serde(_)),
            "expected decode error, got {err:?}"
        );
    }
}
