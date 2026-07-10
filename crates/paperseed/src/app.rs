use crate::corpus::import_local_file;
use crate::db::{CorpusDb, IndexedPaper, QueryHit};
use crate::error::{PaperseedError, Result};
use crate::indexing;
use crate::models::{CorpusAction, License, LocalPaper};
use crate::policy::{evaluate, license_slug, parse_license};
use crate::sources::{PaperbridgeMetadata, apply_metadata};
use crate::storage::content_addressed_path;
use crate::yams::{
    CommandYamsRunner, YamsConfig, YamsIndexRequest, YamsRunner, cat_with_runner,
    index_paper_with_runner, query_with_runner,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CorpusPaths {
    pub root: PathBuf,
    pub files_dir: PathBuf,
    pub db_path: PathBuf,
    pub index_path: PathBuf,
    pub seeds_dir: PathBuf,
}

impl CorpusPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            files_dir: root.join("files"),
            db_path: root.join("corpus.json"),
            index_path: root.join("corpus.idx.json"),
            seeds_dir: root.join("seeds"),
            root,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub path: PathBuf,
    pub title: Option<String>,
    pub license: Option<String>,
    pub yams_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub path: PathBuf,
    pub metadata: PaperbridgeMetadata,
    pub license: Option<String>,
    pub yams_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedManifest {
    pub paper_id: String,
    pub title: String,
    pub hash: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub license: crate::models::License,
    pub reason: String,
}

pub fn default_corpus_root() -> PathBuf {
    xdg_data_home().join("paperbridge").join("paperseed")
}

fn xdg_data_home() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local").join("share");
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn status(paths: &CorpusPaths) -> Result<CorpusDb> {
    CorpusDb::load(&paths.db_path)
}

pub fn import(paths: &CorpusPaths, request: ImportRequest) -> Result<LocalPaper> {
    import_with_yams(paths, request, &YamsConfig::auto_detect())
}

pub fn import_with_yams(
    paths: &CorpusPaths,
    request: ImportRequest,
    yams: &YamsConfig,
) -> Result<LocalPaper> {
    let runner = CommandYamsRunner::new(&yams.binary);
    import_with_yams_runner(paths, request, yams, &runner)
}

pub fn import_with_yams_runner(
    paths: &CorpusPaths,
    request: ImportRequest,
    yams: &YamsConfig,
    runner: &impl YamsRunner,
) -> Result<LocalPaper> {
    fs::create_dir_all(&paths.files_dir)?;
    let license = request
        .license
        .as_deref()
        .map(parse_license)
        .unwrap_or(crate::models::License::UserOwnedPrivate);
    let title = request.title.unwrap_or_else(|| infer_title(&request.path));
    let mime = infer_mime(&request.path);
    let mut paper = import_local_file(&request.path, title, license, mime)?;

    let extension = request.path.extension().and_then(|ext| ext.to_str());
    let destination = content_addressed_path(&paths.files_dir, &paper.file.hash, extension);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if !destination.exists() {
        fs::copy(&request.path, &destination)?;
    }
    paper.file.path = destination;

    let full_text = extract_full_text(&request.path, &paper.file.mime)?;
    let mut yams_hash = request.yams_hash;
    if yams.enabled && yams_hash.is_none() {
        yams_hash = index_paper_with_runner(
            yams,
            runner,
            YamsIndexRequest {
                paper: &paper,
                full_text: full_text.as_deref(),
            },
        );
    }
    let mut db = CorpusDb::load(&paths.db_path)?;
    db.upsert(IndexedPaper {
        paper: paper.clone(),
        full_text: full_text.clone(),
        yams_hash,
    });
    db.save(&paths.db_path)?;
    save_index(paths, &db);
    Ok(paper)
}

pub fn ingest(paths: &CorpusPaths, request: IngestRequest) -> Result<LocalPaper> {
    ingest_with_yams(paths, request, &YamsConfig::auto_detect())
}

pub fn ingest_with_yams(
    paths: &CorpusPaths,
    request: IngestRequest,
    yams: &YamsConfig,
) -> Result<LocalPaper> {
    let mut paper = import_with_yams(
        paths,
        ImportRequest {
            path: request.path,
            title: request.metadata.title.clone(),
            license: request.license.or(request.metadata.license.clone()),
            yams_hash: request.yams_hash,
        },
        yams,
    )?;

    let mut db = CorpusDb::load(&paths.db_path)?;
    let full_text = db
        .get(&paper.metadata.id)
        .and_then(|entry| entry.full_text.clone());
    let yams_hash = db
        .get(&paper.metadata.id)
        .and_then(|entry| entry.yams_hash.clone());
    apply_metadata(&mut paper.metadata, request.metadata);
    db.upsert(IndexedPaper {
        paper: paper.clone(),
        full_text,
        yams_hash,
    });
    db.save(&paths.db_path)?;
    save_index(paths, &db);
    Ok(paper)
}

pub fn fetch_open_file(
    paths: &CorpusPaths,
    doi: String,
    path: PathBuf,
    title: Option<String>,
    license: Option<String>,
) -> Result<LocalPaper> {
    let license = license
        .as_deref()
        .map(parse_license)
        .unwrap_or(License::Unknown);
    let decision = evaluate(CorpusAction::Download, license);
    if !decision.allowed {
        return Err(PaperseedError::PolicyBlocked {
            reason: decision.reason.to_string(),
        });
    }

    let mut paper = import(
        paths,
        ImportRequest {
            path,
            title: title.or_else(|| Some(format!("DOI {doi}"))),
            license: Some(license_slug(license).to_string()),
            yams_hash: None,
        },
    )?;
    paper.metadata.doi = Some(doi);

    let mut db = CorpusDb::load(&paths.db_path)?;
    let full_text = db
        .get(&paper.metadata.id)
        .and_then(|entry| entry.full_text.clone());
    db.upsert(IndexedPaper {
        paper: paper.clone(),
        full_text,
        yams_hash: None,
    });
    db.save(&paths.db_path)?;
    save_index(paths, &db);
    Ok(paper)
}

pub fn query(paths: &CorpusPaths, q: &str) -> Result<Vec<QueryHit>> {
    query_with_yams(paths, q, &YamsConfig::auto_detect())
}

pub fn query_with_yams(paths: &CorpusPaths, q: &str, yams: &YamsConfig) -> Result<Vec<QueryHit>> {
    if yams.enabled {
        let runner = CommandYamsRunner::new(&yams.binary);
        if let Some(hits) = query_with_runner(yams, &runner, q) {
            return Ok(hits);
        }
    }
    let db = CorpusDb::load(&paths.db_path)?;
    Ok(indexing::search(
        &db,
        &paths.index_path,
        q,
        DEFAULT_QUERY_TOP_K,
    ))
}

pub fn query_entries(paths: &CorpusPaths, q: &str) -> Result<Vec<IndexedPaper>> {
    let db = CorpusDb::load(&paths.db_path)?;
    let hits = indexing::search(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K);
    Ok(entries_from_hits(&db, hits))
}

pub fn query_entries_with_yams(
    paths: &CorpusPaths,
    q: &str,
    yams: &YamsConfig,
) -> Result<Vec<IndexedPaper>> {
    let runner = CommandYamsRunner::new(&yams.binary);
    query_entries_with_yams_runner(paths, q, yams, &runner)
}

pub fn query_entries_with_yams_runner(
    paths: &CorpusPaths,
    q: &str,
    yams: &YamsConfig,
    runner: &impl YamsRunner,
) -> Result<Vec<IndexedPaper>> {
    let db = CorpusDb::load(&paths.db_path)?;
    if yams.enabled
        && let Some(hits) = query_with_runner(yams, runner, q)
    {
        let entries = entries_from_hits(&db, hits);
        if !entries.is_empty() {
            return Ok(entries);
        }
    }
    let hits = indexing::search(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K);
    Ok(entries_from_hits(&db, hits))
}

/// Same as [`query_entries_with_yams`] but also returns the BM25F relevance
/// score per hit. `None` indicates the hit came from the YAMS path which does
/// not produce comparable scores.
pub fn query_entries_scored_with_yams(
    paths: &CorpusPaths,
    q: &str,
    yams: &YamsConfig,
) -> Result<Vec<(IndexedPaper, Option<f32>)>> {
    let runner = CommandYamsRunner::new(&yams.binary);
    let db = CorpusDb::load(&paths.db_path)?;
    if yams.enabled
        && let Some(hits) = query_with_runner(yams, &runner, q)
    {
        let entries: Vec<(IndexedPaper, Option<f32>)> = hits
            .into_iter()
            .filter_map(|hit| db.get(&hit.id).cloned().map(|entry| (entry, None)))
            .collect();
        if !entries.is_empty() {
            return Ok(entries);
        }
    }
    let scored = indexing::search_scored(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K);
    Ok(scored
        .into_iter()
        .filter_map(|(hit, score)| db.get(&hit.id).cloned().map(|entry| (entry, Some(score))))
        .collect())
}

/// Default top-K for index-backed corpus queries. Generous enough to feed the
/// downstream merge step without over-collecting.
pub const DEFAULT_QUERY_TOP_K: usize = 64;

fn save_index(paths: &CorpusPaths, db: &CorpusDb) {
    if let Err(err) = indexing::persist_index(db, &paths.index_path) {
        // Non-fatal: query path falls back to in-memory build on miss. Log
        // via tracing once the crate adopts it; for now, swallow and move on.
        let _ = err;
    }
}

/// Rebuild the BM25F index from `corpus.json`. Used by `paperseed reindex`.
pub fn reindex(paths: &CorpusPaths) -> Result<usize> {
    let db = CorpusDb::load(&paths.db_path)?;
    let count = db.papers.len();
    indexing::persist_index(&db, &paths.index_path)?;
    Ok(count)
}

pub fn get_entry(paths: &CorpusPaths, paper_id: &str) -> Result<IndexedPaper> {
    CorpusDb::load(&paths.db_path)?
        .get(paper_id)
        .cloned()
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))
}

pub fn get_full_text(paths: &CorpusPaths, paper_id: &str, yams: &YamsConfig) -> Result<String> {
    let runner = CommandYamsRunner::new(&yams.binary);
    get_full_text_with_yams_runner(paths, paper_id, yams, &runner)
}

pub fn get_full_text_with_yams_runner(
    paths: &CorpusPaths,
    paper_id: &str,
    yams: &YamsConfig,
    runner: &impl YamsRunner,
) -> Result<String> {
    let mut db = CorpusDb::load(&paths.db_path)?;
    let entry = db
        .get(paper_id)
        .cloned()
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
    if let Some(full_text) = entry.full_text {
        return Ok(full_text);
    }
    if yams.enabled
        && let Some(hash) = entry.yams_hash.as_deref()
        && let Some(full_text) = cat_with_runner(yams, runner, hash)
    {
        let mut updated = entry;
        updated.full_text = Some(full_text.clone());
        db.upsert(updated);
        db.save(&paths.db_path)?;
        return Ok(full_text);
    }
    // Last resort: try to extract text from the stored file
    if let Some(full_text) = extract_full_text(&entry.paper.file.path, &entry.paper.file.mime)? {
        let mut updated = entry;
        updated.full_text = Some(full_text.clone());
        db.upsert(updated);
        db.save(&paths.db_path)?;
        return Ok(full_text);
    }
    Err(PaperseedError::PaperNotFound(paper_id.to_string()))
}

fn entries_from_hits(db: &CorpusDb, hits: Vec<QueryHit>) -> Vec<IndexedPaper> {
    hits.into_iter()
        .filter_map(|hit| db.get(&hit.id).cloned())
        .collect()
}

pub fn seed_check(paths: &CorpusPaths, paper_id: &str) -> Result<&'static str> {
    let db = CorpusDb::load(&paths.db_path)?;
    let entry = db
        .get(paper_id)
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
    let decision = evaluate(CorpusAction::SeedRedistribute, entry.paper.metadata.license);
    if decision.allowed {
        return Ok(decision.reason);
    }
    Err(PaperseedError::PolicyBlocked {
        reason: decision.reason.to_string(),
    })
}

pub fn create_seed_manifest(paths: &CorpusPaths, paper_id: &str) -> Result<SeedManifest> {
    let reason = seed_check(paths, paper_id)?.to_string();
    let db = CorpusDb::load(&paths.db_path)?;
    let entry = db
        .get(paper_id)
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
    let manifest = SeedManifest {
        paper_id: entry.paper.metadata.id.clone(),
        title: entry.paper.metadata.title.clone(),
        hash: entry.paper.file.hash.clone(),
        path: entry.paper.file.path.clone(),
        size_bytes: entry.paper.file.size_bytes,
        license: entry.paper.metadata.license,
        reason,
    };
    fs::create_dir_all(&paths.seeds_dir)?;
    let manifest_path = paths.seeds_dir.join(format!("{}.json", manifest.paper_id));
    fs::write(
        &manifest_path,
        format!(
            "{}
",
            serde_json::to_string_pretty(&manifest)?
        ),
    )?;
    Ok(manifest)
}

fn infer_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled-paper")
        .replace(['_', '-'], " ")
}

fn infer_mime(path: &Path) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("pdf") => "application/pdf".to_string(),
        Some(ext) if ext.eq_ignore_ascii_case("txt") => "text/plain".to_string(),
        Some(ext) if ext.eq_ignore_ascii_case("md") => "text/markdown".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn extract_full_text(path: &Path, mime: &str) -> Result<Option<String>> {
    if mime.starts_with("text/") {
        return Ok(Some(fs::read_to_string(path)?));
    }
    if mime == "application/pdf" {
        let bytes = fs::read(path)?;
        return Ok(extract_pdf_text_from_bytes(&bytes));
    }
    Ok(None)
}

/// Extract normalized text directly from PDF bytes without importing them.
/// Used by Paperbridge's stateless `open_paper` fallback.
pub fn extract_pdf_text_from_bytes(bytes: &[u8]) -> Option<String> {
    let text = pdf_extract::extract_text_from_mem(bytes).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn export_bibtex(db: &CorpusDb) -> String {
    db.papers
        .iter()
        .map(|entry| {
            let metadata = &entry.paper.metadata;
            let key = metadata
                .doi
                .as_deref()
                .map(sanitize_bibtex_key)
                .unwrap_or_else(|| metadata.id.clone());
            let mut fields = vec![format!("  title = {{{}}}", escape_bibtex(&metadata.title))];
            if !metadata.authors.is_empty() {
                fields.push(format!(
                    "  author = {{{}}}",
                    escape_bibtex(&metadata.authors.join(" and "))
                ));
            }
            if let Some(year) = metadata.year {
                fields.push(format!("  year = {{{year}}}"));
            }
            if let Some(doi) = &metadata.doi {
                fields.push(format!("  doi = {{{}}}", escape_bibtex(doi)));
            }
            if let Some(venue) = &metadata.venue {
                fields.push(format!("  journal = {{{}}}", escape_bibtex(venue)));
            }
            format!("@article{{{},\n{}\n}}", key, fields.join(",\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn sanitize_bibtex_key(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
}

fn escape_bibtex(input: &str) -> String {
    input.replace('{', "\\{").replace('}', "\\}")
}
