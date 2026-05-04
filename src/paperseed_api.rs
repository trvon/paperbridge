//! Paperseed integration boundary.
//!
//! Paperbridge owns research/search user flows and MCP/API exposure. Paperseed
//! owns local corpus storage, license-gated seed manifests, and future P2P
//! transport state. This module keeps that boundary explicit so Paperbridge can
//! call Paperseed as a library without growing a second corpus/seeding CLI.

use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    CachedPaperDetail, CachedPaperSummary, FulltextContent, PaperHit, PaperSource,
};
use paperseed::app::{CorpusPaths, ImportRequest, IngestRequest, SeedManifest};
use paperseed::db::{CorpusDb, IndexedPaper, QueryHit};
use paperseed::models::LocalPaper;
use paperseed::resolver::{ResolvedOpenPaper, ResolverClient, SearchResult};
use paperseed::sources::PaperbridgeMetadata;
use paperseed::yams::{CommandYamsRunner, YamsConfig, YamsDownloadRequest, YamsDownloadResult};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PaperseedApi {
    paths: CorpusPaths,
    resolver: ResolverClient,
    yams: YamsConfig,
}

impl PaperseedApi {
    pub fn new(corpus_root: impl Into<PathBuf>, email: Option<String>) -> Self {
        Self::with_yams(corpus_root, email, YamsConfig::auto_detect())
    }

    pub fn with_yams(
        corpus_root: impl Into<PathBuf>,
        email: Option<String>,
        yams: YamsConfig,
    ) -> Self {
        Self {
            paths: CorpusPaths::new(corpus_root),
            resolver: ResolverClient::new(email),
            yams,
        }
    }

    pub fn default(email: Option<String>) -> Self {
        Self::new(paperseed::app::default_corpus_root(), email)
    }

    pub fn default_with_yams(email: Option<String>, yams: YamsConfig) -> Self {
        Self::with_yams(paperseed::app::default_corpus_root(), email, yams)
    }

    pub fn paths(&self) -> &CorpusPaths {
        &self.paths
    }

    pub fn download_with_yams_queue(
        &self,
        url: &str,
        title: Option<&str>,
        doi: Option<&str>,
        source_url: Option<&str>,
    ) -> Option<YamsDownloadResult> {
        let runner = CommandYamsRunner::with_timeout(&self.yams.binary, Duration::from_secs(30));
        paperseed::yams::download_with_runner(
            &self.yams,
            &runner,
            YamsDownloadRequest {
                url,
                title,
                doi,
                source_url,
            },
        )
    }

    pub async fn search_open_sources(
        &self,
        query: &str,
        source: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        self.resolver.search(query, source).await.map_err(map_error)
    }

    pub async fn resolve_open_doi(
        &self,
        doi: &str,
        source: Option<&str>,
    ) -> Result<ResolvedOpenPaper> {
        self.resolver
            .resolve_doi(doi, source)
            .await
            .map_err(map_error)
    }

    pub fn corpus_status(&self) -> Result<CorpusDb> {
        paperseed::app::status(&self.paths).map_err(map_error)
    }

    pub fn import_local_file(
        &self,
        path: impl AsRef<Path>,
        title: Option<String>,
        license: Option<String>,
    ) -> Result<LocalPaper> {
        paperseed::app::import_with_yams(
            &self.paths,
            ImportRequest {
                path: path.as_ref().to_path_buf(),
                title,
                license,
                yams_hash: None,
            },
            &self.yams,
        )
        .map_err(map_error)
    }

    pub fn ingest_with_metadata(
        &self,
        path: impl AsRef<Path>,
        metadata: PaperbridgeMetadata,
        license: Option<String>,
    ) -> Result<LocalPaper> {
        paperseed::app::ingest_with_yams(
            &self.paths,
            IngestRequest {
                path: path.as_ref().to_path_buf(),
                metadata,
                license,
                yams_hash: None,
            },
            &self.yams,
        )
        .map_err(map_error)
    }

    pub fn query_corpus(&self, query: &str) -> Result<Vec<QueryHit>> {
        paperseed::app::query_with_yams(&self.paths, query, &self.yams).map_err(map_error)
    }

    pub fn query_corpus_entries(&self, query: &str) -> Result<Vec<IndexedPaper>> {
        paperseed::app::query_entries_with_yams(&self.paths, query, &self.yams).map_err(map_error)
    }

    pub fn search_cached_papers(&self, query: &str, limit: usize) -> Result<Vec<PaperHit>> {
        let entries = paperseed::app::query_entries_with_yams(&self.paths, query, &self.yams)
            .map_err(map_error)?;
        Ok(entries
            .into_iter()
            .take(limit)
            .map(|entry| PaperHit {
                source: PaperSource::Paperseed,
                title: entry.paper.metadata.title.clone(),
                authors: entry.paper.metadata.authors.clone(),
                year: entry.paper.metadata.year.map(|year| year.to_string()),
                doi: entry.paper.metadata.doi.clone(),
                arxiv_id: None,
                pmid: None,
                abstract_note: entry
                    .full_text
                    .as_ref()
                    .map(|text| summarize_abstract(text)),
                url: entry.paper.metadata.source_url.clone(),
                pdf_url: Some(entry.paper.file.path.display().to_string()),
                oa_pdf_url: entry.paper.metadata.source_url.clone(),
                venue: entry.paper.metadata.venue.clone(),
                citation_count: None,
                cache: Some(CachedPaperSummary {
                    paper_id: entry.paper.metadata.id.clone(),
                    cached: true,
                    has_full_text: entry.full_text.is_some(),
                }),
            })
            .collect())
    }

    pub fn find_cached_hit(&self, hit: &PaperHit) -> Option<IndexedPaper> {
        let db = self.corpus_status().ok()?;
        db.papers.into_iter().find(|entry| {
            doi_matches(entry, hit) || source_url_matches(entry, hit) || title_matches(entry, hit)
        })
    }

    pub fn get_cached_paper(&self, paper_id: &str) -> Result<CachedPaperDetail> {
        let entry = paperseed::app::get_entry(&self.paths, paper_id).map_err(map_error)?;
        Ok(cached_paper_detail(entry))
    }

    pub fn get_cached_paper_fulltext(&self, paper_id: &str) -> Result<FulltextContent> {
        let content =
            paperseed::app::get_full_text(&self.paths, paper_id, &self.yams).map_err(map_error)?;
        let indexed_chars = u32::try_from(content.chars().count()).ok();
        Ok(FulltextContent {
            item_key: paper_id.to_string(),
            content,
            indexed_pages: None,
            total_pages: None,
            indexed_chars,
            total_chars: indexed_chars,
        })
    }

    pub fn create_seed_manifest(&self, paper_id: &str) -> Result<SeedManifest> {
        paperseed::app::create_seed_manifest(&self.paths, paper_id).map_err(map_error)
    }
}

fn cached_paper_detail(entry: IndexedPaper) -> CachedPaperDetail {
    CachedPaperDetail {
        paper_id: entry.paper.metadata.id,
        title: entry.paper.metadata.title,
        authors: entry.paper.metadata.authors,
        year: entry.paper.metadata.year.map(|year| year.to_string()),
        doi: entry.paper.metadata.doi,
        venue: entry.paper.metadata.venue,
        source_url: entry.paper.metadata.source_url,
        stored_path: entry.paper.file.path.display().to_string(),
        mime: entry.paper.file.mime,
        yams_hash: entry.yams_hash,
        has_full_text: entry.full_text.is_some(),
    }
}

fn doi_matches(entry: &IndexedPaper, hit: &PaperHit) -> bool {
    matches!(
        (entry.paper.metadata.doi.as_deref(), hit.doi.as_deref()),
        (Some(left), Some(right)) if left.eq_ignore_ascii_case(right)
    )
}

fn source_url_matches(entry: &IndexedPaper, hit: &PaperHit) -> bool {
    let Some(source_url) = entry.paper.metadata.source_url.as_deref() else {
        return false;
    };
    hit.url.as_deref() == Some(source_url) || hit.oa_pdf_url.as_deref() == Some(source_url)
}

fn title_matches(entry: &IndexedPaper, hit: &PaperHit) -> bool {
    normalize_title(&entry.paper.metadata.title) == normalize_title(&hit.title)
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn summarize_abstract(text: &str) -> String {
    const MAX_CHARS: usize = 280;
    let text = text.trim();
    if text.len() <= MAX_CHARS {
        return text.to_string();
    }
    let mut out = text.chars().take(MAX_CHARS).collect::<String>();
    out.push_str("...");
    out
}

pub fn map_error(error: paperseed::PaperseedError) -> ZoteroMcpError {
    match error {
        paperseed::PaperseedError::Io(error) => ZoteroMcpError::Config(error.to_string()),
        paperseed::PaperseedError::Json(error) => ZoteroMcpError::Serde(error.to_string()),
        paperseed::PaperseedError::Http(error) => ZoteroMcpError::Http(error.to_string()),
        paperseed::PaperseedError::NotAFile(path) => ZoteroMcpError::InvalidInput(format!(
            "Paperseed import failed: path is not a file: {}\nTry:\n  paperseed corpus import <file> --license user-owned-private",
            path.display()
        )),
        paperseed::PaperseedError::PaperNotFound(id) => ZoteroMcpError::InvalidInput(format!(
            "Paperseed corpus paper not found: {id}\nTry:\n  paperseed corpus query -q <terms>\n  paperseed corpus status"
        )),
        paperseed::PaperseedError::PolicyBlocked { reason } => {
            ZoteroMcpError::InvalidInput(format!(
                "Paperseed policy blocked this action: {reason}\nTry:\n  paperseed corpus import <file> --license cc-by\n  paperseed seed check --paper-id <id>"
            ))
        }
    }
}
