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
use crate::models::{
    PaperHit, PaperSource, SearchCacheMode, SearchDetail, SearchDiagnostics, SourceDiagnostic,
};
use crate::request_router::{RoutedResponse, global_request_router};
use futures::future::BoxFuture;
use futures::future::FutureExt;
use std::time::Duration;
use tokio::time::timeout;

const DEFAULT_LIMIT_PER_SOURCE: u32 = 10;
const DEFAULT_TIMEOUT_MS: u64 = 8000;
/// Default page size for agent-facing search (never unbounded).
pub const DEFAULT_PAGE_LIMIT: u32 = 10;
pub const MAX_PAGE_LIMIT: u32 = 50;
pub const MAX_SOURCE_FETCH_LIMIT: u32 = 200;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub limit_per_source: u32,
    pub sources: Option<Vec<PaperSource>>,
    pub timeout_ms: u64,
    pub offset: u32,
    /// Page size. 0 is treated as [`DEFAULT_PAGE_LIMIT`] (not "all").
    pub limit: u32,
    pub cache_mode: SearchCacheMode,
    pub detail: SearchDetail,
    /// When set, truncate abstracts to this many chars (full detail). None = default 280 in compact.
    pub abstract_max_chars: Option<usize>,
}

impl SearchOptions {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit_per_source: DEFAULT_LIMIT_PER_SOURCE,
            sources: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            offset: 0,
            limit: DEFAULT_PAGE_LIMIT,
            cache_mode: SearchCacheMode::Auto,
            detail: SearchDetail::Compact,
            abstract_max_chars: None,
        }
    }

    pub fn page_limit(&self) -> u32 {
        let lim = if self.limit == 0 {
            DEFAULT_PAGE_LIMIT
        } else {
            self.limit
        };
        lim.min(MAX_PAGE_LIMIT)
    }

    /// Fixed per-source candidate prefix for bounded stateless pagination.
    /// Offset/page size must not expand this pool: reranking a larger prefix
    /// between pages can repeat or skip hits. Upstream changes can still reorder it.
    pub fn source_fetch_limit(&self) -> u32 {
        if self.limit_per_source == 0 {
            DEFAULT_LIMIT_PER_SOURCE
        } else {
            self.limit_per_source
        }
    }

    pub fn validate_source_fetch_limit(&self) -> Result<()> {
        let fetch_limit = self.source_fetch_limit();
        if fetch_limit > MAX_SOURCE_FETCH_LIMIT {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "limit_per_source is {fetch_limit}, above the safe maximum of {MAX_SOURCE_FETCH_LIMIT}. Use a smaller limit_per_source."
            )));
        }
        Ok(())
    }

    fn enabled(&self, source: PaperSource) -> bool {
        match &self.sources {
            None => true,
            Some(v) => v.contains(&source),
        }
    }
}

/// Outcome of a multi-source external search.
#[derive(Debug, Clone, Default)]
pub struct PaperSearchOutcome {
    pub hits: Vec<PaperHit>,
    pub diagnostics: SearchDiagnostics,
}

#[derive(Debug, Clone)]
enum SourceRunResult {
    /// Source was not in the enabled set for this request.
    Disabled,
    Ok(Vec<PaperHit>),
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
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

    pub async fn search(&self, opts: SearchOptions) -> Result<PaperSearchOutcome> {
        opts.validate_source_fetch_limit()?;
        let timeout_duration = Duration::from_millis(opts.timeout_ms);
        let limit = opts.source_fetch_limit();
        let query = opts.query.clone();

        let mut futs: Vec<BoxFuture<'_, (PaperSource, SourceRunResult)>> = Vec::new();

        futs.push(
            run_source(
                PaperSource::SemanticScholar,
                opts.enabled(PaperSource::SemanticScholar),
                timeout_duration,
                async {
                    match self.s2.as_ref() {
                        Some(c) => c.search(&query, limit).await,
                        None => Err(ZoteroMcpError::MissingConfig(
                            "no semantic_scholar_api_key/SEMANTIC_SCHOLAR_API_KEY configured"
                                .into(),
                        )),
                    }
                },
                self.s2.is_none(),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Crossref,
                opts.enabled(PaperSource::Crossref),
                timeout_duration,
                self.crossref.search(&query, limit),
                false,
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
                        None => Err(ZoteroMcpError::MissingConfig(
                            "no hf_token/HF_TOKEN configured".into(),
                        )),
                    }
                },
                self.hf.is_none(),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Arxiv,
                opts.enabled(PaperSource::Arxiv),
                timeout_duration,
                self.arxiv.search(&query, limit),
                false,
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::OpenAlex,
                opts.enabled(PaperSource::OpenAlex),
                timeout_duration,
                self.openalex.search(&query, limit),
                false,
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::EuropePmc,
                opts.enabled(PaperSource::EuropePmc),
                timeout_duration,
                self.europe_pmc.search(&query, limit),
                false,
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Dblp,
                opts.enabled(PaperSource::Dblp),
                timeout_duration,
                self.dblp.search(&query, limit),
                false,
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::OpenReview,
                opts.enabled(PaperSource::OpenReview),
                timeout_duration,
                self.openreview.search(&query, limit),
                false,
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
                        None => Err(ZoteroMcpError::MissingConfig(
                            "no core_api_key/CORE_API_KEY configured".into(),
                        )),
                    }
                },
                self.core.is_none(),
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
                        None => Err(ZoteroMcpError::MissingConfig(
                            "no ads_api_token/ADS_API_TOKEN configured".into(),
                        )),
                    }
                },
                self.ads.is_none(),
            )
            .boxed(),
        );
        futs.push(
            run_source(
                PaperSource::Pubmed,
                opts.enabled(PaperSource::Pubmed),
                timeout_duration,
                self.pubmed.search(&query, limit),
                false,
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
                        None => Err(ZoteroMcpError::MissingConfig(
                            "no scholarapi_key/SCHOLARAPI_KEY configured".into(),
                        )),
                    }
                },
                self.scholarapi.is_none(),
            )
            .boxed(),
        );

        let results = futures::future::join_all(futs).await;
        let mut diagnostics = SearchDiagnostics::default();
        let mut merged: Vec<PaperHit> = Vec::new();
        for (source, outcome) in results {
            let name = source_wire_name(source);
            match outcome {
                SourceRunResult::Disabled => {}
                SourceRunResult::Ok(hits) => {
                    diagnostics.sources_ok.push(name);
                    merged.extend(hits.into_iter().take(limit as usize));
                }
                SourceRunResult::Skipped { reason } => {
                    diagnostics.sources_skipped.push(SourceDiagnostic {
                        source: name,
                        reason,
                    });
                }
                SourceRunResult::Failed { reason } => {
                    diagnostics.sources_failed.push(SourceDiagnostic {
                        source: name,
                        reason,
                    });
                }
            }
        }
        Ok(PaperSearchOutcome {
            hits: dedupe(merged),
            diagnostics,
        })
    }

    /// Resolve an exact DOI through the same Crossref client used by the
    /// search fan-out. This keeps test/custom endpoints and production
    /// behavior aligned.
    pub async fn resolve_doi(&self, doi: &str) -> Result<crate::models::CrossrefWork> {
        self.crossref.resolve_doi(doi).await
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

fn source_wire_name(source: PaperSource) -> String {
    serde_json::to_value(source)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{source:?}").to_ascii_lowercase())
}

async fn run_source<F>(
    source: PaperSource,
    enabled: bool,
    dur: Duration,
    fut: F,
    missing_key: bool,
) -> (PaperSource, SourceRunResult)
where
    F: std::future::Future<Output = Result<Vec<PaperHit>>>,
{
    if !enabled {
        return (source, SourceRunResult::Disabled);
    }
    if missing_key {
        return (
            source,
            SourceRunResult::Skipped {
                reason: "missing_api_key".into(),
            },
        );
    }
    match timeout(dur, fut).await {
        Ok(Ok(hits)) => (source, SourceRunResult::Ok(hits)),
        Ok(Err(ZoteroMcpError::MissingConfig(reason))) => {
            let reason = crate::error::sanitize_message(&reason, &[]);
            tracing::debug!(?source, %reason, "source skipped");
            (source, SourceRunResult::Skipped { reason })
        }
        Ok(Err(ZoteroMcpError::Api {
            status: 429,
            message,
        })) => {
            let reason = crate::error::sanitize_message(&format!("rate_limited: {message}"), &[]);
            tracing::debug!(?source, status = 429, %reason, "source rate-limited after retry");
            (source, SourceRunResult::Failed { reason })
        }
        Ok(Err(e)) => {
            let reason = crate::error::sanitize_message(&e.to_string(), &[]);
            tracing::debug!(?source, %reason, "source search failed");
            (source, SourceRunResult::Failed { reason })
        }
        Err(_) => {
            tracing::debug!(?source, "source search timed out");
            (
                source,
                SourceRunResult::Failed {
                    reason: "timeout".into(),
                },
            )
        }
    }
}

/// Route an idempotent external request through shared global/per-origin
/// concurrency limits and one bounded retry for 429/503 responses.
pub(crate) async fn send_with_retry(
    component: &'static str,
    req: reqwest::RequestBuilder,
) -> Result<RoutedResponse> {
    global_request_router().send(component, req).await
}

fn dedupe(hits: Vec<PaperHit>) -> Vec<PaperHit> {
    let mut out: Vec<PaperHit> = Vec::with_capacity(hits.len());
    for hit in hits {
        if let Some(mut index) = out
            .iter()
            .position(|existing| compatible_identity(existing, &hit))
        {
            merge_hit_metadata(&mut out[index], hit);
            // Promoted IDs can connect records that previously had no shared ID.
            // Recheck against the merged evidence; contradictions still veto.
            let mut candidate = 0;
            while candidate < out.len() {
                if candidate != index && compatible_identity(&out[index], &out[candidate]) {
                    let kept = index.min(candidate);
                    let other = out.remove(index.max(candidate));
                    merge_hit_metadata(&mut out[kept], other);
                    index = kept;
                    candidate = 0;
                } else {
                    candidate += 1;
                }
            }
        } else {
            out.push(hit);
        }
    }
    out
}

/// Strong-ID contradictions veto even a matching title or another shared ID.
/// Without a shared ID, require both title and a nonempty author to corroborate.
pub(crate) fn compatible_identity(left: &PaperHit, right: &PaperHit) -> bool {
    let pairs = [
        (doi_key(left), doi_key(right)),
        (arxiv_key(left), arxiv_key(right)),
        (pmid_key(left), pmid_key(right)),
    ];
    if pairs
        .iter()
        .any(|(a, b)| matches!((a, b), (Some(a), Some(b)) if a != b))
    {
        return false;
    }
    pairs.iter().any(|(a, b)| a.is_some() && a == b)
        || title_author_key(left).is_some_and(|key| Some(key) == title_author_key(right))
}

/// Keep the preferred source/cached record while promoting complementary metadata.
pub(crate) fn merge_hit_metadata(kept: &mut PaperHit, other: PaperHit) {
    kept.doi = kept
        .doi
        .take()
        .filter(|s| !s.trim().is_empty())
        .or(other.doi);
    kept.arxiv_id = kept
        .arxiv_id
        .take()
        .filter(|s| !s.trim().is_empty())
        .or(other.arxiv_id);
    kept.pmid = kept
        .pmid
        .take()
        .filter(|s| !s.trim().is_empty())
        .or(other.pmid);
    kept.year = kept.year.take().or(other.year);
    kept.abstract_note = kept.abstract_note.take().or(other.abstract_note);
    kept.url = kept.url.take().or(other.url);
    // Local file paths remain reachable through cache.paper_id; retain the
    // externally usable PDF URL instead when the preferred hit only has a path.
    if !kept
        .pdf_url
        .as_deref()
        .is_some_and(crate::hit_enrich::usable_http_url)
        && other
            .pdf_url
            .as_deref()
            .is_some_and(crate::hit_enrich::usable_http_url)
    {
        kept.pdf_url = other.pdf_url;
    } else {
        kept.pdf_url = kept.pdf_url.take().or(other.pdf_url);
    }
    kept.oa_pdf_url = kept.oa_pdf_url.take().or(other.oa_pdf_url);
    kept.venue = kept.venue.take().or(other.venue);
    kept.citation_count = kept.citation_count.max(other.citation_count);
    kept.cache = kept.cache.take().or(other.cache);
    kept.relevance_score = kept.relevance_score.or(other.relevance_score);
    for author in other.authors {
        if !kept
            .authors
            .iter()
            .any(|a| normalize_text_key(a) == normalize_text_key(&author))
        {
            kept.authors.push(author);
        }
    }
    if let Some(access) = other.access {
        if let Some(existing) = kept.access.as_mut() {
            existing.pdf |= access.pdf;
            existing.cached |= access.cached;
            existing.full_text |= access.full_text;
            existing.content_state = existing.content_state.or(access.content_state);
        } else {
            kept.access = Some(access);
        }
    }
}

pub(crate) fn doi_key(hit: &PaperHit) -> Option<String> {
    hit.doi.as_deref().and_then(normalize_doi_key)
}

pub(crate) fn arxiv_key(hit: &PaperHit) -> Option<String> {
    hit.arxiv_id
        .as_deref()
        .map(|a| strip_arxiv_version(a).to_ascii_lowercase())
        .filter(|k| !k.is_empty())
}

pub(crate) fn pmid_key(hit: &PaperHit) -> Option<String> {
    hit.pmid
        .as_deref()
        .map(|p| p.trim().to_string())
        .filter(|k| !k.is_empty())
}

pub(crate) fn title_author_key(hit: &PaperHit) -> Option<String> {
    let key = title_authors_key(hit);
    if key.is_empty() { None } else { Some(key) }
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
    let title_norm = normalize_text_key(&hit.title);
    let first_author_norm = hit
        .authors
        .first()
        .map(|a| normalize_text_key(a))
        .unwrap_or_default();

    if title_norm.is_empty() || first_author_norm.is_empty() {
        String::new()
    } else {
        format!("{title_norm}||{first_author_norm}")
    }
}

fn normalize_doi_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_lowercase();
    let normalized = lowered
        .strip_prefix("https://doi.org/")
        .or_else(|| lowered.strip_prefix("http://doi.org/"))
        .or_else(|| lowered.strip_prefix("doi:"))
        .unwrap_or(lowered.as_str())
        .trim();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn normalize_text_key(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use reqwest::Client;

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
        let resp = send_with_retry("test", client.get(server.uri()))
            .await
            .unwrap();
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
        let resp = send_with_retry("test", client.get(server.uri()))
            .await
            .unwrap();
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
        let resp = send_with_retry("test", client.get(server.uri()))
            .await
            .unwrap();
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
        let resp = send_with_retry("test", client.get(server.uri()))
            .await
            .unwrap();
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
        let resp = send_with_retry("test", client.get(server.uri()))
            .await
            .unwrap();
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
            hit_id: None,
            truncation: None,
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
            cache: None,
            relevance_score: None,
            ids: None,
            match_info: None,
            access: None,
            next: Vec::new(),
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
    fn dedupe_by_doi_normalizes_prefix_and_case() {
        let hits = vec![
            mk(
                PaperSource::SemanticScholar,
                "Paper A",
                Some("https://doi.org/10.1/ABC"),
                None,
                None,
            ),
            mk(
                PaperSource::Crossref,
                "Paper A",
                Some("doi:10.1/abc"),
                None,
                None,
            ),
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
    fn dedupe_by_title_author_handles_unicode_case() {
        let hits = vec![
            mk(
                PaperSource::SemanticScholar,
                "RÉSUMÉ Systems",
                None,
                None,
                Some("JOSÉ"),
            ),
            mk(
                PaperSource::Arxiv,
                "résumé systems",
                None,
                None,
                Some("josé"),
            ),
        ];
        let out = dedupe(hits);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn audit_dedupe_preserves_complementary_identity_and_access() {
        let first = mk(
            PaperSource::Crossref,
            "Paper",
            Some("10.1234/p"),
            None,
            None,
        );
        let mut second = mk(
            PaperSource::Arxiv,
            "Paper",
            Some("10.1234/p"),
            Some("2401.00001"),
            Some("Author"),
        );
        second.pdf_url = Some("https://example.test/p.pdf".into());
        second.oa_pdf_url = second.pdf_url.clone();
        second.pmid = Some("12345678".into());
        let out = dedupe(vec![first, second]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].arxiv_id.as_deref(), Some("2401.00001"));
        assert_eq!(out[0].pmid.as_deref(), Some("12345678"));
        assert_eq!(out[0].authors, vec!["Author"]);
        assert_eq!(
            out[0].pdf_url.as_deref(),
            Some("https://example.test/p.pdf")
        );
        assert_eq!(out[0].oa_pdf_url, out[0].pdf_url);
    }

    #[test]
    fn audit_dedupe_rejects_conflicting_ids_even_with_title_author_match() {
        let first = mk(
            PaperSource::Crossref,
            "Paper",
            Some("10.1234/a"),
            None,
            Some("Author"),
        );
        let second = mk(
            PaperSource::Arxiv,
            "Paper",
            Some("10.1234/b"),
            None,
            Some("Author"),
        );
        assert_eq!(dedupe(vec![first, second]).len(), 2);
        let first = mk(
            PaperSource::Crossref,
            "Paper",
            Some("10.1234/a"),
            Some("2401.00001"),
            Some("Author"),
        );
        let second = mk(
            PaperSource::Arxiv,
            "Paper",
            Some("10.1234/a"),
            Some("2401.00002"),
            Some("Author"),
        );
        assert_eq!(dedupe(vec![first, second]).len(), 2);
    }

    #[test]
    fn audit_dedupe_promoted_ids_join_previously_disjoint_records() {
        let doi = mk(
            PaperSource::Crossref,
            "DOI title",
            Some("10.1234/a"),
            None,
            None,
        );
        let arxiv = mk(
            PaperSource::Arxiv,
            "Preprint title",
            None,
            Some("2401.00001"),
            Some("Author"),
        );
        let bridge = mk(
            PaperSource::SemanticScholar,
            "Bridge",
            Some("10.1234/a"),
            Some("2401.00001v2"),
            None,
        );
        let out = dedupe(vec![doi, arxiv, bridge]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].source, PaperSource::Crossref);
        assert_eq!(out[0].authors, vec!["Author"]);
    }

    #[test]
    fn audit_dedupe_empty_metadata_does_not_hide_complementary_ids() {
        let first = mk(
            PaperSource::Crossref,
            "Paper",
            Some(" "),
            None,
            Some("Author"),
        );
        let second = mk(
            PaperSource::Arxiv,
            "Paper",
            Some("10.1234/a"),
            None,
            Some("Author"),
        );
        let out = dedupe(vec![first, second]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].doi.as_deref(), Some("10.1234/a"));
    }

    #[test]
    fn audit_dedupe_requires_author_corroboration_for_title_fallback() {
        let first = mk(PaperSource::Crossref, "Paper", None, None, None);
        let second = mk(PaperSource::Arxiv, "Paper", None, None, None);
        assert_eq!(dedupe(vec![first, second]).len(), 2);
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
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();
        assert!(hits.hits.is_empty());
    }

    #[test]
    fn search_options_enabled_respects_scope() {
        let mut opts = SearchOptions::new("q");
        assert!(opts.enabled(PaperSource::Arxiv));
        opts.sources = Some(vec![PaperSource::Crossref]);
        assert!(opts.enabled(PaperSource::Crossref));
        assert!(!opts.enabled(PaperSource::Arxiv));
    }

    #[test]
    fn source_fetch_limit_is_fixed_across_pages() {
        let mut opts = SearchOptions::new("q");
        assert_eq!(opts.source_fetch_limit(), 10);
        opts.limit_per_source = 2;
        for offset in [0, 1, 10, u32::MAX] {
            opts.offset = offset;
            opts.limit = 50;
            assert_eq!(opts.source_fetch_limit(), 2);
            assert!(opts.validate_source_fetch_limit().is_ok());
        }
        opts.limit_per_source = 0;
        assert_eq!(opts.source_fetch_limit(), 10);
    }

    #[test]
    fn source_fetch_limit_rejects_unsafe_windows() {
        let mut opts = SearchOptions::new("q");
        opts.limit_per_source = MAX_SOURCE_FETCH_LIMIT + 1;
        assert!(opts.validate_source_fetch_limit().is_err());
    }

    #[tokio::test]
    async fn diagnostic_conversion_bounds_provider_error_not_just_timeouts() {
        let message = format!(
            "https://example.org?api_key=provider-secret {}",
            "é".repeat(5000)
        );
        let (_, result) = run_source(
            PaperSource::Arxiv,
            true,
            Duration::from_secs(1),
            async {
                Err::<Vec<PaperHit>, _>(ZoteroMcpError::Api {
                    status: 500,
                    message,
                })
            },
            false,
        )
        .await;
        let SourceRunResult::Failed { reason } = result else {
            panic!("expected source failure")
        };
        assert!(reason.len() <= crate::error::MAX_ERROR_BYTES);
        assert!(!reason.contains("provider-secret"));
    }

    #[tokio::test]
    async fn audit_search_diagnostics_bound_and_redact_upstream_errors() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string(format!(
                "https://example.test/?api_key=secret-token {}",
                "é".repeat(5000)
            )))
            .mount(&server)
            .await;
        let base = server.uri();
        let search = PaperSearch::with_clients(
            ArxivClient::new(Some(&base)),
            HuggingFaceClient::new(Some(&base), None),
            SemanticScholarClient::new(Some(&base), None),
            CrossrefClient::new(Some(&base)),
        );
        let mut opts = SearchOptions::new("test");
        opts.sources = Some(vec![PaperSource::SemanticScholar]);
        let result = search.search(opts).await.unwrap();
        let reason = &result.diagnostics.sources_failed[0].reason;
        assert!(reason.len() <= crate::error::MAX_ERROR_BYTES);
        assert!(!reason.contains("secret-token"));
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
            offset: 0,
            limit: 0,
            cache_mode: SearchCacheMode::Auto,
            detail: crate::models::SearchDetail::Compact,
            abstract_max_chars: None,
        };
        let hits = paper_search.search(opts).await.unwrap().hits;

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
            offset: 0,
            limit: 0,
            cache_mode: SearchCacheMode::Auto,
            detail: crate::models::SearchDetail::Compact,
            abstract_max_chars: None,
        };
        let hits = paper_search.search(opts).await.unwrap().hits;

        // S2 was rate-limited; Crossref + HF should still contribute.
        assert_eq!(hits.len(), 2, "got {:?}", hits);
        let sources: Vec<_> = hits.iter().map(|h| h.source).collect();
        assert!(sources.contains(&PaperSource::Crossref));
        assert!(sources.contains(&PaperSource::HuggingFace));
        assert!(!sources.contains(&PaperSource::SemanticScholar));
    }
}
