use crate::error::Result;
use crate::models::License;
use crate::policy::parse_license;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use urlencoding::encode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub source: String,
    pub title: String,
    pub doi: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub open_url: Option<String>,
    pub license: License,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedOpenPaper {
    pub doi: String,
    pub title: Option<String>,
    pub open_pdf_url: Option<String>,
    pub landing_url: Option<String>,
    pub license: License,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct ResolverClient {
    http: reqwest::Client,
    email: Option<String>,
}

impl ResolverClient {
    pub fn new(email: Option<String>) -> Self {
        // Bound each resolve/search call so a slow upstream can't leak a
        // lingering background thread during OA auto-mirroring.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { http, email }
    }

    pub async fn search(&self, q: &str, source: Option<&str>) -> Result<Vec<SearchResult>> {
        match source.unwrap_or("openalex") {
            "openalex" => self.search_openalex(q).await,
            "arxiv" => self.search_arxiv(q).await,
            "crossref" => self.search_crossref(q).await,
            _ => self.search_openalex(q).await,
        }
    }

    pub async fn resolve_doi(&self, doi: &str, source: Option<&str>) -> Result<ResolvedOpenPaper> {
        match source {
            Some("unpaywall") => self.resolve_unpaywall(doi).await,
            Some("openalex") => self.resolve_openalex(doi).await,
            Some("crossref") => self.resolve_crossref(doi).await,
            // Default: try Unpaywall, then fall back to OpenAlex's OA location
            // when Unpaywall has no open PDF (or is unreachable). This is what
            // lets DOIs from metadata-only sources (Crossref, PubMed, DBLP)
            // resolve to a mirrorable open PDF.
            _ => self.resolve_best(doi).await,
        }
    }

    async fn resolve_best(&self, doi: &str) -> Result<ResolvedOpenPaper> {
        match self.resolve_unpaywall(doi).await {
            Ok(paper) if paper.open_pdf_url.is_some() => Ok(paper),
            Ok(paper) => match self.resolve_openalex(doi).await {
                Ok(openalex) if openalex.open_pdf_url.is_some() => Ok(openalex),
                // OpenAlex added nothing usable — keep the Unpaywall metadata.
                _ => Ok(paper),
            },
            Err(_) => self.resolve_openalex(doi).await,
        }
    }

    async fn resolve_openalex(&self, doi: &str) -> Result<ResolvedOpenPaper> {
        let mut url = format!("https://api.openalex.org/works/https://doi.org/{}", doi);
        if let Some(email) = self.email.as_deref() {
            url.push_str(&format!("?mailto={}", encode(email)));
        }
        let work: OpenAlexWork = self.get_with_retry(&url).await?.json().await?;
        Ok(openalex_work_to_resolved(work, doi))
    }

    async fn search_openalex(&self, q: &str) -> Result<Vec<SearchResult>> {
        let url = openalex_search_url(q, self.email.as_deref());
        let body: OpenAlexResponse = self.get_with_retry(&url).await?.json().await?;
        Ok(body
            .results
            .into_iter()
            .map(|work| SearchResult {
                source: "openalex".to_string(),
                title: work.title.unwrap_or_else(|| "untitled".to_string()),
                doi: work
                    .doi
                    .map(|doi| doi.trim_start_matches("https://doi.org/").to_string()),
                authors: work
                    .authorships
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|authorship| authorship.author.display_name)
                    .collect(),
                year: work.publication_year,
                open_url: work.open_access.and_then(|oa| oa.oa_url).or(work
                    .primary_location
                    .and_then(|location| location.landing_page_url)),
                license: work
                    .best_oa_location
                    .and_then(|location| location.license)
                    .map(|license| parse_license(&license))
                    .unwrap_or(License::Unknown),
            })
            .collect())
    }

    async fn search_arxiv(&self, q: &str) -> Result<Vec<SearchResult>> {
        let url = arxiv_search_url(q);
        let text = self.get_with_retry(&url).await?.text().await?;
        Ok(parse_arxiv_atom(&text))
    }

    async fn resolve_unpaywall(&self, doi: &str) -> Result<ResolvedOpenPaper> {
        let email = self
            .email
            .as_deref()
            .filter(|email| !email.trim().is_empty())
            .ok_or(crate::error::PaperseedError::MissingResolverEmail)?;
        let url = format!(
            "https://api.unpaywall.org/v2/{}?email={}",
            encode(doi),
            encode(email)
        );
        let body: UnpaywallResponse = self.get_with_retry(&url).await?.json().await?;
        Ok(unpaywall_to_resolved(body, doi))
    }

    async fn search_crossref(&self, q: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://api.crossref.org/works?query.bibliographic={}&rows=10",
            encode(q)
        );
        let body: CrossrefResponse = self.get_with_retry(&url).await?.json().await?;
        Ok(body
            .message
            .items
            .into_iter()
            .map(crossref_to_search_result)
            .collect())
    }

    async fn resolve_crossref(&self, doi: &str) -> Result<ResolvedOpenPaper> {
        let url = format!("https://api.crossref.org/works/{}", encode(doi));
        let body: CrossrefSingleResponse = self.get_with_retry(&url).await?.json().await?;
        Ok(ResolvedOpenPaper {
            doi: body.message.doi.unwrap_or_else(|| doi.to_string()),
            title: body.message.title.into_iter().next(),
            open_pdf_url: None,
            landing_url: body.message.url,
            license: License::Unknown,
            source: "crossref".to_string(),
        })
    }

    async fn get_with_retry(&self, url: &str) -> Result<reqwest::Response> {
        const ATTEMPTS: usize = 3;
        for attempt in 0..ATTEMPTS {
            match self.http.get(url).send().await {
                Ok(response) if !retryable_status(response.status()) => {
                    return Ok(response.error_for_status()?);
                }
                Ok(response) if attempt + 1 == ATTEMPTS => {
                    return Ok(response.error_for_status()?);
                }
                Ok(response) => {
                    let delay = retry_delay(response.headers(), attempt);
                    tokio::time::sleep(delay).await;
                }
                Err(error) if attempt + 1 == ATTEMPTS => return Err(error.into()),
                Err(_) => tokio::time::sleep(exponential_delay(attempt)).await,
            }
        }
        unreachable!("retry loop always returns on its final attempt")
    }
}

fn openalex_search_url(query: &str, email: Option<&str>) -> String {
    let mut url = format!(
        "https://api.openalex.org/works?search={}&per-page=10",
        encode(query)
    );
    if let Some(email) = email.filter(|email| !email.trim().is_empty()) {
        url.push_str(&format!("&mailto={}", encode(email)));
    }
    url
}

fn arxiv_search_url(query: &str) -> String {
    let query = query.trim();
    let fielded = if let Some(author) = query.strip_prefix("author:") {
        format!("au:\"{}\"", author.trim())
    } else if looks_like_arxiv_id(query) {
        format!("id:{}", query.trim_start_matches("arxiv:"))
    } else {
        format!("ti:\"{query}\"")
    };
    format!(
        "https://export.arxiv.org/api/query?search_query={}&start=0&max_results=10&sortBy=relevance&sortOrder=descending",
        encode(&fielded)
    )
}

fn looks_like_arxiv_id(query: &str) -> bool {
    let query = query.trim_start_matches("arxiv:");
    let query = query.strip_suffix(".pdf").unwrap_or(query);
    query.contains('.')
        && query
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '/' | '-'))
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_delay(headers: &reqwest::header::HeaderMap, attempt: usize) -> Duration {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| exponential_delay(attempt))
}

fn exponential_delay(attempt: usize) -> Duration {
    Duration::from_millis(200_u64.saturating_mul(1_u64 << attempt.min(4)))
}

fn unpaywall_to_resolved(body: UnpaywallResponse, doi: &str) -> ResolvedOpenPaper {
    let best = body.best_oa_location;
    ResolvedOpenPaper {
        doi: body.doi.unwrap_or_else(|| doi.to_string()),
        title: body.title,
        open_pdf_url: best
            .as_ref()
            .and_then(|location| location.url_for_pdf.clone()),
        landing_url: best.as_ref().and_then(|location| location.url.clone()),
        license: best
            .and_then(|location| location.license)
            .map(|license| parse_license(&license))
            .unwrap_or(License::Unknown),
        source: "unpaywall".to_string(),
    }
}

fn openalex_work_to_resolved(work: OpenAlexWork, doi: &str) -> ResolvedOpenPaper {
    let best = work.best_oa_location;
    let oa_url = work.open_access.and_then(|oa| oa.oa_url);
    let open_pdf_url = best.as_ref().and_then(|location| location.pdf_url.clone());
    let landing_url = best
        .as_ref()
        .and_then(|location| location.landing_page_url.clone())
        .or(oa_url);
    ResolvedOpenPaper {
        doi: work
            .doi
            .map(|doi| doi.trim_start_matches("https://doi.org/").to_string())
            .unwrap_or_else(|| doi.to_string()),
        title: work.title,
        open_pdf_url,
        landing_url,
        license: best
            .and_then(|location| location.license)
            .map(|license| parse_license(&license))
            .unwrap_or(License::Unknown),
        source: "openalex".to_string(),
    }
}

pub fn parse_arxiv_atom(atom: &str) -> Vec<SearchResult> {
    atom.split("<entry>")
        .skip(1)
        .filter_map(|entry| {
            let title = tag(entry, "title")?
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let open_url = tag(entry, "id");
            let authors = entry
                .split("<author>")
                .skip(1)
                .filter_map(|author| tag(author, "name"))
                .collect();
            let year = tag(entry, "published")
                .and_then(|published| published.get(0..4).map(str::to_string))
                .and_then(|year| year.parse::<u16>().ok());
            Some(SearchResult {
                source: "arxiv".to_string(),
                title,
                doi: tag(entry, "arxiv:doi"),
                authors,
                year,
                open_url,
                license: License::Unknown,
            })
        })
        .collect()
}

fn tag(input: &str, name: &str) -> Option<String> {
    let start = format!("<{name}>");
    let end = format!("</{name}>");
    let after_start = input.split_once(&start)?.1;
    Some(after_start.split_once(&end)?.0.trim().to_string())
}

fn crossref_to_search_result(work: CrossrefWork) -> SearchResult {
    SearchResult {
        source: "crossref".to_string(),
        title: work
            .title
            .into_iter()
            .next()
            .unwrap_or_else(|| "untitled".to_string()),
        doi: work.doi,
        authors: work
            .author
            .into_iter()
            .filter_map(|author| {
                let name = format!(
                    "{} {}",
                    author.given.unwrap_or_default(),
                    author.family.unwrap_or_default()
                )
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
                (!name.is_empty()).then_some(name)
            })
            .collect(),
        year: work
            .published
            .and_then(|date| date.date_parts.into_iter().next())
            .and_then(|parts| parts.into_iter().next()),
        open_url: work.url,
        license: License::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openalex_work_maps_pdf_and_strips_doi_prefix() {
        let work: OpenAlexWork = serde_json::from_str(
            r#"{
                "title": "A Paper",
                "doi": "https://doi.org/10.1/abc",
                "publication_year": 2020,
                "open_access": {"oa_url": "https://oa.example/landing"},
                "best_oa_location": {
                    "landing_page_url": "https://pub.example/land",
                    "pdf_url": "https://pub.example/paper.pdf",
                    "license": "cc-by"
                }
            }"#,
        )
        .unwrap();
        let resolved = openalex_work_to_resolved(work, "10.1/abc");
        assert_eq!(resolved.source, "openalex");
        assert_eq!(resolved.doi, "10.1/abc");
        assert_eq!(
            resolved.open_pdf_url.as_deref(),
            Some("https://pub.example/paper.pdf")
        );
        assert_eq!(resolved.license, License::CcBy);
    }

    #[test]
    fn openalex_work_without_oa_location_has_no_pdf() {
        let work: OpenAlexWork = serde_json::from_str(
            r#"{"title": "Closed", "doi": "10.1/x", "best_oa_location": null}"#,
        )
        .unwrap();
        let resolved = openalex_work_to_resolved(work, "10.1/x");
        assert!(resolved.open_pdf_url.is_none());
        assert_eq!(resolved.license, License::Unknown);
    }

    #[test]
    fn unpaywall_maps_best_oa_location() {
        let body: UnpaywallResponse = serde_json::from_str(
            r#"{
                "doi": "10.1/abc",
                "title": "A Paper",
                "best_oa_location": {
                    "url": "https://land.example",
                    "url_for_pdf": "https://land.example/p.pdf",
                    "license": "cc0"
                }
            }"#,
        )
        .unwrap();
        let resolved = unpaywall_to_resolved(body, "10.1/abc");
        assert_eq!(resolved.source, "unpaywall");
        assert_eq!(
            resolved.open_pdf_url.as_deref(),
            Some("https://land.example/p.pdf")
        );
        assert_eq!(resolved.license, License::Cc0);
    }

    #[test]
    fn openalex_search_includes_configured_mailto() {
        let url = openalex_search_url("graph learning", Some("researcher@example.org"));
        assert!(url.contains("mailto=researcher%40example.org"));
    }

    #[test]
    fn arxiv_search_uses_field_prefix_and_relevance_sort() {
        let title_url = arxiv_search_url("Attention Is All You Need");
        assert!(title_url.contains("search_query=ti%3A%22Attention%20Is%20All%20You%20Need%22"));
        assert!(title_url.contains("sortBy=relevance"));

        let id_url = arxiv_search_url("arxiv:1706.03762");
        assert!(id_url.contains("search_query=id%3A1706.03762"));
    }

    #[tokio::test]
    async fn explicit_unpaywall_resolution_requires_real_email() {
        let resolver = ResolverClient::new(None);
        let error = resolver
            .resolve_doi("10.1/example", Some("unpaywall"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("contact email"));
    }

    #[test]
    fn crossref_metadata_maps_to_search_result() {
        let work: CrossrefWork = serde_json::from_str(
            r#"{
                "title": ["A Crossref Paper"],
                "DOI": "10.1/crossref",
                "author": [{"given": "Ada", "family": "Lovelace"}],
                "published": {"date-parts": [[2025, 1, 2]]},
                "URL": "https://doi.org/10.1/crossref"
            }"#,
        )
        .unwrap();
        let result = crossref_to_search_result(work);
        assert_eq!(result.title, "A Crossref Paper");
        assert_eq!(result.authors, vec!["Ada Lovelace"]);
        assert_eq!(result.year, Some(2025));
    }
}

#[derive(Debug, Deserialize)]
struct OpenAlexResponse {
    results: Vec<OpenAlexWork>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexWork {
    title: Option<String>,
    doi: Option<String>,
    publication_year: Option<u16>,
    authorships: Option<Vec<OpenAlexAuthorship>>,
    open_access: Option<OpenAlexAccess>,
    primary_location: Option<OpenAlexLocation>,
    best_oa_location: Option<OpenAlexLocation>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexAuthorship {
    author: OpenAlexAuthor,
}

#[derive(Debug, Deserialize)]
struct OpenAlexAuthor {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexAccess {
    oa_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexLocation {
    landing_page_url: Option<String>,
    pdf_url: Option<String>,
    license: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UnpaywallResponse {
    doi: Option<String>,
    title: Option<String>,
    best_oa_location: Option<UnpaywallLocation>,
}

#[derive(Debug, Deserialize)]
struct UnpaywallLocation {
    url: Option<String>,
    url_for_pdf: Option<String>,
    license: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrossrefResponse {
    message: CrossrefMessage,
}

#[derive(Debug, Deserialize)]
struct CrossrefSingleResponse {
    message: CrossrefWork,
}

#[derive(Debug, Deserialize)]
struct CrossrefMessage {
    items: Vec<CrossrefWork>,
}

#[derive(Debug, Deserialize)]
struct CrossrefWork {
    #[serde(default)]
    title: Vec<String>,
    #[serde(default, rename = "DOI")]
    doi: Option<String>,
    #[serde(default)]
    author: Vec<CrossrefAuthor>,
    #[serde(default)]
    published: Option<CrossrefDate>,
    #[serde(default, rename = "URL")]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrossrefAuthor {
    given: Option<String>,
    family: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CrossrefDate {
    #[serde(default, rename = "date-parts")]
    date_parts: Vec<Vec<u16>>,
}
