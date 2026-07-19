use crate::corpus::paper_from_stored_file;
use crate::db::{CorpusDb, IndexedPaper, QueryHit, with_corpus_write_lock};
use crate::error::{PaperseedError, Result};
use crate::indexing;
use crate::models::{CorpusAction, License, LocalPaper};
use crate::policy::{evaluate, parse_license};
use crate::sources::{PaperbridgeMetadata, apply_metadata};
use crate::storage::{content_addressed_path, copy_and_describe_file};
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
    pub text_dir: PathBuf,
    pub seeds_dir: PathBuf,
}

impl CorpusPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            files_dir: root.join("files"),
            db_path: root.join("corpus.json"),
            index_path: root.join("corpus.idx.bin"),
            text_dir: root.join("text"),
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
    pub extract_full_text: bool,
}

#[derive(Debug, Clone)]
pub struct IngestRequest {
    pub path: PathBuf,
    pub metadata: PaperbridgeMetadata,
    pub license: Option<String>,
    pub yams_hash: Option<String>,
    pub extract_full_text: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusStatus {
    pub root: PathBuf,
    pub papers: usize,
    pub index_docs: Option<usize>,
    pub index_in_sync: bool,
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

pub fn status_summary(paths: &CorpusPaths) -> Result<CorpusStatus> {
    let db = CorpusDb::load(&paths.db_path)?;
    let papers = db.papers.len();
    let index_docs = indexing::persisted_doc_count(&paths.index_path);
    Ok(CorpusStatus {
        root: paths.root.clone(),
        papers,
        index_docs,
        index_in_sync: index_docs == Some(papers),
    })
}

pub fn list_entries(paths: &CorpusPaths) -> Result<Vec<IndexedPaper>> {
    Ok(CorpusDb::load(&paths.db_path)?.papers)
}

pub fn import(paths: &CorpusPaths, request: ImportRequest) -> Result<LocalPaper> {
    import_with_yams(paths, request, &YamsConfig::auto_detect())
}

pub fn import_with_yams(
    paths: &CorpusPaths,
    request: ImportRequest,
    yams: &YamsConfig,
) -> Result<LocalPaper> {
    let runner = CommandYamsRunner::with_timeout(&yams.binary, std::time::Duration::from_secs(35));
    import_with_yams_runner(paths, request, yams, &runner)
}

pub fn import_with_yams_runner(
    paths: &CorpusPaths,
    request: ImportRequest,
    yams: &YamsConfig,
    runner: &impl YamsRunner,
) -> Result<LocalPaper> {
    let license = request
        .license
        .as_deref()
        .map(parse_license)
        .unwrap_or(crate::models::License::UserOwnedPrivate);
    let title = request.title.unwrap_or_else(|| infer_title(&request.path));
    let (paper, full_text) = prepare_local_file(
        paths,
        &request.path,
        title,
        license,
        request.extract_full_text,
    )?;
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
    persist_entry(
        paths,
        IndexedPaper {
            paper: paper.clone(),
            full_text,
            full_text_path: None,
            yams_hash,
        },
    )?;
    Ok(paper)
}

fn prepare_local_file(
    paths: &CorpusPaths,
    source: &Path,
    title: String,
    license: License,
    should_extract_full_text: bool,
) -> Result<(LocalPaper, Option<String>)> {
    fs::create_dir_all(&paths.files_dir)?;
    let mime = infer_mime(source);
    let file = copy_and_describe_file(source, &paths.files_dir, mime)?;
    let paper = paper_from_stored_file(file, title, license)?;
    let full_text = if should_extract_full_text {
        extract_full_text(&paper.file.path, &paper.file.mime)?
    } else {
        None
    };
    Ok((paper, full_text))
}

fn persist_entry(paths: &CorpusPaths, entry: IndexedPaper) -> Result<()> {
    with_corpus_write_lock(|| {
        let mut db = CorpusDb::load(&paths.db_path)?;
        let paper_id = entry.paper.metadata.id.clone();
        db.upsert(entry);
        externalize_full_text(paths, &mut db)?;
        db.save(&paths.db_path)?;
        indexing::persist_upsert(&db, &paths.index_path, &paper_id)?;
        Ok(())
    })
}

fn persist_full_text(paths: &CorpusPaths, entry: IndexedPaper) -> Result<()> {
    with_corpus_write_lock(|| {
        let mut db = CorpusDb::load(&paths.db_path)?;
        let paper_id = entry.paper.metadata.id.clone();
        db.upsert(entry);
        externalize_full_text(paths, &mut db)?;
        db.save(&paths.db_path)?;
        indexing::persist_upsert(&db, &paths.index_path, &paper_id)?;
        Ok(())
    })
}

fn externalize_full_text(paths: &CorpusPaths, db: &mut CorpusDb) -> Result<()> {
    for entry in &mut db.papers {
        let Some(text) = entry.full_text.take() else {
            continue;
        };
        let path = content_addressed_path(&paths.text_dir, &entry.paper.file.hash, Some("txt"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = path.with_extension(format!("txt.tmp.{}-{nonce}", std::process::id()));
        fs::write(&temp, text.as_bytes())?;
        fs::rename(&temp, &path)?;
        entry.full_text_path = Some(path);
    }
    Ok(())
}

pub fn ingest(paths: &CorpusPaths, request: IngestRequest) -> Result<LocalPaper> {
    ingest_with_yams(paths, request, &YamsConfig::auto_detect())
}

pub fn ingest_with_yams(
    paths: &CorpusPaths,
    request: IngestRequest,
    yams: &YamsConfig,
) -> Result<LocalPaper> {
    let runner = CommandYamsRunner::with_timeout(&yams.binary, std::time::Duration::from_secs(35));
    ingest_with_yams_runner(paths, request, yams, &runner)
}

pub fn ingest_with_yams_runner(
    paths: &CorpusPaths,
    request: IngestRequest,
    yams: &YamsConfig,
    runner: &impl YamsRunner,
) -> Result<LocalPaper> {
    let IngestRequest {
        path,
        metadata,
        license,
        yams_hash,
        extract_full_text,
    } = request;
    let resolved_license = license
        .or_else(|| metadata.license.clone())
        .as_deref()
        .map(parse_license)
        .unwrap_or(License::UserOwnedPrivate);
    let title = metadata.title.clone().unwrap_or_else(|| infer_title(&path));
    let (mut paper, full_text) =
        prepare_local_file(paths, &path, title, resolved_license, extract_full_text)?;
    apply_metadata(&mut paper.metadata, metadata);
    let yams_hash = if yams_hash.is_some() || !yams.enabled {
        yams_hash
    } else {
        index_paper_with_runner(
            yams,
            runner,
            YamsIndexRequest {
                paper: &paper,
                full_text: full_text.as_deref(),
            },
        )
    };
    persist_entry(
        paths,
        IndexedPaper {
            paper: paper.clone(),
            full_text,
            full_text_path: None,
            yams_hash,
        },
    )?;
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
    indexing::search(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K)
}

pub fn query_entries(paths: &CorpusPaths, q: &str) -> Result<Vec<IndexedPaper>> {
    let db = CorpusDb::load(&paths.db_path)?;
    let hits = indexing::search(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K)?;
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
    let hits = indexing::search(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K)?;
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
            .filter_map(|hit| {
                db.get(&hit.id)
                    .ok()
                    .flatten()
                    .cloned()
                    .map(|entry| (entry, None))
            })
            .collect();
        if !entries.is_empty() {
            return Ok(entries);
        }
    }
    let scored = indexing::search_scored(&db, &paths.index_path, q, DEFAULT_QUERY_TOP_K)?;
    Ok(scored
        .into_iter()
        .filter_map(|(hit, score)| {
            db.get(&hit.id)
                .ok()
                .flatten()
                .cloned()
                .map(|entry| (entry, Some(score)))
        })
        .collect())
}

/// Default top-K for index-backed corpus queries. Generous enough to feed the
/// downstream merge step without over-collecting.
pub const DEFAULT_QUERY_TOP_K: usize = 64;

/// Rebuild the BM25F index from `corpus.json`. Used by `paperseed reindex`.
pub fn reindex(paths: &CorpusPaths) -> Result<usize> {
    let db = CorpusDb::load(&paths.db_path)?;
    let count = db.papers.len();
    indexing::persist_index(&db, &paths.index_path)?;
    Ok(count)
}

pub fn get_entry(paths: &CorpusPaths, paper_id: &str) -> Result<IndexedPaper> {
    CorpusDb::load(&paths.db_path)?
        .get(paper_id)?
        .cloned()
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))
}

pub fn remove_entry(paths: &CorpusPaths, paper_id: &str) -> Result<IndexedPaper> {
    let removed = with_corpus_write_lock(|| {
        let mut db = CorpusDb::load(&paths.db_path)?;
        let entry = db
            .get(paper_id)?
            .cloned()
            .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
        db.papers
            .retain(|candidate| candidate.paper.file.hash != entry.paper.file.hash);
        externalize_full_text(paths, &mut db)?;
        db.save(&paths.db_path)?;
        indexing::persist_index(&db, &paths.index_path)?;
        Ok(entry)
    })?;

    match fs::remove_file(&removed.paper.file.path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(path) = &removed.full_text_path {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let manifest = paths
        .seeds_dir
        .join(format!("{}.json", removed.paper.metadata.id));
    match fs::remove_file(manifest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(removed)
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
    let db = CorpusDb::load(&paths.db_path)?;
    let entry = db
        .get(paper_id)?
        .cloned()
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
    if let Some(full_text) = entry.read_full_text()? {
        return Ok(full_text);
    }
    if yams.enabled
        && let Some(hash) = entry.yams_hash.as_deref()
        && let Some(full_text) = cat_with_runner(yams, runner, hash)
    {
        let mut updated = entry;
        updated.full_text = Some(full_text.clone());
        updated.full_text_path = None;
        persist_full_text(paths, updated)?;
        return Ok(full_text);
    }
    // Last resort: try to extract text from the stored file
    if let Some(full_text) = extract_full_text(&entry.paper.file.path, &entry.paper.file.mime)? {
        let mut updated = entry;
        updated.full_text = Some(full_text.clone());
        updated.full_text_path = None;
        persist_full_text(paths, updated)?;
        return Ok(full_text);
    }
    Err(PaperseedError::PaperNotFound(paper_id.to_string()))
}

fn entries_from_hits(db: &CorpusDb, hits: Vec<QueryHit>) -> Vec<IndexedPaper> {
    hits.into_iter()
        .filter_map(|hit| db.get(&hit.id).ok().flatten().cloned())
        .collect()
}

pub fn seed_check(paths: &CorpusPaths, paper_id: &str) -> Result<&'static str> {
    let db = CorpusDb::load(&paths.db_path)?;
    let entry = db
        .get(paper_id)?
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
        .get(paper_id)?
        .ok_or_else(|| PaperseedError::PaperNotFound(paper_id.to_string()))?;
    let actual_hash = crate::storage::hash_file(&entry.paper.file.path)?;
    if actual_hash != entry.paper.file.hash {
        return Err(PaperseedError::IntegrityMismatch {
            path: entry.paper.file.path.clone(),
            expected: entry.paper.file.hash.clone(),
            actual: actual_hash,
        });
    }
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
