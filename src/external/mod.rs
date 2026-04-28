pub mod ads;
pub mod arxiv;
pub mod core;
pub mod dblp;
pub mod europe_pmc;
pub mod huggingface;
pub mod openalex;
pub mod openreview;
pub mod pubmed;
pub mod scholarapi;
pub mod semantic_scholar;
pub mod unpaywall;

pub use ads::AdsClient;
pub use arxiv::ArxivClient;
pub use core::CoreClient;
pub use dblp::DblpClient;
pub use europe_pmc::EuropePmcClient;
pub use huggingface::HuggingFaceClient;
pub use openalex::OpenAlexClient;
pub use openreview::OpenReviewClient;
pub use pubmed::PubmedClient;
pub use scholarapi::ScholarApiClient;
pub use semantic_scholar::SemanticScholarClient;
pub use unpaywall::UnpaywallClient;

use crate::crossref::CrossrefClient;
use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use futures::future::BoxFuture;
use futures::future::FutureExt;
use reqwest::header::{HeaderMap, RETRY_AFTER};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::timeout;

const DEFAULT_LIMIT_PER_SOURCE: u32 = 10;
const DEFAULT_TIMEOUT_MS: u64 = 8000;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub limit_per_source: u32,
    pub sources: Option<Vec<PaperSource>>,
    pub timeout_ms: u64,
}

impl SearchOptions {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit_per_source: DEFAULT_LIMIT_PER_SOURCE,
            sources: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    fn enabled(&self, source: PaperSource) -> bool {
        match &self.sources {
            None => true,
            Some(v) => v.contains(&source),
        }
    }
}

#[derive(Clone)]
pub struct PaperSearch {
    arxiv: ArxivClient,
    hf: Option<HuggingFaceClient>,
    s2: Option<SemanticScholarClient>,
    crossref: CrossrefClient,
    openalex: OpenAlexClient,
    europe_pmc: EuropePmcClient,
    dblp: DblpClient,
    openreview: OpenReviewClient,
    core: Option<CoreClient>,
    ads: Option<AdsClient>,
    pubmed: PubmedClient,
    scholarapi: Option<ScholarApiClient>,
}

#[derive(Default, Clone)]
pub struct PaperSearchKeys {
    pub hf_token: Option<String>,
    pub s2_api_key: Option<String>,
    pub core_api_key: Option<String>,
    pub ads_api_token: Option<String>,
    pub ncbi_api_key: Option<String>,
    pub scholarapi_key: Option<String>,
    pub unpaywall_email: Option<String>,
}

impl PaperSearch {
    pub fn new() -> Self {
        Self::with_keys_struct(PaperSearchKeys::default())
    }

    pub fn with_keys(hf_token: Option<String>, s2_api_key: Option<String>) -> Self {
        Self::with_keys_struct(PaperSearchKeys {
            hf_token,
            s2_api_key,
            ..PaperSearchKeys::default()
        })
    }

    pub fn with_keys_struct(keys: PaperSearchKeys) -> Self {
        Self {
            arxiv: ArxivClient::new(None),
            hf: keys.hf_token.map(|t| HuggingFaceClient::new(None, Some(t))),
            s2: keys
                .s2_api_key
                .map(|k| SemanticScholarClient::new(None, Some(k))),
            crossref: CrossrefClient::new(None),
            openalex: OpenAlexClient::new(None, keys.unpaywall_email.clone()),
            europe_pmc: EuropePmcClient::new(None),
            dblp: DblpClient::new(None),
            openreview: OpenReviewClient::new(None),
            core: keys.core_api_key.map(|k| CoreClient::new(None, k)),
            ads: keys.ads_api_token.map(|k| AdsClient::new(None, k)),
            pubmed: PubmedClient::new(None, keys.ncbi_api_key),
            scholarapi: keys.scholarapi_key.map(|k| ScholarApiClient::new(None, k)),
        }
    }

    pub fn with_clients(
        arxiv: ArxivClient,
        hf: HuggingFaceClient,
        s2: SemanticScholarClient,
        crossref: CrossrefClient,
    ) -> Self {
        Self {
            arxiv,
            hf: Some(hf),
            s2: Some(s2),
            crossref,
            openalex: OpenAlexClient::new(None, None),
            europe_pmc: EuropePmcClient::new(None),
            dblp: DblpClient::new(None),
            openreview: OpenReviewClient::new(None),
            core: None,
            ads: None,
            pubmed: PubmedClient::new(None, None),
            scholarapi: None,
        }
    }

    pub async fn search(&self, opts: SearchOptions) -> Result<Vec<PaperHit>> {
        let timeout_duration = Duration::from_millis(opts.timeout_ms);
        let limit = opts.limit_per_source;
        let query = opts.query.clone();

        let mut futs: Vec<BoxFuture<'_, Vec<PaperHit>>> = Vec::new();

        // Always-on sources
        futs.push(
            run_source(
                PaperSource::SemanticScholar,
                opts.enabled(PaperSource::SemanticScholar),
                timeout_duration,
                async {
                    match self.s2.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => {
                            tracing::debug!(
                                source = ?PaperSource::SemanticScholar,
                                reason = "no semantic_scholar_api_key/SEMANTIC_SCHOLAR_API_KEY configured",
                                "source skipped"
                            );
                            Ok(Vec::new())
                        }
                    }
                },
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Crossref,
                opts.enabled(PaperSource::Crossref),
                timeout_duration,
                self.crossref.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::HuggingFace,
                opts.enabled(PaperSource::HuggingFace),
                timeout_duration,
                async {
                    match self.hf.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => {
                            tracing::debug!(
                                source = ?PaperSource::HuggingFace,
                                reason = "no hf_token/HF_TOKEN configured",
                                "source skipped"
                            );
                            Ok(Vec::new())
                        }
                    }
                },
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Arxiv,
                opts.enabled(PaperSource::Arxiv),
                timeout_duration,
                self.arxiv.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::OpenAlex,
                opts.enabled(PaperSource::OpenAlex),
                timeout_duration,
                self.openalex.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::EuropePmc,
                opts.enabled(PaperSource::EuropePmc),
                timeout_duration,
                self.europe_pmc.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Dblp,
                opts.enabled(PaperSource::Dblp),
                timeout_duration,
                self.dblp.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::OpenReview,
                opts.enabled(PaperSource::OpenReview),
                timeout_duration,
                self.openreview.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Core,
                opts.enabled(PaperSource::Core),
                timeout_duration,
                async {
                    match self.core.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => {
                            tracing::debug!(
                                source = ?PaperSource::Core,
                                reason = "no core_api_key/CORE_API_KEY configured",
                                "source skipped"
                            );
                            Ok(Vec::new())
                        }
                    }
                },
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Ads,
                opts.enabled(PaperSource::Ads),
                timeout_duration,
                async {
                    match self.ads.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => {
                            tracing::debug!(
                                source = ?PaperSource::Ads,
                                reason = "no ads_api_token/ADS_API_TOKEN configured",
                                "source skipped"
                            );
                            Ok(Vec::new())
                        }
                    }
                },
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Pubmed,
                opts.enabled(PaperSource::Pubmed),
                timeout_duration,
                self.pubmed.search(&query, limit),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::ScholarApi,
                opts.enabled(PaperSource::ScholarApi),
                timeout_duration,
                async {
                    match self.scholarapi.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => {
                            tracing::debug!(
                                source = ?PaperSource::ScholarApi,
                                reason = "no scholarapi_key/SCHOLARAPI_KEY configured",
                                "source skipped"
                            );
                            Ok(Vec::new())
                        }
                    }
                },
            )
            .boxed(),
        );

        let results = futures::future::join_all(futs).await;
        let merged: Vec<PaperHit> = results.into_iter().flatten().collect();
        Ok(dedupe(merged))
    }
}

impl Default for PaperSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PaperSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaperSearch").finish()
    }
}

async fn run_source<F>(source: PaperSource, enabled: bool, dur: Duration, fut: F) -> Vec<PaperHit>
where
    F: std::future::Future<Output = Result<Vec<PaperHit>>>,
{
    if !enabled {
        return Vec::new();
    }
    match timeout(dur, fut).await {
        Ok(Ok(hits)) => hits,
        Ok(Err(ZoteroMcpError::Api {
            status: 429,
            message,
        })) => {
            tracing::debug!(?source, status = 429, reason = "rate_limited", %message, "source rate-limited after retry");
            Vec::new()
        }
        Ok(Err(e)) => {
            tracing::debug!(?source, error = %e, "source search failed");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!(?source, "source search timed out");
            Vec::new()
        }
    }
}

const RETRY_AFTER_CAP_MS: u64 = 2000;
const RETRY_AFTER_DEFAULT_MS: u64 = 500;

/// Send a request, retrying once on 429/503 while respecting the `Retry-After`
/// header. Retry delay is clamped to [0, 2000] ms so one slow provider can't
/// starve the parallel fan-out. Non-429/503 responses (including 2xx) are
/// returned unchanged on the first attempt.
pub(crate) async fn send_with_retry(req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    let retry_req = req
        .try_clone()
        .ok_or_else(|| ZoteroMcpError::Http("request not cloneable for retry".into()))?;
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    if status != 429 && status != 503 {
        return Ok(resp);
    }
    let wait_ms = parse_retry_after_ms(resp.headers())
        .unwrap_or(RETRY_AFTER_DEFAULT_MS)
        .min(RETRY_AFTER_CAP_MS);
    tracing::debug!(status, wait_ms, "rate-limit response; retrying once");
    // Drain the first response body so the connection can be reused.
    let _ = resp.bytes().await;
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    Ok(retry_req.send().await?)
}

fn parse_retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    let raw = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    // Integer-seconds form (e.g. "Retry-After: 30") is what all major providers send;
    // HTTP-date form is accepted too but not parsed — we fall through to the default.
    raw.parse::<u64>()
        .ok()
        .map(|secs| secs.saturating_mul(1000))
}

fn dedupe(hits: Vec<PaperHit>) -> Vec<PaperHit> {
    let mut seen_doi: HashSet<String> = HashSet::new();
    let mut seen_arxiv: HashSet<String> = HashSet::new();
    let mut seen_pmid: HashSet<String> = HashSet::new();
    let mut seen_titlekey: HashSet<String> = HashSet::new();
    let mut out: Vec<PaperHit> = Vec::with_capacity(hits.len());

    for hit in hits {
        if let Some(doi) = hit.doi.as_deref() {
            let key = doi.trim().to_ascii_lowercase();
            if !key.is_empty() && !seen_doi.insert(key) {
                continue;
            }
        }
        if let Some(arxiv) = hit.arxiv_id.as_deref() {
            let key = strip_arxiv_version(arxiv).to_ascii_lowercase();
            if !key.is_empty() && !seen_arxiv.insert(key) {
                continue;
            }
        }
        if let Some(pmid) = hit.pmid.as_deref() {
            let key = pmid.trim().to_string();
            if !key.is_empty() && !seen_pmid.insert(key) {
                continue;
            }
        }
        let title_key = title_authors_key(&hit);
        if !title_key.is_empty() && !seen_titlekey.insert(title_key) {
            continue;
        }
        out.push(hit);
    }

    out
}

fn strip_arxiv_version(id: &str) -> String {
    if let Some(idx) = id.rfind('v') {
        let (base, ver) = id.split_at(idx);
        if ver.len() > 1 && ver[1..].chars().all(|c| c.is_ascii_digit()) {
            return base.to_string();
        }
    }
    id.to_string()
}

fn title_authors_key(hit: &PaperHit) -> String {
    let title_norm: String = hit
        .title
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let first_author_norm: String = hit
        .authors
        .first()
        .map(|a| a.trim().to_ascii_lowercase())
        .unwrap_or_default();

    if title_norm.is_empty() {
        String::new()
    } else {
        format!("{title_norm}||{first_author_norm}")
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use reqwest::Client;

    #[test]
    fn parse_retry_after_accepts_integer_seconds() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(2000));
    }

    #[test]
    fn parse_retry_after_rejects_http_date() {
        let mut h = HeaderMap::new();
        h.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[test]
    fn parse_retry_after_returns_none_when_missing() {
        let h = HeaderMap::new();
        assert_eq!(parse_retry_after_ms(&h), None);
    }

    #[tokio::test]
    async fn send_with_retry_passes_through_200() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = send_with_retry(client.get(server.uri())).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_retries_once_after_429_then_succeeds() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = send_with_retry(client.get(server.uri())).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_returns_429_after_second_attempt() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .expect(2)
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = send_with_retry(client.get(server.uri())).await.unwrap();
        assert_eq!(resp.status(), 429);
    }

    #[tokio::test]
    async fn send_with_retry_retries_on_503() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = send_with_retry(client.get(server.uri())).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn send_with_retry_does_not_retry_on_500() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let resp = send_with_retry(client.get(server.uri())).await.unwrap();
        assert_eq!(resp.status(), 500);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(
        source: PaperSource,
        title: &str,
        doi: Option<&str>,
        arxiv: Option<&str>,
        author: Option<&str>,
    ) -> PaperHit {
        PaperHit {
            source,
            title: title.to_string(),
            authors: author.map(|a| vec![a.to_string()]).unwrap_or_default(),
            year: None,
            doi: doi.map(|s| s.to_string()),
            arxiv_id: arxiv.map(|s| s.to_string()),
            pmid: None,
            abstract_note: None,
            url: None,
            pdf_url: None,
            oa_pdf_url: None,
            venue: None,
            citation_count: None,
        }
    }

    fn mk_pmid(source: PaperSource, title: &str, pmid: &str) -> PaperHit {
        let mut h = mk(source, title, None, None, None);
        h.pmid = Some(pmid.to_string());
        h
    }

    #[test]
    fn dedupe_by_doi_keeps_first() {
        let hits = vec![
            mk(
                PaperSource::SemanticScholar,
                "Paper A",
                Some("10.1/a"),
                None,
                None,
            ),
            mk(PaperSource::Crossref, "Paper A", Some("10.1/a"), None, None),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, PaperSource::SemanticScholar);
    }

    #[test]
    fn dedupe_by_arxiv_id_strips_version() {
        let hits = vec![
            mk(PaperSource::Arxiv, "X", None, Some("1706.03762"), None),
            mk(
                PaperSource::HuggingFace,
                "X",
                None,
                Some("1706.03762v2"),
                None,
            ),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, PaperSource::Arxiv);
    }

    #[test]
    fn dedupe_by_pmid_keeps_first() {
        let hits = vec![
            mk_pmid(PaperSource::EuropePmc, "Paper A", "12345"),
            mk_pmid(PaperSource::Pubmed, "Different title", "12345"),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, PaperSource::EuropePmc);
    }

    #[test]
    fn dedupe_by_title_author_fallback() {
        let hits = vec![
            mk(
                PaperSource::SemanticScholar,
                "Attention Is All You Need",
                None,
                None,
                Some("Vaswani"),
            ),
            mk(
                PaperSource::Arxiv,
                "Attention is all you need!",
                None,
                None,
                Some("Vaswani"),
            ),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn dedupe_keeps_distinct_hits() {
        let hits = vec![
            mk(
                PaperSource::SemanticScholar,
                "Paper A",
                Some("10.1/a"),
                None,
                None,
            ),
            mk(PaperSource::Crossref, "Paper B", Some("10.1/b"), None, None),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn search_with_keys_none_skips_hf_and_s2() {
        let search = PaperSearch::with_keys(None, None);
        assert!(search.hf.is_none());
        assert!(search.s2.is_none());
        let hits = search
            .search(SearchOptions {
                query: "x".to_string(),
                limit_per_source: 1,
                sources: Some(vec![PaperSource::HuggingFace, PaperSource::SemanticScholar]),
                timeout_ms: 200,
            })
            .await
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_options_enabled_respects_scope() {
        let mut opts = SearchOptions::new("q");
        assert!(opts.enabled(PaperSource::Arxiv));
        opts.sources = Some(vec![PaperSource::Crossref]);
        assert!(opts.enabled(PaperSource::Crossref));
        assert!(!opts.enabled(PaperSource::Arxiv));
    }

    #[tokio::test]
    async fn search_tolerates_partial_source_failures() {
        use std::time::Duration as StdDuration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Semantic Scholar: OK with one hit (DOI 10.1/shared)
        let s2_body = serde_json::json!({
            "data": [{
                "title": "Shared Paper",
                "authors": [{"name": "Author One"}],
                "year": 2024,
                "externalIds": {"DOI": "10.1/shared"}
            }]
        });
        Mock::given(method("GET"))
            .and(path("/paper/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(s2_body))
            .mount(&server)
            .await;

        // Crossref: OK with a dup of the S2 hit (same DOI) and one unique
        let crossref_body = serde_json::json!({
            "message": {
                "items": [
                    {"DOI": "10.1/shared", "title": ["Shared Paper"]},
                    {"DOI": "10.1/unique", "title": ["Unique Paper"]}
                ]
            }
        });
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(crossref_body))
            .mount(&server)
            .await;

        // HuggingFace: 500 failure — should contribute nothing
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        // arXiv: delay past the timeout — should contribute nothing
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<feed xmlns=\"http://www.w3.org/2005/Atom\"></feed>")
                    .set_delay(StdDuration::from_millis(800)),
            )
            .mount(&server)
            .await;

        let base = server.uri();
        let arxiv = ArxivClient::new(Some(&base));
        let hf = HuggingFaceClient::new(Some(&base), None);
        let s2 = SemanticScholarClient::new(Some(&base), None);
        let crossref = CrossrefClient::new(Some(&base));
        let paper_search = PaperSearch::with_clients(arxiv, hf, s2, crossref);

        let opts = SearchOptions {
            query: "quantum".to_string(),
            limit_per_source: 5,
            sources: Some(vec![
                PaperSource::SemanticScholar,
                PaperSource::Crossref,
                PaperSource::HuggingFace,
                PaperSource::Arxiv,
            ]),
            timeout_ms: 200,
        };
        let hits = paper_search.search(opts).await.unwrap();

        // S2 (Shared) + Crossref (Unique). Crossref's duplicate Shared dropped by DOI dedup.
        assert_eq!(hits.len(), 2, "got {:?}", hits);
        assert_eq!(hits[0].source, PaperSource::SemanticScholar);
        assert_eq!(hits[0].doi.as_deref(), Some("10.1/shared"));
        assert_eq!(hits[1].source, PaperSource::Crossref);
        assert_eq!(hits[1].doi.as_deref(), Some("10.1/unique"));
    }

    #[tokio::test]
    async fn router_isolates_rate_limited_source() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Semantic Scholar: persistent 429 — should drop out entirely after retry.
        Mock::given(method("GET"))
            .and(path("/paper/search"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .mount(&server)
            .await;

        // Crossref: happy path, contributes one hit.
        Mock::given(method("GET"))
            .and(path("/works"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {"items": [{"DOI": "10.1/crossref", "title": ["Crossref OK"]}]}
            })))
            .mount(&server)
            .await;

        // HuggingFace: happy path, contributes one hit.
        Mock::given(method("GET"))
            .and(path("/papers/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"paper": {"id": "2401.42", "title": "HF OK"}}
            ])))
            .mount(&server)
            .await;

        // arXiv fallback (catchall): empty feed.
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<feed xmlns=\"http://www.w3.org/2005/Atom\"></feed>"),
            )
            .mount(&server)
            .await;

        let base = server.uri();
        let arxiv = ArxivClient::new(Some(&base));
        let hf = HuggingFaceClient::new(Some(&base), None);
        let s2 = SemanticScholarClient::new(Some(&base), None);
        let crossref = CrossrefClient::new(Some(&base));
        let paper_search = PaperSearch::with_clients(arxiv, hf, s2, crossref);

        let opts = SearchOptions {
            query: "q".to_string(),
            limit_per_source: 5,
            sources: Some(vec![
                PaperSource::SemanticScholar,
                PaperSource::Crossref,
                PaperSource::HuggingFace,
                PaperSource::Arxiv,
            ]),
            timeout_ms: 4000,
        };
        let hits = paper_search.search(opts).await.unwrap();

        // S2 was rate-limited; Crossref + HF should still contribute.
        assert_eq!(hits.len(), 2, "got {:?}", hits);
        let sources: Vec<_> = hits.iter().map(|h| h.source).collect();
        assert!(sources.contains(&PaperSource::Crossref));
        assert!(sources.contains(&PaperSource::HuggingFace));
        assert!(!sources.contains(&PaperSource::SemanticScholar));
    }
}
