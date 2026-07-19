//! Paperseed integration boundary.
//!
//! Paperbridge owns research/search user flows and MCP/API exposure. Paperseed
//! owns local corpus storage and license-gated seed manifests. This module keeps
//! that boundary explicit so Paperbridge can call Paperseed as a library
//! without growing a second corpus/seeding CLI.

use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    CachedPaperDetail, CachedPaperSummary, FulltextContent, PaperHit, PaperSource,
};
use paperseed::app::{CorpusPaths, ImportRequest, IngestRequest, SeedManifest};
use paperseed::db::{CorpusDb, IndexedPaper, QueryHit};
use paperseed::models::LocalPaper;
use paperseed::resolver::{ResolvedOpenPaper, ResolverClient, SearchResult};
use paperseed::sources::PaperbridgeMetadata;
use paperseed::yams::{
    CommandYamsRunner, YamsConfig, YamsDownloadRequest, YamsDownloadResult, YamsResearchHit,
    YamsRunner, YamsStoredDocument, cat_with_runner, list_research_group_with_runner,
    query_research_with_runner,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PaperseedApi {
    paths: CorpusPaths,
    resolver: ResolverClient,
    yams: YamsConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResearchPaperHit {
    pub hash: String,
    pub path: PathBuf,
    pub title: String,
    pub snippet: String,
    pub score: f32,
    pub content_available: bool,
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

    pub fn corpus_status_summary(&self) -> Result<paperseed::app::CorpusStatus> {
        paperseed::app::status_summary(&self.paths).map_err(map_error)
    }

    pub fn list_corpus_entries(&self) -> Result<Vec<IndexedPaper>> {
        paperseed::app::list_entries(&self.paths).map_err(map_error)
    }

    pub fn remove_corpus_entry(&self, paper_id: &str) -> Result<IndexedPaper> {
        paperseed::app::remove_entry(&self.paths, paper_id).map_err(map_error)
    }

    pub fn reindex_corpus(&self) -> Result<usize> {
        paperseed::app::reindex(&self.paths).map_err(map_error)
    }

    pub fn import_local_file(
        &self,
        path: impl AsRef<Path>,
        title: Option<String>,
        license: Option<String>,
    ) -> Result<LocalPaper> {
        self.import_local_file_with_options(path, title, license, true)
    }

    pub fn import_local_file_with_options(
        &self,
        path: impl AsRef<Path>,
        title: Option<String>,
        license: Option<String>,
        extract_full_text: bool,
    ) -> Result<LocalPaper> {
        paperseed::app::import_with_yams(
            &self.paths,
            ImportRequest {
                path: path.as_ref().to_path_buf(),
                title,
                license,
                yams_hash: None,
                extract_full_text,
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
        self.ingest_with_metadata_options(path, metadata, license, true)
    }

    pub fn ingest_with_metadata_options(
        &self,
        path: impl AsRef<Path>,
        metadata: PaperbridgeMetadata,
        license: Option<String>,
        extract_full_text: bool,
    ) -> Result<LocalPaper> {
        paperseed::app::ingest_with_yams(
            &self.paths,
            IngestRequest {
                path: path.as_ref().to_path_buf(),
                metadata,
                license,
                yams_hash: None,
                extract_full_text,
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

    pub fn research_enabled(&self) -> bool {
        self.yams.enabled
    }

    pub fn search_research_papers(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ResearchPaperHit>> {
        if !self.yams.enabled {
            return Err(ZoteroMcpError::MissingConfig(
                "YAMS research search is not ready".into(),
            ));
        }
        let runner = CommandYamsRunner::with_timeout(&self.yams.binary, Duration::from_secs(8));
        // Current YAMS deployments reliably serve agent-sized prefixes up to
        // 20 results; larger limits may fall back to local initialization and
        // fail even while the daemon is healthy.
        let fetch_limit = limit.saturating_mul(2).clamp(10, 20);
        let hits = query_research_with_runner(&self.yams, &runner, query, fetch_limit).map_err(
            |reason| ZoteroMcpError::InvalidInput(format!("YAMS research search failed: {reason}")),
        )?;
        Ok(group_research_hits(&self.yams, &runner, hits, limit))
    }

    pub fn get_research_content(&self, hash: &str) -> Result<String> {
        if !self.yams.enabled {
            return Err(ZoteroMcpError::MissingConfig(
                "YAMS research content is not available".into(),
            ));
        }
        let runner = CommandYamsRunner::with_timeout(&self.yams.binary, Duration::from_secs(5));
        let primary = cat_with_runner(&self.yams, &runner, hash).ok_or_else(|| {
            ZoteroMcpError::InvalidInput(format!(
                "Research document '{hash}' is stale: YAMS can search its metadata but has no readable content.\nTry:\n  yams doctor\n  yams add <paper-or-source-file>"
            ))
        })?;
        let path = query_research_with_runner(&self.yams, &runner, hash, 1)
            .ok()
            .and_then(|hits| hits.into_iter().find(|hit| hit.hash == hash))
            .map(|hit| hit.path);
        let Some(group) = path.as_deref().and_then(research_group_path) else {
            return Ok(primary);
        };
        let documents =
            list_research_group_with_runner(&self.yams, &runner, &group, 50).unwrap_or_default();
        Ok(assemble_research_bundle(
            &self.yams, &runner, &primary, documents,
        ))
    }

    pub fn search_cached_papers(&self, query: &str, limit: usize) -> Result<Vec<PaperHit>> {
        let scored = paperseed::app::query_entries_scored_with_yams(&self.paths, query, &self.yams)
            .map_err(map_error)?;
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(entry, score)| PaperHit {
                hit_id: None,
                source: PaperSource::Paperseed,
                title: entry.paper.metadata.title.clone(),
                authors: entry.paper.metadata.authors.clone(),
                year: entry.paper.metadata.year.map(|year| year.to_string()),
                doi: entry.paper.metadata.doi.clone(),
                arxiv_id: entry.paper.metadata.arxiv_id.clone(),
                pmid: None,
                abstract_note: entry
                    .paper
                    .metadata
                    .abstract_note
                    .as_deref()
                    .map(summarize_abstract),
                url: entry.paper.metadata.source_url.clone(),
                pdf_url: Some(entry.paper.file.path.display().to_string()),
                oa_pdf_url: entry.paper.metadata.source_url.clone(),
                venue: entry.paper.metadata.venue.clone(),
                citation_count: None,
                cache: Some(CachedPaperSummary {
                    paper_id: entry.paper.metadata.id.clone(),
                    cached: true,
                    has_full_text: entry.has_full_text(),
                    yams_indexed: entry.yams_hash.is_some(),
                }),
                relevance_score: score,
                ids: None,
                match_info: None,
                access: None,
                next: Vec::new(),
            })
            .collect())
    }

    pub fn find_cached_hit(&self, hit: &PaperHit) -> Option<IndexedPaper> {
        let db = self.corpus_status().ok()?;
        db.papers.into_iter().find(|entry| {
            doi_matches(entry, hit) || source_url_matches(entry, hit) || title_matches(entry, hit)
        })
    }

    /// Resolve an agent-supplied identifier against the corpus without using
    /// full-text ranking. Identifier lookup must never turn a nearby BM25 hit
    /// into the requested paper.
    pub fn find_cached_identity(
        &self,
        doi: Option<&str>,
        arxiv_id: Option<&str>,
        url: Option<&str>,
    ) -> Option<IndexedPaper> {
        let db = self.corpus_status().ok()?;
        db.papers.into_iter().find(|entry| {
            doi.is_some_and(|doi| entry_doi_matches(entry, doi))
                || arxiv_id.is_some_and(|id| entry_arxiv_matches(entry, id))
                || url.is_some_and(|url| entry_url_matches(entry, url))
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
    let has_full_text = entry.has_full_text();
    CachedPaperDetail {
        paper_id: entry.paper.metadata.id,
        title: entry.paper.metadata.title,
        authors: entry.paper.metadata.authors,
        year: entry.paper.metadata.year.map(|year| year.to_string()),
        doi: entry.paper.metadata.doi,
        arxiv_id: entry.paper.metadata.arxiv_id,
        venue: entry.paper.metadata.venue,
        abstract_note: entry.paper.metadata.abstract_note,
        source_url: entry.paper.metadata.source_url,
        stored_path: entry.paper.file.path.display().to_string(),
        mime: entry.paper.file.mime,
        yams_hash: entry.yams_hash,
        has_full_text,
    }
}

fn doi_matches(entry: &IndexedPaper, hit: &PaperHit) -> bool {
    matches!(
        (entry.paper.metadata.doi.as_deref(), hit.doi.as_deref()),
        (Some(left), Some(right)) if left.eq_ignore_ascii_case(right)
    )
}

fn entry_doi_matches(entry: &IndexedPaper, expected: &str) -> bool {
    entry
        .paper
        .metadata
        .doi
        .as_deref()
        .and_then(normalize_doi)
        .zip(normalize_doi(expected))
        .is_some_and(|(actual, expected)| actual == expected)
}

fn entry_arxiv_matches(entry: &IndexedPaper, expected: &str) -> bool {
    let expected = normalize_arxiv_id(expected);
    if expected.is_empty() {
        return false;
    }
    if entry
        .paper
        .metadata
        .arxiv_id
        .as_deref()
        .is_some_and(|id| normalize_arxiv_id(id) == expected)
    {
        return true;
    }
    if entry
        .paper
        .metadata
        .doi
        .as_deref()
        .and_then(normalize_doi)
        .is_some_and(|doi| doi == format!("10.48550/arxiv.{expected}"))
    {
        return true;
    }
    entry
        .paper
        .metadata
        .source_url
        .as_deref()
        .is_some_and(|url| arxiv_id_from_url(url).as_deref() == Some(expected.as_str()))
}

fn entry_url_matches(entry: &IndexedPaper, expected: &str) -> bool {
    entry
        .paper
        .metadata
        .source_url
        .as_deref()
        .and_then(canonical_url)
        .zip(canonical_url(expected))
        .is_some_and(|(actual, expected)| actual == expected)
}

fn normalize_doi(raw: &str) -> Option<String> {
    let lower = raw.trim().to_ascii_lowercase();
    let value = lower
        .strip_prefix("https://doi.org/")
        .or_else(|| lower.strip_prefix("http://doi.org/"))
        .or_else(|| lower.strip_prefix("doi:"))
        .unwrap_or(&lower)
        .trim();
    value.starts_with("10.").then(|| value.to_string())
}

fn normalize_arxiv_id(raw: &str) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    let value = lower
        .strip_prefix("https://arxiv.org/abs/")
        .or_else(|| lower.strip_prefix("http://arxiv.org/abs/"))
        .or_else(|| lower.strip_prefix("https://arxiv.org/pdf/"))
        .or_else(|| lower.strip_prefix("http://arxiv.org/pdf/"))
        .or_else(|| lower.strip_prefix("arxiv:"))
        .unwrap_or(&lower)
        .trim_end_matches(".pdf");
    strip_arxiv_version(value).to_string()
}

fn strip_arxiv_version(id: &str) -> &str {
    id.rfind('v')
        .and_then(|index| {
            let suffix = &id[index + 1..];
            (!suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
                .then_some(&id[..index])
        })
        .unwrap_or(id)
}

fn arxiv_id_from_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    if !parsed.host_str()?.eq_ignore_ascii_case("arxiv.org") {
        return None;
    }
    let path = parsed.path().trim_start_matches('/');
    let id = path
        .strip_prefix("abs/")
        .or_else(|| path.strip_prefix("pdf/"))?;
    Some(normalize_arxiv_id(id))
}

fn canonical_url(raw: &str) -> Option<String> {
    let mut parsed = url::Url::parse(raw.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);
    if parsed.path() != "/" {
        let path = parsed.path().trim_end_matches('/').to_string();
        parsed.set_path(&path);
    }
    Some(parsed.to_string())
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

fn group_research_hits(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    hits: Vec<YamsResearchHit>,
    limit: usize,
) -> Vec<ResearchPaperHit> {
    let mut groups: HashMap<String, Vec<YamsResearchHit>> = HashMap::new();
    for hit in hits {
        if let Some(key) = research_group_key(&hit.path) {
            groups.entry(key).or_default().push(hit);
        }
    }

    let mut grouped = groups.into_values().collect::<Vec<_>>();
    grouped.sort_by(|left, right| {
        let left_score = left.iter().map(|hit| hit.score).fold(0.0_f64, f64::max);
        let right_score = right.iter().map(|hit| hit.score).fold(0.0_f64, f64::max);
        right_score.total_cmp(&left_score)
    });
    grouped.truncate(limit);

    let mut papers = grouped
        .into_iter()
        .filter_map(|mut candidates| {
            candidates.sort_by(|left, right| {
                representation_priority(&right.path)
                    .cmp(&representation_priority(&left.path))
                    .then_with(|| right.score.total_cmp(&left.score))
            });
            let score = candidates
                .iter()
                .map(|hit| hit.score)
                .fold(0.0_f64, f64::max) as f32;
            let pdf_title = candidates
                .iter()
                .filter(|hit| extension(&hit.path) == "pdf")
                .find_map(|hit| title_from_snippet(&hit.snippet));

            let mut selected = None;
            let mut selected_content = None;
            for candidate in candidates.iter().take(4) {
                if let Some(content) = cat_with_runner(config, runner, &candidate.hash) {
                    selected = Some(candidate.clone());
                    selected_content = Some(content);
                    break;
                }
            }
            let selected = selected.or_else(|| candidates.first().cloned())?;
            let content_available = selected_content.is_some();
            let title = pdf_title
                .or_else(|| selected_content.as_deref().and_then(title_from_content))
                .or_else(|| title_from_snippet(&selected.snippet))
                .unwrap_or_else(|| fallback_research_title(&selected.path));
            Some(ResearchPaperHit {
                hash: selected.hash,
                path: selected.path,
                title,
                snippet: selected.snippet,
                score,
                content_available,
            })
        })
        .collect::<Vec<_>>();
    papers.sort_by(|left, right| right.score.total_cmp(&left.score));
    papers.truncate(limit);
    papers
}

fn research_group_key(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if !text.contains("/research/") {
        return None;
    }
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if let Some(index) = components.iter().position(|component| {
        component
            .strip_prefix("paper-")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
    }) {
        return Some(components[..=index].join("/"));
    }
    (extension(path) == "pdf").then(|| text.to_string())
}

fn research_group_path(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        name.strip_prefix("paper-")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
            .then(|| ancestor.to_path_buf())
    })
}

fn assemble_research_bundle(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    primary: &str,
    documents: Vec<YamsStoredDocument>,
) -> String {
    const ORDER: [&str; 7] = [
        "abstract",
        "introduction",
        "design",
        "results",
        "related",
        "conclusion",
        "appendix",
    ];
    let mut selected: HashMap<&'static str, YamsStoredDocument> = HashMap::new();
    for document in documents {
        let Some(kind) = research_section_kind(&document.path) else {
            continue;
        };
        let replace = selected.get(kind).is_none_or(|current| {
            canonical_section_score(&document.path, document.indexed)
                > canonical_section_score(&current.path, current.indexed)
        });
        if replace {
            selected.insert(kind, document);
        }
    }
    if selected.is_empty() {
        return primary.to_string();
    }

    let mut bundle = String::new();
    if let Some(title) = title_from_content(primary) {
        bundle.push_str("# ");
        bundle.push_str(&title);
        bundle.push_str("\n\n");
    }
    for kind in ORDER {
        let Some(document) = selected.get(kind) else {
            continue;
        };
        if let Some(content) = cat_with_runner(config, runner, &document.hash) {
            bundle.push_str(&content);
            if !bundle.ends_with('\n') {
                bundle.push('\n');
            }
            bundle.push('\n');
        }
    }
    if bundle.trim().is_empty() {
        primary.to_string()
    } else {
        bundle
    }
}

fn research_section_kind(path: &Path) -> Option<&'static str> {
    let name = path.file_stem()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "abstract" => Some("abstract"),
        "introduction" => Some("introduction"),
        "design" | "method" | "methods" => Some("design"),
        "results" | "evaluation" => Some("results"),
        "related" | "related-work" | "related_work" => Some("related"),
        "conclusion" | "conclusions" => Some("conclusion"),
        "appendix" => Some("appendix"),
        _ => None,
    }
}

fn canonical_section_score(path: &Path, indexed: i64) -> (u8, i64) {
    let path = path.to_string_lossy();
    let canonical = (!path.contains("draft") && path.contains("/tex/")) as u8;
    (canonical, indexed)
}

fn representation_priority(path: &Path) -> u8 {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match (extension(path), name.as_str()) {
        ("pdf", _) => 100,
        (_, "paper.tex" | "main.tex") => 90,
        (_, "abstract.tex") => 80,
        (_, "introduction.tex") => 70,
        ("tex", _) => 60,
        ("md", _) => 50,
        _ => 10,
    }
}

fn extension(path: &Path) -> &str {
    path.extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
}

fn title_from_content(content: &str) -> Option<String> {
    if let Some(after) = content.split("\\title{").nth(1)
        && let Some(title) = after.split('}').next()
    {
        return cleaned_title(title);
    }
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix('#').and_then(cleaned_title))
}

fn title_from_snippet(snippet: &str) -> Option<String> {
    let text = snippet.trim().trim_start_matches('#').trim();
    let end = [" [Author", " [Affiliation", " Abstract", "Abstract—"]
        .into_iter()
        .filter_map(|marker| text.find(marker))
        .min()
        .unwrap_or(text.len());
    cleaned_title(&text[..end.min(text.len())])
}

fn cleaned_title(raw: &str) -> Option<String> {
    let title = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty() && title.chars().count() <= 240).then_some(title)
}

fn fallback_research_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Research paper")
        .replace(['-', '_'], " ")
}

pub fn map_error(error: paperseed::PaperseedError) -> ZoteroMcpError {
    match error {
        paperseed::PaperseedError::Io(error) => ZoteroMcpError::Config(error.to_string()),
        paperseed::PaperseedError::Json(error) => ZoteroMcpError::Serde(error.to_string()),
        paperseed::PaperseedError::CorruptCorpus { .. } => {
            ZoteroMcpError::Config(error.to_string())
        }
        paperseed::PaperseedError::Http(error) => ZoteroMcpError::Http(error.to_string()),
        paperseed::PaperseedError::MissingResolverEmail => {
            ZoteroMcpError::MissingConfig(error.to_string())
        }
        paperseed::PaperseedError::NotAFile(path) => ZoteroMcpError::InvalidInput(format!(
            "Paperseed import failed: path is not a file: {}\nTry:\n  paperseed corpus import <file> --license user-owned-private",
            path.display()
        )),
        paperseed::PaperseedError::PaperNotFound(id) => ZoteroMcpError::InvalidInput(format!(
            "Paperseed corpus paper not found: {id}\nTry:\n  paperseed corpus query -q <terms>\n  paperseed corpus status"
        )),
        paperseed::PaperseedError::EmptyPaperId
        | paperseed::PaperseedError::AmbiguousPaperId { .. }
        | paperseed::PaperseedError::IntegrityMismatch { .. } => {
            ZoteroMcpError::InvalidInput(error.to_string())
        }
        paperseed::PaperseedError::PolicyBlocked { reason } => {
            ZoteroMcpError::InvalidInput(format!(
                "Paperseed policy blocked this action: {reason}\nTry:\n  paperseed corpus import <file> --license cc-by\n  paperseed seed check --paper-id <id>"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ResearchRunner {
        content: HashMap<String, String>,
    }

    impl YamsRunner for ResearchRunner {
        fn run(&self, args: &[String]) -> std::io::Result<paperseed::yams::YamsOutput> {
            let hash = args.get(1).map(String::as_str).unwrap_or_default();
            let content = self.content.get(hash).cloned().unwrap_or_default();
            Ok(paperseed::yams::YamsOutput {
                status_success: !content.is_empty(),
                stdout: content,
                stderr: String::new(),
            })
        }
    }

    fn ingest(
        api: &PaperseedApi,
        dir: &Path,
        name: &str,
        doi: &str,
        source_url: &str,
    ) -> LocalPaper {
        let path = dir.join(name);
        std::fs::write(&path, format!("full text for {name}")).unwrap();
        api.ingest_with_metadata(
            &path,
            PaperbridgeMetadata {
                title: Some(name.to_string()),
                doi: Some(doi.to_string()),
                arxiv_id: None,
                authors: vec!["Test Author".into()],
                year: Some(2024),
                venue: None,
                abstract_note: None,
                license: Some("cc-by".into()),
                source_url: Some(source_url.to_string()),
            },
            Some("cc-by".into()),
        )
        .unwrap()
    }

    #[test]
    fn cached_identity_requires_exact_doi_not_shared_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::with_yams(dir.path().join("corpus"), None, YamsConfig::disabled());
        ingest(
            &api,
            dir.path(),
            "ability.txt",
            "10.14722/futureg.2025.23099",
            "https://example.test/ability.pdf",
        );

        assert!(
            api.find_cached_identity(Some("10.14722/ndss.2023.23080"), None, None)
                .is_none()
        );
    }

    #[test]
    fn cached_identity_canonicalizes_exact_url() {
        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::with_yams(dir.path().join("corpus"), None, YamsConfig::disabled());
        let paper = ingest(
            &api,
            dir.path(),
            "hypervision.txt",
            "10.14722/ndss.2023.23080",
            "https://www.ndss-symposium.org/paper.pdf",
        );

        let found = api
            .find_cached_identity(
                None,
                None,
                Some("https://www.ndss-symposium.org/paper.pdf#page=2"),
            )
            .unwrap();
        assert_eq!(found.paper.metadata.id, paper.metadata.id);
    }

    #[test]
    fn research_results_collapse_project_fragments_and_skip_stale_pdf_content() {
        let config = YamsConfig {
            enabled: true,
            binary: "yams".into(),
        };
        let runner = ResearchRunner {
            content: HashMap::from([(
                "tex-hash".into(),
                r"\title{Which Component Drives Detection? Decomposing a Graph Neural Network Intrusion Detector}"
                    .into(),
            )]),
        };
        let hits = vec![
            YamsResearchHit {
                hash: "pdf-hash".into(),
                path: "/Users/test/work/research/papers/paper-2/paper.pdf".into(),
                score: 0.68,
                snippet: "Which Component Drives Detection? Decomposing a Graph Neural Network Intrusion Detector [Author Names Removed] Abstract...".into(),
            },
            YamsResearchHit {
                hash: "tex-hash".into(),
                path: "/Users/test/work/research/papers/paper-2/paper.tex".into(),
                score: 0.70,
                snippet: "\\documentclass{IEEEtran}".into(),
            },
            YamsResearchHit {
                hash: "related-hash".into(),
                path: "/Users/test/work/research/papers/paper-2/tex/related.tex".into(),
                score: 0.10,
                snippet: "\\section{Related Work}".into(),
            },
        ];

        let papers = group_research_hits(&config, &runner, hits, 10);

        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].hash, "tex-hash");
        assert!(papers[0].content_available);
        assert_eq!(
            papers[0].title,
            "Which Component Drives Detection? Decomposing a Graph Neural Network Intrusion Detector"
        );
    }

    #[test]
    fn research_bundle_includes_evaluation_content_in_paper_order() {
        let config = YamsConfig {
            enabled: true,
            binary: "yams".into(),
        };
        let runner = ResearchRunner {
            content: HashMap::from([
                (
                    "abstract".into(),
                    "\\begin{abstract}Summary\\end{abstract}".into(),
                ),
                ("results".into(), "\\section{Evaluation}AUC was 0.92".into()),
            ]),
        };
        let documents = vec![
            YamsStoredDocument {
                hash: "results".into(),
                path: "/research/papers/paper-2/tex/results.tex".into(),
                indexed: 2,
            },
            YamsStoredDocument {
                hash: "abstract".into(),
                path: "/research/papers/paper-2/tex/abstract.tex".into(),
                indexed: 1,
            },
        ];

        let bundle = assemble_research_bundle(
            &config,
            &runner,
            "\\title{Which Component Drives Detection?}",
            documents,
        );

        assert!(bundle.starts_with("# Which Component Drives Detection?"));
        assert!(bundle.find("Summary").unwrap() < bundle.find("AUC was 0.92").unwrap());
    }
}
