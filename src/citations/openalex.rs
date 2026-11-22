//! OpenAlex calls for the citation graph: work lookup (references and
//! related works ride on the work object), `cites:` cursor paging, and
//! batched hydration by OpenAlex id. Kept apart from
//! `external::openalex` (search), which owns API-key handling.

use super::{Fetched, Transport, UpstreamError};
use citegraph::{ExtId, WorkMeta, WorkRecord};
use reqwest::Client;
use serde::Deserialize;

pub(crate) const SOURCE: &str = "openalex";
/// Ids per hydration request (OR-filter); OpenAlex accepts up to 100.
pub(crate) const HYDRATE_BATCH: usize = 50;
pub(crate) const MAX_PER_PAGE: u32 = 200;
const SELECT: &str = "id,doi,ids,display_name,publication_year,authorships,primary_location";
const SELECT_WITH_LISTS: &str = "id,doi,ids,display_name,publication_year,authorships,primary_location,referenced_works,related_works";

#[derive(Clone)]
pub(crate) struct OpenAlexCitations {
    client: Client,
    base_url: String,
    mailto: Option<String>,
    api_key: Option<String>,
}

impl std::fmt::Debug for OpenAlexCitations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAlexCitations")
            .field("base_url", &self.base_url)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

/// A work with its reference and related-work id lists.
#[derive(Debug, Clone)]
pub(crate) struct OpenAlexWork {
    pub record: WorkRecord,
    pub referenced: Vec<ExtId>,
    pub related: Vec<ExtId>,
}

pub(crate) struct CitesPage {
    pub records: Vec<WorkRecord>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
}

impl OpenAlexCitations {
    pub(crate) fn new(
        client: Client,
        base_url: &str,
        mailto: Option<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            mailto,
            api_key,
        }
    }

    fn url(&self, path_and_query: &str) -> String {
        let mut url = format!("{}{path_and_query}", self.base_url);
        let sep = if url.contains('?') { '&' } else { '?' };
        if let Some(email) = &self.mailto {
            url.push(sep);
            url.push_str(&format!("mailto={}", urlencoding::encode(email)));
        }
        if let Some(key) = &self.api_key {
            let sep = if url.contains('?') { '&' } else { '?' };
            url.push(sep);
            url.push_str(&format!("api_key={}", urlencoding::encode(key)));
        }
        url
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        t: &mut Transport<'_>,
        url: &str,
    ) -> Fetched<T> {
        let response = t
            .send(SOURCE, "citations_openalex", self.client.get(url))
            .await?;
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
            .map_err(|e| UpstreamError::Failed(format!("invalid OpenAlex JSON: {e}")))
    }

    /// Look a work up by OpenAlex id, DOI or PMID.
    pub(crate) async fn work(&self, t: &mut Transport<'_>, id: &ExtId) -> Fetched<OpenAlexWork> {
        let key = lookup_key(id).ok_or(UpstreamError::NotFound)?;
        let url = self.url(&format!(
            "/works/{}?select={SELECT_WITH_LISTS}",
            super::path_segment(&key)
        ));
        let raw: RawWork = self.get(t, &url).await?;
        let referenced = raw
            .referenced_works
            .iter()
            .filter_map(|w| ExtId::openalex(w))
            .collect();
        let related = raw
            .related_works
            .iter()
            .filter_map(|w| ExtId::openalex(w))
            .collect();
        let record = raw.into_record().ok_or(UpstreamError::NotFound)?;
        Ok(OpenAlexWork {
            record,
            referenced,
            related,
        })
    }

    /// One page of works citing `work` (`filter=cites:W…`, cursor paging).
    pub(crate) async fn cites_page(
        &self,
        t: &mut Transport<'_>,
        work: &ExtId,
        cursor: Option<&str>,
        per_page: u32,
    ) -> Fetched<CitesPage> {
        let url = self.url(&format!(
            "/works?filter=cites:{}&per_page={}&cursor={}&select={SELECT}",
            work.value,
            per_page.clamp(1, MAX_PER_PAGE),
            urlencoding::encode(cursor.unwrap_or("*")),
        ));
        let raw: RawList = self.get(t, &url).await?;
        Ok(CitesPage {
            records: raw
                .results
                .into_iter()
                .filter_map(RawWork::into_record)
                .collect(),
            next_cursor: raw.meta.as_ref().and_then(|m| m.next_cursor.clone()),
            total: raw.meta.and_then(|m| m.count),
        })
    }

    /// Metadata for up to [`HYDRATE_BATCH`] OpenAlex ids.
    pub(crate) async fn hydrate(
        &self,
        t: &mut Transport<'_>,
        ids: &[ExtId],
    ) -> Fetched<Vec<WorkRecord>> {
        let filter = ids
            .iter()
            .map(|i| i.value.as_str())
            .collect::<Vec<_>>()
            .join("|");
        let url = self.url(&format!(
            "/works?filter=openalex:{filter}&per_page={}&select={SELECT}",
            ids.len().max(1)
        ));
        let raw: RawList = self.get(t, &url).await?;
        Ok(raw
            .results
            .into_iter()
            .filter_map(RawWork::into_record)
            .collect())
    }
}

/// OpenAlex `/works/{key}` path segment for an id it can resolve.
fn lookup_key(id: &ExtId) -> Option<String> {
    use citegraph::Scheme;
    Some(match id.scheme {
        Scheme::Openalex => id.value.clone(),
        Scheme::Doi => format!("doi:{}", id.value),
        Scheme::Arxiv => format!("doi:10.48550/arxiv.{}", id.value),
        Scheme::Pmid => format!("pmid:{}", id.value),
        Scheme::SemanticScholar => return None,
    })
}

#[derive(Debug, Deserialize)]
struct RawList {
    meta: Option<RawMeta>,
    #[serde(default)]
    results: Vec<RawWork>,
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    count: Option<u64>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawWork {
    id: Option<String>,
    doi: Option<String>,
    ids: Option<RawIds>,
    display_name: Option<String>,
    title: Option<String>,
    publication_year: Option<i32>,
    #[serde(default)]
    authorships: Vec<RawAuthorship>,
    primary_location: Option<RawLocation>,
    #[serde(default)]
    referenced_works: Vec<String>,
    #[serde(default)]
    related_works: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawIds {
    doi: Option<String>,
    pmid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAuthorship {
    author: Option<RawAuthor>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawLocation {
    source: Option<RawSource>,
}

#[derive(Debug, Deserialize)]
struct RawSource {
    display_name: Option<String>,
}

impl RawWork {
    fn into_record(self) -> Option<WorkRecord> {
        let primary = ExtId::openalex(self.id.as_deref()?)?;
        let ids = self.ids;
        let doi = self
            .doi
            .as_deref()
            .or(ids.as_ref().and_then(|i| i.doi.as_deref()))
            .and_then(ExtId::doi);
        let pmid = ids
            .as_ref()
            .and_then(|i| i.pmid.as_deref())
            .and_then(ExtId::pmid);
        let meta = WorkMeta {
            title: self.display_name.or(self.title).filter(|t| !t.is_empty()),
            year: self.publication_year,
            venue: self
                .primary_location
                .and_then(|l| l.source)
                .and_then(|s| s.display_name),
            authors: self
                .authorships
                .into_iter()
                .filter_map(|a| a.author.and_then(|a| a.display_name))
                .filter(|n| !n.is_empty())
                .collect(),
        };
        Some(
            WorkRecord::new(primary)
                .with_id(doi)
                .with_id(pmid)
                .with_meta(meta),
        )
    }
}
