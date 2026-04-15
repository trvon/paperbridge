use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://www.ebi.ac.uk/europepmc/webservices/rest";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct EuropePmcClient {
    client: Client,
    base_url: String,
}

impl EuropePmcClient {
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
                "Europe PMC search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/search?query={encoded}&format=json&resultType=core&pageSize={limit}",
            self.base_url
        );

        let response = send_with_retry(self.client.get(&url)).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Europe PMC API error at {url}"),
            });
        }

        let raw: RawEpmcResponse = response.json().await?;
        Ok(raw
            .result_list
            .map(|rl| rl.result.into_iter().map(convert_result).collect())
            .unwrap_or_default())
    }
}

impl std::fmt::Debug for EuropePmcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EuropePmcClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawEpmcResponse {
    #[serde(rename = "resultList")]
    result_list: Option<RawEpmcResultList>,
}

#[derive(Debug, Deserialize)]
struct RawEpmcResultList {
    #[serde(default)]
    result: Vec<RawEpmcResult>,
}

#[derive(Debug, Deserialize)]
struct RawEpmcResult {
    title: Option<String>,
    #[serde(rename = "authorString")]
    author_string: Option<String>,
    #[serde(rename = "pubYear")]
    pub_year: Option<String>,
    doi: Option<String>,
    pmid: Option<String>,
    #[serde(rename = "journalTitle")]
    journal_title: Option<String>,
    #[serde(rename = "abstractText")]
    abstract_text: Option<String>,
    #[serde(rename = "fullTextUrlList")]
    full_text_url_list: Option<RawEpmcFullTextUrlList>,
    #[serde(rename = "isOpenAccess")]
    is_open_access: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEpmcFullTextUrlList {
    #[serde(rename = "fullTextUrl", default)]
    full_text_url: Vec<RawEpmcFullTextUrl>,
}

#[derive(Debug, Deserialize)]
struct RawEpmcFullTextUrl {
    url: Option<String>,
    #[serde(rename = "documentStyle")]
    document_style: Option<String>,
}

fn convert_result(r: RawEpmcResult) -> PaperHit {
    let authors = r
        .author_string
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let pdf_urls: Vec<String> = r
        .full_text_url_list
        .as_ref()
        .map(|list| {
            list.full_text_url
                .iter()
                .filter(|u| u.document_style.as_deref() == Some("pdf"))
                .filter_map(|u| u.url.clone())
                .collect()
        })
        .unwrap_or_default();

    let pdf_url = pdf_urls.first().cloned();
    let oa_pdf_url = if r.is_open_access.as_deref() == Some("Y") {
        pdf_url.clone()
    } else {
        None
    };

    let url = r.doi.as_deref().map(|d| format!("https://doi.org/{d}"));

    PaperHit {
        source: PaperSource::EuropePmc,
        title: r.title.unwrap_or_default(),
        authors,
        year: r.pub_year,
        doi: r.doi,
        arxiv_id: None,
        pmid: r.pmid,
        abstract_note: r.abstract_text,
        url,
        pdf_url,
        oa_pdf_url,
        venue: r.journal_title,
        citation_count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_result_extracts_fields() {
        let json = serde_json::json!({
            "title": "CRISPR study",
            "authorString": "Doudna J, Charpentier E",
            "pubYear": "2014",
            "doi": "10.1038/nature12373",
            "pmid": "23892897",
            "journalTitle": "Nature",
            "abstractText": "An abstract.",
            "isOpenAccess": "Y",
            "fullTextUrlList": {
                "fullTextUrl": [
                    {"url": "https://europepmc.org/article/MED/23892897", "documentStyle": "html"},
                    {"url": "https://europepmc.org/articles/PMC123/pdf", "documentStyle": "pdf"}
                ]
            }
        });
        let r: RawEpmcResult = serde_json::from_value(json).unwrap();
        let hit = convert_result(r);
        assert_eq!(hit.title, "CRISPR study");
        assert_eq!(hit.authors, vec!["Doudna J", "Charpentier E"]);
        assert_eq!(hit.year.as_deref(), Some("2014"));
        assert_eq!(hit.doi.as_deref(), Some("10.1038/nature12373"));
        assert_eq!(hit.pmid.as_deref(), Some("23892897"));
        assert_eq!(hit.venue.as_deref(), Some("Nature"));
        assert_eq!(
            hit.pdf_url.as_deref(),
            Some("https://europepmc.org/articles/PMC123/pdf")
        );
        assert_eq!(
            hit.oa_pdf_url.as_deref(),
            Some("https://europepmc.org/articles/PMC123/pdf")
        );
    }

    #[test]
    fn convert_result_handles_missing_fields() {
        let json = serde_json::json!({"title": "Minimal"});
        let r: RawEpmcResult = serde_json::from_value(json).unwrap();
        let hit = convert_result(r);
        assert_eq!(hit.title, "Minimal");
        assert!(hit.authors.is_empty());
        assert!(hit.doi.is_none());
        assert!(hit.pmid.is_none());
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = EuropePmcClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_search_endpoint() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "resultList": {
                "result": [{
                    "title": "Mock EPMC Paper",
                    "authorString": "Smith J",
                    "pubYear": "2024",
                    "doi": "10.1/epmc",
                    "pmid": "999"
                }]
            }
        });
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("query", "crispr"))
            .and(query_param("format", "json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let client = EuropePmcClient::new(Some(&server.uri()));
        let hits = client.search("crispr", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Mock EPMC Paper");
        assert_eq!(hits[0].pmid.as_deref(), Some("999"));
    }

    #[tokio::test]
    async fn search_retries_on_429_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resultList": {"result": [{"title": "Retried EPMC"}]}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = EuropePmcClient::new(Some(&server.uri()));
        let hits = client.search("q", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Retried EPMC");
    }
}
