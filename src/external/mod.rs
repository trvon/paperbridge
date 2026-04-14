pub mod arxiv;
pub mod huggingface;
pub mod semantic_scholar;

pub use arxiv::ArxivClient;
pub use huggingface::HuggingFaceClient;
pub use semantic_scholar::SemanticScholarClient;

use crate::crossref::CrossrefClient;
use crate::error::Result;
use crate::models::{PaperHit, PaperSource};
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
}

impl PaperSearch {
    pub fn new() -> Self {
        Self::with_keys(None, None)
    }

    pub fn with_keys(hf_token: Option<String>, s2_api_key: Option<String>) -> Self {
        Self {
            arxiv: ArxivClient::new(None),
            hf: hf_token.map(|t| HuggingFaceClient::new(None, Some(t))),
            s2: s2_api_key.map(|k| SemanticScholarClient::new(None, Some(k))),
            crossref: CrossrefClient::new(None),
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
        }
    }

    pub async fn search(&self, opts: SearchOptions) -> Result<Vec<PaperHit>> {
        let timeout_duration = Duration::from_millis(opts.timeout_ms);
        let limit = opts.limit_per_source;
        let query = opts.query.clone();

        let s2_fut = run_optional_source(
            PaperSource::SemanticScholar,
            opts.enabled(PaperSource::SemanticScholar),
            timeout_duration,
            self.s2.as_ref().map(|c| c.search(&query, limit)),
            "no semantic_scholar_api_key/SEMANTIC_SCHOLAR_API_KEY configured",
        );
        let crossref_fut = run_source(
            PaperSource::Crossref,
            opts.enabled(PaperSource::Crossref),
            timeout_duration,
            self.crossref.search(&query, limit),
        );
        let hf_fut = run_optional_source(
            PaperSource::HuggingFace,
            opts.enabled(PaperSource::HuggingFace),
            timeout_duration,
            self.hf.as_ref().map(|c| c.search(&query, limit)),
            "no hf_token/HF_TOKEN configured",
        );
        let arxiv_fut = run_source(
            PaperSource::Arxiv,
            opts.enabled(PaperSource::Arxiv),
            timeout_duration,
            self.arxiv.search(&query, limit),
        );

        let (s2_hits, crossref_hits, hf_hits, arxiv_hits) =
            tokio::join!(s2_fut, crossref_fut, hf_fut, arxiv_fut);

        let mut merged: Vec<PaperHit> = Vec::new();
        merged.extend(s2_hits);
        merged.extend(crossref_hits);
        merged.extend(hf_hits);
        merged.extend(arxiv_hits);

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
        Ok(Err(e)) => {
            tracing::warn!(?source, error = %e, "source search failed");
            Vec::new()
        }
        Err(_) => {
            tracing::warn!(?source, "source search timed out");
            Vec::new()
        }
    }
}

async fn run_optional_source<F>(
    source: PaperSource,
    enabled: bool,
    dur: Duration,
    fut: Option<F>,
    missing_key_reason: &'static str,
) -> Vec<PaperHit>
where
    F: std::future::Future<Output = Result<Vec<PaperHit>>>,
{
    if !enabled {
        return Vec::new();
    }
    let Some(fut) = fut else {
        tracing::debug!(?source, reason = missing_key_reason, "source skipped");
        return Vec::new();
    };
    run_source(source, true, dur, fut).await
}

fn dedupe(hits: Vec<PaperHit>) -> Vec<PaperHit> {
    let mut seen_doi: HashSet<String> = HashSet::new();
    let mut seen_arxiv: HashSet<String> = HashSet::new();
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
            abstract_note: None,
            url: None,
            pdf_url: None,
            venue: None,
            citation_count: None,
        }
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
            sources: None,
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
}
