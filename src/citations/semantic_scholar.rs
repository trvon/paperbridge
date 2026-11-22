//! Semantic Scholar calls for the citation graph: paper lookup and
//! offset-paged references/citations with contexts, intents and
//! `isInfluential`.

use super::{Fetched, UpstreamError};
use crate::external::send_with_retry;
use citegraph::{Direction, ExtId, PageRow, Scheme, WorkMeta, WorkRecord};
use reqwest::Client;
use serde::Deserialize;

pub(crate) const SOURCE: &str = "semantic_scholar";
/// The graph API rejects `offset + limit` above this for citations and
/// references; results past it are unreachable.
pub(crate) const OFFSET_CAP: u32 = 10_000;
pub(crate) const MAX_LIMIT: u32 = 1_000;
const PAPER_FIELDS: &str = "paperId,externalIds,title,year,venue,authors";
const EDGE_FIELDS: &str =
    "contexts,intents,isInfluential,paperId,externalIds,title,year,venue,authors";

#[derive(Clone)]
pub(crate) struct SemanticScholarCitations {
    client: Client,
    base_url: String,
    api_key: String,
}

impl std::fmt::Debug for SemanticScholarCitations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticScholarCitations")
            .field("base_url", &self.base_url)
            .finish()
    }
}

pub(crate) struct EdgePage {
    pub rows: Vec<PageRow>,
    /// Raw JSON of entries whose paper has no `paperId`.
    pub unresolved: Vec<String>,
    pub next: Option<u32>,
}

impl SemanticScholarCitations {
    pub(crate) fn new(client: Client, base_url: &str, api_key: String) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Fetched<T> {
        let request = self.client.get(url).header("x-api-key", &self.api_key);
        let response = send_with_retry("citations_semantic_scholar", request)
            .await
            .map_err(|e| UpstreamError::Failed(e.to_string()))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(UpstreamError::NotFound);
        }
        if !status.is_success() {
            return Err(UpstreamError::Status(status.as_u16()));
        }
        response
            .json()
            .await
            .map_err(|e| UpstreamError::Failed(format!("invalid Semantic Scholar JSON: {e}")))
    }

    pub(crate) async fn paper(&self, id: &ExtId) -> Fetched<WorkRecord> {
        let key = lookup_key(id).ok_or(UpstreamError::NotFound)?;
        let url = format!(
            "{}/paper/{}?fields={PAPER_FIELDS}",
            self.base_url,
            super::path_segment(&key)
        );
        let raw: RawPaper = self.get(&url).await?;
        raw.into_record().ok_or(UpstreamError::NotFound)
    }

    /// One page of references (`Refs`) or citations (`CitedBy`).
    pub(crate) async fn edges(
        &self,
        paper: &ExtId,
        direction: Direction,
        offset: u32,
        limit: u32,
    ) -> Fetched<EdgePage> {
        let path = match direction {
            Direction::Refs => "references",
            Direction::CitedBy => "citations",
            Direction::Related => return Err(UpstreamError::NotFound),
        };
        let url = format!(
            "{}/paper/{}/{path}?fields={EDGE_FIELDS}&offset={offset}&limit={limit}",
            self.base_url, paper.value
        );
        let raw: RawEdgeList = self.get(&url).await?;
        let mut rows = Vec::new();
        let mut unresolved = Vec::new();
        for entry in raw.data {
            let other = match direction {
                Direction::Refs => entry.cited_paper,
                _ => entry.citing_paper,
            };
            let raw_other = other.clone();
            match other.and_then(RawPaper::into_record) {
                Some(record) => rows.push(PageRow {
                    work: record,
                    intents: entry.intents.unwrap_or_default(),
                    is_influential: entry.is_influential,
                    contexts: entry.contexts.unwrap_or_default(),
                }),
                None => unresolved.push(
                    raw_other
                        .and_then(|p| serde_json::to_string(&p.as_json()).ok())
                        .unwrap_or_else(|| "null".to_string()),
                ),
            }
        }
        Ok(EdgePage {
            rows,
            unresolved,
            next: raw.next,
        })
    }
}

fn lookup_key(id: &ExtId) -> Option<String> {
    Some(match id.scheme {
        Scheme::SemanticScholar => id.value.clone(),
        Scheme::Doi => format!("DOI:{}", id.value),
        Scheme::Arxiv => format!("ARXIV:{}", id.value),
        Scheme::Pmid => format!("PMID:{}", id.value),
        Scheme::Openalex => return None,
    })
}

#[derive(Debug, Deserialize)]
struct RawEdgeList {
    next: Option<u32>,
    #[serde(default)]
    data: Vec<RawEdge>,
}

#[derive(Debug, Deserialize)]
struct RawEdge {
    contexts: Option<Vec<String>>,
    intents: Option<Vec<String>>,
    #[serde(rename = "isInfluential")]
    is_influential: Option<bool>,
    #[serde(rename = "citedPaper")]
    cited_paper: Option<RawPaper>,
    #[serde(rename = "citingPaper")]
    citing_paper: Option<RawPaper>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawPaper {
    #[serde(rename = "paperId")]
    paper_id: Option<String>,
    #[serde(rename = "externalIds")]
    external_ids: Option<RawExternalIds>,
    title: Option<String>,
    year: Option<i32>,
    venue: Option<String>,
    #[serde(default)]
    authors: Vec<RawAuthor>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawExternalIds {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
    #[serde(rename = "PubMed")]
    pubmed: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawAuthor {
    name: Option<String>,
}

impl RawPaper {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "paperId": self.paper_id,
            "title": self.title,
            "year": self.year,
            "doi": self.external_ids.as_ref().and_then(|e| e.doi.clone()),
        })
    }

    fn into_record(self) -> Option<WorkRecord> {
        let primary = ExtId::semantic_scholar(self.paper_id.as_deref()?)?;
        let ext = self.external_ids;
        let doi = ext
            .as_ref()
            .and_then(|e| e.doi.as_deref())
            .and_then(ExtId::doi);
        let arxiv = ext
            .as_ref()
            .and_then(|e| e.arxiv.as_deref())
            .and_then(ExtId::arxiv);
        let pmid = ext
            .as_ref()
            .and_then(|e| e.pubmed.as_deref())
            .and_then(ExtId::pmid);
        let meta = WorkMeta {
            title: self.title.filter(|t| !t.is_empty()),
            year: self.year,
            venue: self.venue.filter(|v| !v.is_empty()),
            authors: self
                .authors
                .into_iter()
                .filter_map(|a| a.name)
                .filter(|n| !n.is_empty())
                .collect(),
        };
        Some(
            WorkRecord::new(primary)
                .with_id(doi)
                .with_id(arxiv)
                .with_id(pmid)
                .with_meta(meta),
        )
    }
}
