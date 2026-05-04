use crate::error::Result;
use crate::models::License;
use crate::policy::parse_license;
use serde::{Deserialize, Serialize};
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
        Self {
            http: reqwest::Client::new(),
            email,
        }
    }

    pub async fn search(&self, q: &str, source: Option<&str>) -> Result<Vec<SearchResult>> {
        match source.unwrap_or("openalex") {
            "openalex" => self.search_openalex(q).await,
            "arxiv" => self.search_arxiv(q).await,
            _ => self.search_openalex(q).await,
        }
    }

    pub async fn resolve_doi(&self, doi: &str, source: Option<&str>) -> Result<ResolvedOpenPaper> {
        match source.unwrap_or("unpaywall") {
            "unpaywall" => self.resolve_unpaywall(doi).await,
            _ => self.resolve_unpaywall(doi).await,
        }
    }

    async fn search_openalex(&self, q: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://api.openalex.org/works?search={}&per-page=10",
            encode(q)
        );
        let body: OpenAlexResponse = self.http.get(url).send().await?.json().await?;
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
        let url = format!(
            "https://export.arxiv.org/api/query?search_query=all:{}&start=0&max_results=10",
            encode(q)
        );
        let text = self.http.get(url).send().await?.text().await?;
        Ok(parse_arxiv_atom(&text))
    }

    async fn resolve_unpaywall(&self, doi: &str) -> Result<ResolvedOpenPaper> {
        let email = self
            .email
            .clone()
            .unwrap_or_else(|| "paperseed@example.invalid".to_string());
        let url = format!(
            "https://api.unpaywall.org/v2/{}?email={}",
            encode(doi),
            encode(&email)
        );
        let body: UnpaywallResponse = self.http.get(url).send().await?.json().await?;
        let best = body.best_oa_location;
        Ok(ResolvedOpenPaper {
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
        })
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
