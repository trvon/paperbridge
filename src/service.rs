use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::crossref::CrossrefClient;
use crate::error::{Result, ZoteroMcpError};
use crate::external::{PaperSearch, PaperSearchOutcome, SearchOptions, UnpaywallClient};
use crate::hit_enrich::{apply_detail, enrich_hit_identity, enrich_match};
use crate::models::{
    BackendInfo, CachedPaperDetail, CachedPaperSummary, CollectionListResult, CollectionSummary,
    CollectionUpdateRequest, CollectionWriteRequest, CrossrefWork, DeleteCollectionRequest,
    DeleteItemRequest, FulltextContent, ItemDetail, ItemListResult, ItemSummary, ItemUpdateRequest,
    ItemVoxPayload, ItemWriteRequest, ListCollectionsQuery, PaperHit, PaperSource, PaperStructure,
    SearchCacheMode, SearchDiagnostics, SearchItemsQuery, SearchPapersResult, SearchVoxPayload,
    SkillPayload, SourceDiagnostic, ValidationIssue, ValidationIssueLevel, ValidationReport,
    VoxTextPayload,
};
use crate::paper;
use crate::paper::docker::DEFAULT_PORT as GROBID_DEFAULT_PORT;
use crate::paper::grobid::GrobidClient;
use crate::paper::tei;
use crate::paperseed_api::PaperseedApi;
use crate::pdf;
use crate::validation;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

pub const DEFAULT_CHUNK_SIZE: usize = 1200;
pub const DEFAULT_PIPELINE_SEARCH_LIMIT: u32 = 5;

#[derive(Debug, Clone)]
pub struct PrepareVoxTextRequest {
    pub text: Option<String>,
    pub attachment_key: Option<String>,
    pub source_label: Option<String>,
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct PrepareItemForVoxRequest {
    pub item_key: String,
    pub attachment_key: Option<String>,
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct PrepareSearchResultForVoxRequest {
    pub q: String,
    pub qmode: Option<String>,
    pub item_type: Option<String>,
    pub tag: Option<String>,
    pub result_index: Option<usize>,
    pub search_limit: Option<u32>,
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct OpenPaperRequest {
    pub hit_id: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub item_key: Option<String>,
    pub paper_id: Option<String>,
    pub attachment_key: Option<String>,
    pub url: Option<String>,
    pub want: Vec<String>,
    pub max_chars: Option<usize>,
    pub selector: Option<String>,
    pub max_chars_per_chunk: Option<usize>,
}

pub const DEFAULT_FULLTEXT_MAX_CHARS: usize = 8000;

#[derive(Debug, Clone)]
pub struct PaperseedMirrorConfig {
    pub corpus_root: Option<String>,
    pub unpaywall_email: Option<String>,
    pub auto_download: bool,
    pub yams_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct PaperConfig {
    pub grobid_url: Option<String>,
    pub grobid_auto_spawn: bool,
    pub grobid_image: String,
    pub grobid_timeout_secs: u64,
}

type PaperCacheKey = (String, String, Option<u64>);

#[derive(Clone)]
pub struct PaperbridgeService {
    backend: Arc<dyn LibraryBackend>,
    crossref: CrossrefClient,
    paper_search: PaperSearch,
    unpaywall: Option<UnpaywallClient>,
    paper_config: Option<PaperConfig>,
    paper_cache: Arc<Mutex<HashMap<PaperCacheKey, PaperStructure>>>,
    paperseed: Option<PaperseedApi>,
    paperseed_auto_download: bool,
}

impl PaperbridgeService {
    pub fn new(backend: Arc<dyn LibraryBackend>) -> Self {
        Self::with_paper_search(backend, PaperSearch::new())
    }

    pub fn with_paper_search(backend: Arc<dyn LibraryBackend>, paper_search: PaperSearch) -> Self {
        Self {
            backend,
            crossref: CrossrefClient::new(None),
            paper_search,
            unpaywall: None,
            paper_config: None,
            paper_cache: Arc::new(Mutex::new(HashMap::new())),
            paperseed: None,
            paperseed_auto_download: false,
        }
    }

    pub fn with_unpaywall(mut self, email: Option<String>) -> Self {
        self.unpaywall = email.map(|e| UnpaywallClient::new(None, e));
        self
    }

    pub fn with_paper_config(mut self, cfg: PaperConfig) -> Self {
        self.paper_config = Some(cfg);
        self
    }

    pub fn with_paperseed(mut self, cfg: PaperseedMirrorConfig) -> Self {
        let yams = if cfg.yams_enabled {
            paperseed::yams::YamsConfig::auto_detect()
        } else {
            paperseed::yams::YamsConfig::disabled()
        };
        let api = match cfg.corpus_root {
            Some(root) => PaperseedApi::with_yams(root, cfg.unpaywall_email, yams),
            None => PaperseedApi::default_with_yams(cfg.unpaywall_email, yams),
        };
        self.paperseed = Some(api);
        self.paperseed_auto_download = cfg.auto_download;
        self
    }

    pub async fn search_papers(&self, opts: SearchOptions) -> Result<SearchPapersResult> {
        // Preserve the original query for ranking (its exact form still drives
        // the DOI/arXiv exact-match short-circuit in rank_search_hits). When
        // the user pasted a DOI or arXiv URL we also rewrite the *dispatched*
        // query so external clients and the BM25F cache see the canonical
        // ID rather than a URL — otherwise the upstream APIs tokenize the URL
        // as free text and miss the actual paper.
        let original_query = opts.query.clone();
        let detail = opts.detail;
        let abstract_max_chars = opts.abstract_max_chars;
        let mut opts = opts;
        opts.validate_source_fetch_limit()?;
        if let Some(doi) = normalize_doi(&opts.query) {
            opts.query = doi;
        } else if let Some(arxiv) = normalize_arxiv_id(&opts.query) {
            opts.query = arxiv;
        }
        let cache_mode = effective_cache_mode(opts.cache_mode, opts.sources.as_deref());
        let mut diagnostics = SearchDiagnostics::default();
        let mut hits = if cache_mode == SearchCacheMode::Only {
            Vec::new()
        } else {
            let PaperSearchOutcome {
                hits,
                diagnostics: d,
            } = self.paper_search.search(opts.clone()).await?;
            diagnostics = d;
            hits
        };
        if cache_mode != SearchCacheMode::Off
            && let Some(api) = &self.paperseed
        {
            let cache_start = hits.len();
            match api.search_cached_papers(&opts.query, opts.source_fetch_limit() as usize) {
                Ok(mut cached_hits) => {
                    if cache_mode == SearchCacheMode::Auto {
                        cached_hits.retain(|hit| should_surface_cached_hit(&original_query, hit));
                    }
                    if !cached_hits.is_empty() || cache_mode == SearchCacheMode::Only {
                        diagnostics.sources_ok.push("paperseed".into());
                    }
                    hits.extend(cached_hits);
                    merge_prefer_cached(&mut hits, cache_start);
                }
                Err(e) => {
                    diagnostics.sources_failed.push(SourceDiagnostic {
                        source: "paperseed".into(),
                        reason: e.to_string(),
                    });
                }
            }
        }
        if cache_mode != SearchCacheMode::Off {
            self.annotate_cached_hits(&mut hits);
        }
        rank_search_hits(&original_query, &mut hits);

        for hit in &mut hits {
            enrich_hit_identity(hit);
            enrich_match(hit, &original_query);
            apply_detail(hit, detail, abstract_max_chars);
        }

        // Clamp to u32 via .min() — safe, bounded cast.
        let total_count = hits.len().min(u32::MAX as usize) as u32;
        self.mirror_open_access_hits(&hits);

        let offset = opts.offset;
        let page_limit = opts.page_limit();
        let start = offset.min(total_count) as usize;
        let end = (start + page_limit as usize).min(total_count as usize);
        hits = hits[start..end].to_vec();
        let has_more = end < total_count as usize;
        let next_offset = if has_more {
            Some(offset.saturating_add(page_limit))
        } else {
            None
        };

        Ok(SearchPapersResult {
            query: original_query,
            total_count,
            offset,
            limit: page_limit,
            has_more,
            next_offset,
            detail: Some(detail),
            hits,
            diagnostics: Some(diagnostics),
        })
    }

    /// Resolve a paper by id and return requested slices (metadata/fulltext/structure/chunks).
    pub async fn open_paper(&self, req: OpenPaperRequest) -> Result<serde_json::Value> {
        let max_chars = req.max_chars.unwrap_or(DEFAULT_FULLTEXT_MAX_CHARS);
        let mut resolved = resolve_open_targets(&req)?;

        // Prefer cache when paper_id known or hit_id is paperseed:
        if resolved.paper_id.is_none()
            && let Some(pid) = req.paper_id.clone()
        {
            resolved.paper_id = Some(pid);
        }

        let mut out = serde_json::Map::new();

        let wants: Vec<String> = if req.want.is_empty() {
            vec!["metadata".into()]
        } else {
            req.want
                .iter()
                .map(|w| w.trim().to_ascii_lowercase())
                .collect()
        };

        if wants.iter().any(|w| w == "metadata") {
            if let Some(doi) = resolved.doi.as_deref() {
                match self.resolve_doi(doi).await {
                    Ok(work) => {
                        out.insert("metadata".into(), serde_json::to_value(work)?);
                    }
                    Err(e) => {
                        out.insert("metadata_error".into(), serde_json::json!(e.to_string()));
                    }
                }
            } else if let Some(key) = resolved.item_key.as_deref() {
                let item = self.get_item(key).await?;
                out.insert("metadata".into(), serde_json::to_value(item)?);
            } else if let Some(pid) = resolved.paper_id.as_deref() {
                if let Some(api) = &self.paperseed
                    && let Ok(detail) = api.get_cached_paper(pid)
                {
                    out.insert("metadata".into(), serde_json::to_value(detail)?);
                } else {
                    out.insert("metadata".into(), serde_json::json!({"paper_id": pid}));
                }
            } else if let Some(arxiv) = resolved.arxiv_id.as_deref() {
                out.insert(
                    "metadata".into(),
                    serde_json::json!({
                        "arxiv_id": arxiv,
                        "url": format!("https://arxiv.org/abs/{arxiv}"),
                        "pdf_url": format!("https://arxiv.org/pdf/{arxiv}"),
                    }),
                );
            } else if let Some(url) = resolved.url.as_deref() {
                out.insert("metadata".into(), serde_json::json!({"url": url}));
            }
        }

        if wants.iter().any(|w| w == "fulltext" || w == "chunks") {
            let fulltext = self.resolve_fulltext_for_open(&mut resolved).await?;
            let truncated = truncate_fulltext(&fulltext, max_chars);
            if wants.iter().any(|w| w == "fulltext") {
                out.insert("fulltext".into(), serde_json::to_value(&truncated)?);
            }
            if wants.iter().any(|w| w == "chunks") {
                let chunk_size = req.max_chars_per_chunk.unwrap_or(DEFAULT_CHUNK_SIZE);
                let vox = pdf::prepare_vox_payload(
                    &format!("open:{}", truncated.item_key),
                    &truncated.content,
                    chunk_size,
                );
                out.insert("chunks".into(), serde_json::to_value(vox)?);
            }
        }

        if wants.iter().any(|w| w == "structure") {
            let structure = self
                .resolve_structure_for_open(&mut resolved, max_chars)
                .await?;
            if let Some(selector) = req.selector.as_deref() {
                let value = paper::query(&structure, selector)?;
                out.insert("structure".into(), value);
            } else {
                out.insert("structure".into(), serde_json::to_value(structure)?);
            }
        }

        if out.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "open_paper could not produce content. Provide hit_id, doi, arxiv_id, item_key, paper_id, attachment_key, or url. Try: search_papers { query } then open_paper { hit_id }.".into(),
            ));
        }

        out.insert(
            "resolved".into(),
            serde_json::json!({
                "hit_id": req.hit_id,
                "doi": resolved.doi,
                "arxiv_id": resolved.arxiv_id,
                "item_key": resolved.item_key,
                "paper_id": resolved.paper_id,
                "attachment_key": resolved.attachment_key,
                "url": resolved.url,
            }),
        );

        Ok(serde_json::Value::Object(out))
    }

    async fn resolve_fulltext_for_open(
        &self,
        resolved: &mut OpenResolved,
    ) -> Result<FulltextContent> {
        if let Some(att) = resolved.attachment_key.as_deref() {
            return self.get_pdf_text(att).await;
        }
        if let Some(pid) = resolved.paper_id.as_deref()
            && let Some(ft) = self.try_cached_fulltext(pid)?
        {
            return Ok(ft);
        }
        if let Some(key) = resolved.item_key.as_deref() {
            let item = self.get_item(key).await?;
            let att =
                pdf::select_attachment_for_reading(&item.attachments, None).ok_or_else(|| {
                    ZoteroMcpError::InvalidInput(format!(
                        "No attachments for item '{key}'. Try get_item and pick attachment_key."
                    ))
                })?;
            return self.get_pdf_text(&att.key).await;
        }
        if let Some((paper_id, fulltext)) = self.try_cached_fulltext_by_identity(resolved)? {
            resolved.paper_id = Some(paper_id);
            return Ok(fulltext);
        }

        // Agent path: await OA mirror when possible instead of racing background threads.
        if let Some(api) = &self.paperseed {
            let hit = paper_hit_from_resolved(resolved);
            if let Err(e) = mirror_open_access_hit(api, &hit, MirrorMode::Awaited).await {
                debug!("open_paper OA mirror failed: {e}");
            } else {
                if let Some((paper_id, fulltext)) =
                    self.try_cached_fulltext_by_identity(resolved)?
                {
                    resolved.paper_id = Some(paper_id);
                    return Ok(fulltext);
                }
            }
        }

        if let Some(fulltext) = self.download_open_fulltext(resolved).await? {
            return Ok(fulltext);
        }

        Err(ZoteroMcpError::InvalidInput(
            "No fulltext available yet. For external hits, ensure paperseed cache has the PDF (or use a Zotero attachment_key). Try: open_paper with paper_id after papers search, or library read for Zotero items.".into(),
        ))
    }

    async fn resolve_structure_for_open(
        &self,
        resolved: &mut OpenResolved,
        max_chars: usize,
    ) -> Result<PaperStructure> {
        if let Some(item_key) = resolved.item_key.as_deref() {
            return self
                .get_paper_structure(item_key, resolved.attachment_key.as_deref())
                .await;
        }
        if let Some(paper_id) = resolved.paper_id.as_deref()
            && let Some(structure) = self.try_cached_paper_structure(paper_id)?
        {
            return Ok(structure);
        }

        if let Some((paper_id, structure)) =
            self.try_cached_paper_structure_by_identity(resolved)?
        {
            resolved.paper_id = Some(paper_id);
            return Ok(structure);
        }

        let fulltext =
            truncate_fulltext(&self.resolve_fulltext_for_open(resolved).await?, max_chars);
        if let Some(paper_id) = resolved.paper_id.as_deref()
            && let Some(structure) = self.try_cached_paper_structure(paper_id)?
        {
            return Ok(structure);
        }

        let metadata = self.open_paper_metadata(resolved).await;
        Ok(PaperStructure {
            item_key: fulltext.item_key.clone(),
            attachment_key: None,
            metadata,
            sections: crate::paper::fallback::build_sections(None, &fulltext.content),
            references: Vec::new(),
            figures: Vec::new(),
            source: crate::models::PaperStructureSource::GrobidUnavailable {
                reason: "built from directly downloaded PDF text".into(),
            },
        })
    }

    async fn open_paper_metadata(&self, resolved: &OpenResolved) -> crate::models::PaperMetadata {
        if let Some(doi) = resolved.doi.as_deref()
            && let Ok(work) = self.resolve_doi(doi).await
        {
            return crate::models::PaperMetadata {
                title: work.title,
                authors: work.authors,
                abstract_note: work.abstract_note,
                doi: Some(work.doi),
                year: work.year,
            };
        }
        crate::models::PaperMetadata {
            title: None,
            authors: Vec::new(),
            abstract_note: None,
            doi: resolved.doi.clone(),
            year: None,
        }
    }

    async fn download_open_fulltext(
        &self,
        resolved: &OpenResolved,
    ) -> Result<Option<FulltextContent>> {
        let Some(url) = self.resolve_open_pdf_url(resolved).await else {
            return Ok(None);
        };
        let response = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?
            .get(&url)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("open_paper PDF download failed at {url}"),
            });
        }
        let bytes = response.bytes().await?;
        let content = paperseed::app::extract_pdf_text_from_bytes(&bytes).ok_or_else(|| {
            ZoteroMcpError::InvalidInput(format!(
                "Downloaded paper from {url}, but no extractable PDF text was found. Try a cached/OCR copy or a Zotero attachment."
            ))
        })?;
        let chars = u32::try_from(content.chars().count()).ok();
        let item_key = resolved
            .paper_id
            .clone()
            .or(resolved.arxiv_id.clone())
            .or(resolved.doi.clone())
            .or(resolved.url.clone())
            .unwrap_or_else(|| "open_paper".into());
        Ok(Some(FulltextContent {
            item_key,
            content,
            indexed_pages: None,
            total_pages: None,
            indexed_chars: chars,
            total_chars: chars,
        }))
    }

    async fn resolve_open_pdf_url(&self, resolved: &OpenResolved) -> Option<String> {
        if let Some(url) = resolved.url.clone() {
            return Some(url);
        }
        if let Some(arxiv) = resolved.arxiv_id.as_deref() {
            return Some(format!("https://arxiv.org/pdf/{arxiv}"));
        }
        let doi = resolved.doi.as_deref()?;
        if let Ok(work) = self.resolve_doi(doi).await
            && let Some(url) = work.oa_pdf_url
        {
            return Some(url);
        }
        paperseed::resolver::ResolverClient::new(None)
            .resolve_doi(doi, None)
            .await
            .ok()
            .and_then(|paper| paper.open_pdf_url)
    }

    fn mirror_open_access_hits(&self, hits: &[PaperHit]) {
        if !self.paperseed_auto_download {
            return;
        }
        let Some(api) = &self.paperseed else {
            return;
        };
        let api = api.clone();
        // Mirror hits that already carry an OA PDF url, plus hits that expose a
        // DOI (even without an OA url) so we can resolve one via Unpaywall/
        // OpenAlex — this captures metadata-only sources (Crossref, PubMed,
        // DBLP) whose hits would otherwise never enter the corpus.
        let hits: Vec<PaperHit> = hits
            .iter()
            .filter(|hit| hit.cache.is_none() && (hit.oa_pdf_url.is_some() || hit.doi.is_some()))
            .cloned()
            .collect();
        // Spawn per-paper download in detached OS threads so the main runtime can
        // exit immediately (critical for CLI UX). Each thread builds its own
        // one-shot runtime.
        for hit in hits {
            let api = api.clone();
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                rt.block_on(async {
                    if let Err(error) =
                        mirror_open_access_hit(&api, &hit, MirrorMode::Background).await
                    {
                        debug!("paperseed OA mirror skipped '{}': {}", hit.title, error);
                    }
                });
            });
        }
    }

    fn annotate_cached_hits(&self, hits: &mut [PaperHit]) {
        let Some(api) = &self.paperseed else {
            return;
        };
        for hit in hits {
            if let Some(entry) = api.find_cached_hit(hit) {
                hit.cache = Some(CachedPaperSummary {
                    paper_id: entry.paper.metadata.id,
                    cached: true,
                    has_full_text: entry.full_text.is_some(),
                });
            }
        }
    }

    pub fn backend_mode(&self) -> BackendMode {
        self.backend.mode()
    }

    pub fn backend_capabilities(&self) -> BackendCapabilities {
        self.backend.capabilities()
    }

    pub fn backend_info(&self) -> BackendInfo {
        let caps = self.backend.capabilities();
        BackendInfo {
            mode: match self.backend.mode() {
                BackendMode::Cloud => "cloud",
                BackendMode::Local => "local",
                BackendMode::Hybrid => "hybrid",
            }
            .to_string(),
            read_library: caps.read_library,
            write_basic: caps.write_basic,
            file_upload: caps.file_upload,
            group_libraries: caps.group_libraries,
        }
    }

    fn ensure_write_supported(&self, operation: &str) -> Result<()> {
        if self.backend.capabilities().write_basic {
            return Ok(());
        }

        let mode = match self.backend.mode() {
            BackendMode::Cloud => "cloud",
            BackendMode::Local => "local",
            BackendMode::Hybrid => "hybrid",
        };
        Err(ZoteroMcpError::InvalidInput(format!(
            "'{operation}' is not available for the active {mode} backend. Switch to a cloud Zotero Web API configuration to use write operations."
        )))
    }

    pub fn validate_collection_request(&self, req: &CollectionWriteRequest) -> ValidationReport {
        validation::validate_collection_request(req)
    }

    pub fn validate_item_request(&self, req: &ItemWriteRequest) -> ValidationReport {
        validation::validate_item_request(req)
    }

    pub fn validate_collection_update_request(
        &self,
        req: &CollectionUpdateRequest,
    ) -> ValidationReport {
        validation::validate_collection_update_request(req)
    }

    pub fn validate_item_update_request(&self, req: &ItemUpdateRequest) -> ValidationReport {
        validation::validate_item_update_request(req)
    }

    pub fn validate_delete_collection_request(
        &self,
        req: &DeleteCollectionRequest,
    ) -> ValidationReport {
        validation::validate_delete_collection_request(req)
    }

    pub fn validate_delete_item_request(&self, req: &DeleteItemRequest) -> ValidationReport {
        validation::validate_delete_item_request(req)
    }

    pub async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
        self.backend.search_items(query).await
    }

    /// Agent-facing library search with pagination envelope.
    pub async fn search_items_page(&self, query: SearchItemsQuery) -> Result<ItemListResult> {
        let q_echo = query.q.clone();
        let offset = query.start;
        let query = query.normalized();
        let limit = query.limit;
        let hits = self.backend.search_items(query).await?;
        // Zotero local/cloud may not expose total; use has_more heuristic.
        let page_len = hits.len() as u32;
        let has_more = page_len >= limit && limit > 0;
        let total_count = if has_more {
            offset.saturating_add(page_len).saturating_add(1)
        } else {
            offset.saturating_add(page_len)
        };
        Ok(ItemListResult {
            query: q_echo,
            total_count,
            offset,
            limit,
            has_more,
            next_offset: if has_more {
                Some(offset.saturating_add(limit))
            } else {
                None
            },
            hits,
        })
    }

    pub async fn list_collections(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<Vec<crate::models::CollectionSummary>> {
        self.backend.list_collections(query).await
    }

    pub async fn list_collections_page(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<CollectionListResult> {
        let offset = query.start;
        let query = query.normalized();
        let limit = query.limit;
        let hits = self.backend.list_collections(query).await?;
        let page_len = hits.len() as u32;
        let has_more = page_len >= limit && limit > 0;
        let total_count = if has_more {
            offset.saturating_add(page_len).saturating_add(1)
        } else {
            offset.saturating_add(page_len)
        };
        Ok(CollectionListResult {
            total_count,
            offset,
            limit,
            has_more,
            next_offset: if has_more {
                Some(offset.saturating_add(limit))
            } else {
                None
            },
            hits,
        })
    }

    pub async fn get_item(&self, key: &str) -> Result<ItemDetail> {
        self.backend.get_item(key).await
    }

    pub async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        if let Some(fulltext) = self.try_cached_fulltext(attachment_key)? {
            return Ok(fulltext);
        }
        match self.backend.get_pdf_text(attachment_key).await {
            Ok(fulltext) => Ok(fulltext),
            Err(backend_err) => {
                if let Some(fulltext) = self.try_cached_fulltext_by_query(attachment_key)? {
                    return Ok(fulltext);
                }
                Err(backend_err)
            }
        }
    }

    pub async fn get_item_fulltext(&self, attachment_key: &str) -> Result<FulltextContent> {
        match self.backend.get_item_fulltext(attachment_key).await {
            Ok(fulltext) => Ok(fulltext),
            Err(backend_err) => {
                if let Some(fulltext) = self.try_cached_fulltext_by_query(attachment_key)? {
                    return Ok(fulltext);
                }
                Err(backend_err)
            }
        }
    }

    pub async fn get_paper_structure(
        &self,
        item_key: &str,
        attachment_key: Option<&str>,
    ) -> Result<PaperStructure> {
        if attachment_key.is_none()
            && let Some(structure) = self.try_cached_paper_structure(item_key)?
        {
            return Ok(structure);
        }
        let item = self.backend.get_item(item_key).await?;
        let selected_attachment = pdf::select_attachment_for_reading(
            &item.attachments,
            attachment_key,
        )
        .ok_or_else(|| {
            ZoteroMcpError::InvalidInput(format!("no attachments available for item '{item_key}'"))
        })?;

        let cache_key: PaperCacheKey = (
            item_key.to_string(),
            selected_attachment.key.clone(),
            selected_attachment.version,
        );

        {
            let cache = self.paper_cache.lock().await;
            if let Some(cached) = cache.get(&cache_key) {
                debug!(item_key, attachment_key = %selected_attachment.key, "paper structure cache hit");
                return Ok(cached.clone());
            }
        }

        let structure = if let Some(cfg) = &self.paper_config {
            match self
                .try_grobid_structure(item_key, &selected_attachment.key, cfg)
                .await
            {
                Ok(s) => s,
                Err(err) => {
                    warn!(error = %err, "GROBID path failed, falling back to Zotero fulltext");
                    let fulltext = self
                        .backend
                        .get_item_fulltext(&selected_attachment.key)
                        .await?;
                    let mut s = paper::build_from_fulltext(&item, &fulltext);
                    s.source = crate::models::PaperStructureSource::GrobidUnavailable {
                        reason: err.to_string(),
                    };
                    s
                }
            }
        } else {
            let fulltext = self
                .backend
                .get_item_fulltext(&selected_attachment.key)
                .await?;
            paper::build_from_fulltext(&item, &fulltext)
        };

        let mut cache = self.paper_cache.lock().await;
        cache.insert(cache_key, structure.clone());
        Ok(structure)
    }

    async fn try_grobid_structure(
        &self,
        item_key: &str,
        attachment_key: &str,
        cfg: &PaperConfig,
    ) -> Result<PaperStructure> {
        let base_url = match &cfg.grobid_url {
            Some(url) if !url.trim().is_empty() => url.trim().to_string(),
            _ if cfg.grobid_auto_spawn => {
                paper::docker::ensure_grobid_ready(&cfg.grobid_image, GROBID_DEFAULT_PORT).await?
            }
            _ => {
                return Err(ZoteroMcpError::InvalidInput(
                    "GROBID not configured (grobid_url unset and grobid_auto_spawn=false)"
                        .to_string(),
                ));
            }
        };

        let client = GrobidClient::new(&base_url, cfg.grobid_timeout_secs)?;
        if !client.is_alive().await {
            return Err(ZoteroMcpError::Http(format!(
                "GROBID not responding at {base_url}/api/isalive"
            )));
        }

        debug!(
            item_key,
            attachment_key, "fetching attachment bytes for GROBID"
        );
        let bytes = self.backend.get_attachment_bytes(attachment_key).await?;
        let tei_xml = client.process_fulltext(bytes).await?;
        tei::parse_tei(item_key, attachment_key, &tei_xml)
    }

    pub async fn query_paper(
        &self,
        item_key: &str,
        selector: &str,
        attachment_key: Option<&str>,
    ) -> Result<serde_json::Value> {
        let structure = self.get_paper_structure(item_key, attachment_key).await?;
        paper::query(&structure, selector)
    }

    /// Build a deterministic SKILL.md scaffold from a paper's parsed structure.
    /// Accepts a Zotero item key or a cached Paperseed paper ID (same routing
    /// as `get_paper_structure`).
    pub async fn prepare_paper_for_skill(
        &self,
        item_key: &str,
        attachment_key: Option<&str>,
    ) -> Result<SkillPayload> {
        let structure = self.get_paper_structure(item_key, attachment_key).await?;
        Ok(crate::skill::build_skill_scaffold(&structure))
    }

    pub async fn create_collection(
        &self,
        req: CollectionWriteRequest,
    ) -> Result<CollectionSummary> {
        self.ensure_write_supported("create_collection")?;
        let report = self.validate_collection_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "collection validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.create_collection(req).await
    }

    pub async fn create_item(&self, req: ItemWriteRequest) -> Result<ItemDetail> {
        self.ensure_write_supported("create_item")?;
        let report = self.validate_item_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "item validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.create_item(req).await
    }

    pub async fn update_collection(
        &self,
        req: CollectionUpdateRequest,
    ) -> Result<CollectionSummary> {
        self.ensure_write_supported("update_collection")?;
        let report = self.validate_collection_update_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "collection update validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.update_collection(req).await
    }

    pub async fn update_item(&self, req: ItemUpdateRequest) -> Result<ItemDetail> {
        self.ensure_write_supported("update_item")?;
        let report = self.validate_item_update_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "item update validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.update_item(req).await
    }

    pub async fn delete_collection(&self, req: DeleteCollectionRequest) -> Result<()> {
        self.ensure_write_supported("delete_collection")?;
        let report = self.validate_delete_collection_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "delete collection validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.delete_collection(req).await
    }

    pub async fn delete_item(&self, req: DeleteItemRequest) -> Result<()> {
        self.ensure_write_supported("delete_item")?;
        let report = self.validate_delete_item_request(&req);
        if !report.valid {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "delete item validation failed: {}",
                summarize_validation_report(&report)
            )));
        }
        self.backend.delete_item(req).await
    }

    pub async fn resolve_doi(&self, doi: &str) -> Result<CrossrefWork> {
        let mut work = self.crossref.resolve_doi(doi).await?;
        if let Some(unpaywall) = self.unpaywall.as_ref() {
            match unpaywall.lookup(&work.doi).await {
                Ok(pdf) => work.oa_pdf_url = pdf,
                Err(e) => {
                    tracing::warn!(error = %e, doi = %work.doi, "unpaywall enrichment failed");
                }
            }
        }
        Ok(work)
    }

    pub async fn validate_item_online(&self, req: &ItemWriteRequest) -> Result<ValidationReport> {
        let mut report = validation::validate_item_request(req);

        let doi = match req.doi.as_deref() {
            Some(d) if !d.trim().is_empty() && validation::looks_like_doi(d) => d.trim(),
            _ => return Ok(report),
        };

        let work = match self.crossref.resolve_doi(doi).await {
            Ok(w) => w,
            Err(ZoteroMcpError::Api { status: 404, .. }) => {
                report.issues.push(ValidationIssue {
                    level: ValidationIssueLevel::Warning,
                    field: "doi".to_string(),
                    message: format!("DOI '{doi}' not found in Crossref"),
                });
                return Ok(report);
            }
            Err(e) => {
                report.issues.push(ValidationIssue {
                    level: ValidationIssueLevel::Warning,
                    field: "doi".to_string(),
                    message: format!("Crossref lookup failed: {e}"),
                });
                return Ok(report);
            }
        };

        if let Some(req_title) = req.title.as_deref()
            && let Some(cr_title) = work.title.as_deref()
            && !titles_match(req_title, cr_title)
        {
            report.issues.push(ValidationIssue {
                level: ValidationIssueLevel::Warning,
                field: "title".to_string(),
                message: format!(
                    "Title mismatch: provided '{}', Crossref has '{}'",
                    req_title, cr_title
                ),
            });
        }

        if let Some(req_date) = req.date.as_deref()
            && let Some(cr_year) = work.year.as_deref()
            && !req_date.contains(cr_year)
        {
            report.issues.push(ValidationIssue {
                level: ValidationIssueLevel::Warning,
                field: "date".to_string(),
                message: format!(
                    "Year mismatch: provided date '{}', Crossref year is '{}'",
                    req_date, cr_year
                ),
            });
        }

        Ok(report)
    }

    pub async fn prepare_vox_text(&self, req: PrepareVoxTextRequest) -> Result<VoxTextPayload> {
        let max_chars = req.max_chars_per_chunk.unwrap_or(DEFAULT_CHUNK_SIZE);

        if let Some(text) = req.text {
            let source = req
                .source_label
                .unwrap_or_else(|| "manual-text".to_string());
            return Ok(pdf::prepare_vox_payload(&source, &text, max_chars));
        }

        if let Some(attachment_key) = req.attachment_key {
            if let Some(fulltext) = self.try_cached_fulltext(&attachment_key)? {
                let source = req
                    .source_label
                    .unwrap_or_else(|| format!("paperseed:{attachment_key}"));
                return Ok(pdf::prepare_vox_payload_from_fulltext(
                    &source, &fulltext, max_chars,
                ));
            }
            match self.backend.get_pdf_text(&attachment_key).await {
                Ok(fulltext) => {
                    let source = req
                        .source_label
                        .unwrap_or_else(|| format!("attachment:{attachment_key}"));
                    return Ok(pdf::prepare_vox_payload_from_fulltext(
                        &source, &fulltext, max_chars,
                    ));
                }
                Err(backend_err) => {
                    if let Some(fulltext) = self.try_cached_fulltext_by_query(&attachment_key)? {
                        let source = req
                            .source_label
                            .unwrap_or_else(|| format!("paperseed:{attachment_key}"));
                        return Ok(pdf::prepare_vox_payload_from_fulltext(
                            &source, &fulltext, max_chars,
                        ));
                    }
                    return Err(backend_err);
                }
            }
        }

        Err(ZoteroMcpError::InvalidInput(
            "Provide either 'text' or 'attachment_key'".to_string(),
        ))
    }

    pub async fn prepare_item_for_vox(
        &self,
        req: PrepareItemForVoxRequest,
    ) -> Result<ItemVoxPayload> {
        if req.attachment_key.is_none()
            && let Some(payload) =
                self.try_prepare_cached_item_for_vox(&req.item_key, req.max_chars_per_chunk)?
        {
            return Ok(payload);
        }
        let max_chars = req.max_chars_per_chunk.unwrap_or(DEFAULT_CHUNK_SIZE);
        let item = self.backend.get_item(&req.item_key).await?;

        let attachment =
            pdf::select_attachment_for_reading(&item.attachments, req.attachment_key.as_deref())
                .ok_or_else(|| {
                    ZoteroMcpError::InvalidInput(format!(
                        "No attachments available for item '{}'.",
                        item.key
                    ))
                })?;

        if let Some(expected) = req.attachment_key.as_deref()
            && attachment.key != expected
        {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "Attachment '{}' was not found for item '{}'.",
                expected, item.key
            )));
        }

        let fulltext = self.backend.get_pdf_text(&attachment.key).await?;
        Ok(pdf::build_item_vox_payload(
            &item.key,
            &item.title,
            attachment,
            &fulltext,
            max_chars,
        ))
    }

    pub async fn prepare_search_result_for_vox(
        &self,
        req: PrepareSearchResultForVoxRequest,
    ) -> Result<SearchVoxPayload> {
        let query = req.q.trim().to_string();
        if query.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "Parameter 'q' cannot be empty".to_string(),
            ));
        }

        let paper_hits = self
            .search_papers(SearchOptions {
                query: query.clone(),
                limit_per_source: req.search_limit.unwrap_or(DEFAULT_PIPELINE_SEARCH_LIMIT),
                sources: None,
                timeout_ms: 8_000,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await?
            .hits;
        let result_index = req.result_index.unwrap_or(0);
        if let Some(selected) = paper_hits.get(result_index)
            && let Some(cache) = &selected.cache
        {
            let prepared = self
                .try_prepare_cached_item_for_vox(&cache.paper_id, req.max_chars_per_chunk)?
                .ok_or_else(|| {
                    ZoteroMcpError::InvalidInput(format!(
                        "Cached paper '{}' is not readable yet",
                        cache.paper_id
                    ))
                })?;
            return Ok(SearchVoxPayload {
                query,
                result_index,
                result_count: paper_hits.len(),
                selected_item: ItemSummary {
                    key: cache.paper_id.clone(),
                    item_type: "cached_paper".to_string(),
                    title: selected.title.clone(),
                    creators: selected.authors.clone(),
                    year: selected.year.clone(),
                    url: selected.url.clone().or_else(|| selected.oa_pdf_url.clone()),
                },
                prepared,
            });
        }

        let results = self
            .search_items(SearchItemsQuery {
                q: Some(query.clone()),
                qmode: req.qmode,
                item_type: req.item_type,
                tag: req.tag,
                limit: req.search_limit.unwrap_or(DEFAULT_PIPELINE_SEARCH_LIMIT),
                start: 0,
            })
            .await?;
        if results.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "No search results found for query '{}'.",
                query
            )));
        }

        let selected = results.get(result_index).cloned().ok_or_else(|| {
            ZoteroMcpError::InvalidInput(format!(
                "result_index {} is out of range for {} results",
                result_index,
                results.len()
            ))
        })?;

        let prepared = self
            .prepare_item_for_vox(PrepareItemForVoxRequest {
                item_key: selected.key.clone(),
                attachment_key: None,
                max_chars_per_chunk: req.max_chars_per_chunk,
            })
            .await?;

        Ok(SearchVoxPayload {
            query,
            result_index,
            result_count: results.len(),
            selected_item: selected,
            prepared,
        })
    }

    fn try_cached_fulltext(&self, paper_id: &str) -> Result<Option<FulltextContent>> {
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        match api.get_cached_paper_fulltext(paper_id) {
            Ok(fulltext) => Ok(Some(fulltext)),
            Err(ZoteroMcpError::InvalidInput(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn try_cached_fulltext_by_identity(
        &self,
        resolved: &OpenResolved,
    ) -> Result<Option<(String, FulltextContent)>> {
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        let Some(entry) = api.find_cached_identity(
            resolved.doi.as_deref(),
            resolved.arxiv_id.as_deref(),
            resolved.url.as_deref(),
        ) else {
            return Ok(None);
        };
        let paper_id = entry.paper.metadata.id;
        Ok(self
            .try_cached_fulltext(&paper_id)?
            .map(|fulltext| (paper_id, fulltext)))
    }

    /// Compatibility fallback for legacy read commands that explicitly treat
    /// their key argument as a natural-language cache query. `open_paper`
    /// intentionally does not use this path for identifiers.
    fn try_cached_fulltext_by_query(&self, query: &str) -> Result<Option<FulltextContent>> {
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        let hits = match api.search_cached_papers(query, 1) {
            Ok(hits) => hits,
            Err(_) => return Ok(None),
        };
        if let Some(hit) = hits.first()
            && let Some(cache) = &hit.cache
        {
            return self.try_cached_fulltext(&cache.paper_id);
        }
        Ok(None)
    }

    fn try_cached_paper_structure(&self, paper_id: &str) -> Result<Option<PaperStructure>> {
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        let paper = match api.get_cached_paper(paper_id) {
            Ok(paper) => paper,
            Err(ZoteroMcpError::InvalidInput(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let fulltext = match api.get_cached_paper_fulltext(paper_id) {
            Ok(fulltext) => fulltext,
            Err(ZoteroMcpError::InvalidInput(_)) => FulltextContent {
                item_key: paper_id.to_string(),
                content: String::new(),
                indexed_pages: None,
                total_pages: None,
                indexed_chars: None,
                total_chars: None,
            },
            Err(error) => return Err(error),
        };
        Ok(Some(cached_paper_structure(&paper, &fulltext)))
    }

    fn try_cached_paper_structure_by_identity(
        &self,
        resolved: &OpenResolved,
    ) -> Result<Option<(String, PaperStructure)>> {
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        let Some(entry) = api.find_cached_identity(
            resolved.doi.as_deref(),
            resolved.arxiv_id.as_deref(),
            resolved.url.as_deref(),
        ) else {
            return Ok(None);
        };
        let paper_id = entry.paper.metadata.id;
        Ok(self
            .try_cached_paper_structure(&paper_id)?
            .map(|structure| (paper_id, structure)))
    }

    fn try_prepare_cached_item_for_vox(
        &self,
        item_key: &str,
        max_chars_per_chunk: Option<usize>,
    ) -> Result<Option<ItemVoxPayload>> {
        let Some(fulltext) = self.try_cached_fulltext(item_key)? else {
            return Ok(None);
        };
        let Some(api) = &self.paperseed else {
            return Ok(None);
        };
        let paper = match api.get_cached_paper(item_key) {
            Ok(paper) => paper,
            Err(ZoteroMcpError::InvalidInput(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(ItemVoxPayload {
            item_key: paper.paper_id.clone(),
            item_title: paper.title.clone(),
            attachment: crate::models::AttachmentSummary {
                key: paper.paper_id.clone(),
                title: paper.title.clone(),
                content_type: Some(paper.mime.clone()),
                path: Some(paper.stored_path.clone()),
                version: None,
            },
            indexed_pages: fulltext.indexed_pages,
            total_pages: fulltext.total_pages,
            indexed_chars: fulltext.indexed_chars,
            total_chars: fulltext.total_chars,
            vox: pdf::prepare_vox_payload_from_fulltext(
                &format!("paperseed:{}", paper.paper_id),
                &fulltext,
                max_chars_per_chunk.unwrap_or(DEFAULT_CHUNK_SIZE),
            ),
        }))
    }
}

fn effective_cache_mode(
    requested: SearchCacheMode,
    sources: Option<&[PaperSource]>,
) -> SearchCacheMode {
    if requested != SearchCacheMode::Auto {
        return requested;
    }
    let Some(sources) = sources else {
        return SearchCacheMode::Auto;
    };
    let includes_cache = sources.contains(&PaperSource::Paperseed);
    if !includes_cache {
        return SearchCacheMode::Off;
    }
    if sources.len() == 1 {
        SearchCacheMode::Only
    } else {
        SearchCacheMode::Include
    }
}

/// Merge cache hits into the externals list. Cache hits that collide with an
/// external on DOI / arXiv / PMID / (title + first-author) replace the external
/// entry in place — preserving the external's list position so cache content
/// doesn't get force-promoted to the top. Cache hits with no collision stay at
/// the tail in their original order.
#[derive(Debug, Clone, Default)]
struct OpenResolved {
    doi: Option<String>,
    arxiv_id: Option<String>,
    item_key: Option<String>,
    paper_id: Option<String>,
    attachment_key: Option<String>,
    url: Option<String>,
}

fn resolve_open_targets(req: &OpenPaperRequest) -> Result<OpenResolved> {
    let mut r = OpenResolved {
        doi: req.doi.as_ref().and_then(|d| normalize_doi(d)),
        arxiv_id: req.arxiv_id.as_ref().and_then(|a| normalize_arxiv_id(a)),
        item_key: req.item_key.clone(),
        paper_id: req.paper_id.clone(),
        attachment_key: req.attachment_key.clone(),
        url: req.url.as_deref().map(normalize_open_url).transpose()?,
    };

    if let Some(hit_id) = req.hit_id.as_deref() {
        if let Some(rest) = hit_id.strip_prefix("arxiv:") {
            r.arxiv_id = Some(strip_arxiv_version_local(rest));
        } else if let Some(rest) = hit_id.strip_prefix("doi:") {
            r.doi = normalize_doi(rest);
        } else if let Some(rest) = hit_id.strip_prefix("pmid:") {
            // PMID-only open is limited; stash as paper query key via paper_id-like
            r.paper_id = r.paper_id.or_else(|| Some(rest.to_string()));
        } else if let Some(rest) = hit_id.strip_prefix("paperseed:") {
            r.paper_id = Some(rest.to_string());
        } else if let Some(rest) = hit_id.strip_prefix("zotero:") {
            r.item_key = Some(rest.to_string());
        } else if let Some(rest) = hit_id.strip_prefix("url:") {
            r.url = Some(normalize_open_url(rest)?);
        } else if hit_id.contains('/') {
            // bare DOI in hit_id
            r.doi = r.doi.or_else(|| normalize_doi(hit_id));
        }
    }

    if r.doi.is_none()
        && r.arxiv_id.is_none()
        && r.item_key.is_none()
        && r.paper_id.is_none()
        && r.attachment_key.is_none()
        && r.url.is_none()
    {
        return Err(ZoteroMcpError::InvalidInput(
            "open_paper requires hit_id, doi, arxiv_id, item_key, paper_id, attachment_key, or url."
                .into(),
        ));
    }
    Ok(r)
}

fn normalize_open_url(raw: &str) -> Result<String> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| {
        ZoteroMcpError::InvalidInput(format!(
            "open_paper URL must be an absolute HTTP(S) URL, got '{raw}'"
        ))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ZoteroMcpError::InvalidInput(format!(
            "open_paper URL must use HTTP(S), got '{}': {raw}",
            parsed.scheme()
        )));
    }
    Ok(parsed.to_string())
}

fn strip_arxiv_version_local(id: &str) -> String {
    if let Some(idx) = id.rfind('v') {
        let (base, ver) = id.split_at(idx);
        if ver.len() > 1 && ver[1..].chars().all(|c| c.is_ascii_digit()) {
            return base.to_string();
        }
    }
    id.to_string()
}

fn paper_hit_from_resolved(resolved: &OpenResolved) -> PaperHit {
    let arxiv = resolved.arxiv_id.clone();
    let doi = resolved.doi.clone();
    let (url, pdf_url, oa_pdf_url) = if let Some(ref a) = arxiv {
        (
            Some(format!("https://arxiv.org/abs/{a}")),
            Some(format!("https://arxiv.org/pdf/{a}")),
            Some(format!("https://arxiv.org/pdf/{a}")),
        )
    } else if let Some(ref d) = doi {
        (Some(format!("https://doi.org/{d}")), None, None)
    } else if let Some(url) = resolved.url.clone() {
        (Some(url.clone()), Some(url.clone()), Some(url))
    } else {
        (None, None, None)
    };
    let title = arxiv
        .clone()
        .or_else(|| doi.clone())
        .or_else(|| resolved.url.clone())
        .unwrap_or_else(|| "open_paper".into());
    let mut hit = PaperHit::new(
        if arxiv.is_some() {
            PaperSource::Arxiv
        } else {
            PaperSource::Crossref
        },
        title,
        Vec::new(),
        None,
        doi,
        arxiv,
        None,
        None,
        url,
        pdf_url,
        oa_pdf_url,
        None,
        None,
    );
    if let Some(pid) = resolved.paper_id.clone() {
        hit.cache = Some(CachedPaperSummary {
            paper_id: pid,
            cached: true,
            has_full_text: false,
        });
    }
    hit
}

fn truncate_fulltext(fulltext: &FulltextContent, max_chars: usize) -> FulltextContent {
    let total_chars = fulltext.content.chars().count() as u32;
    if fulltext.content.chars().count() <= max_chars {
        let mut ft = fulltext.clone();
        ft.total_chars = Some(total_chars);
        ft.indexed_chars = Some(total_chars);
        return ft;
    }
    let content: String = fulltext.content.chars().take(max_chars).collect();
    FulltextContent {
        item_key: fulltext.item_key.clone(),
        content,
        indexed_pages: fulltext.indexed_pages,
        total_pages: fulltext.total_pages,
        indexed_chars: Some(max_chars as u32),
        total_chars: Some(total_chars),
    }
}

fn merge_prefer_cached(hits: &mut Vec<PaperHit>, cache_start: usize) {
    if cache_start >= hits.len() {
        return;
    }
    let cached: Vec<PaperHit> = hits.drain(cache_start..).collect();
    for cache_hit in cached {
        match collision_index(hits, &cache_hit) {
            Some(idx) => hits[idx] = cache_hit,
            None => hits.push(cache_hit),
        }
    }
}

fn collision_index(externals: &[PaperHit], cache_hit: &PaperHit) -> Option<usize> {
    use crate::external::{arxiv_key, doi_key, pmid_key, title_author_key};
    let cache_doi = doi_key(cache_hit);
    let cache_arxiv = arxiv_key(cache_hit);
    let cache_pmid = pmid_key(cache_hit);
    let cache_title = title_author_key(cache_hit);

    externals.iter().position(|ext| {
        (cache_doi.is_some() && cache_doi == doi_key(ext))
            || (cache_arxiv.is_some() && cache_arxiv == arxiv_key(ext))
            || (cache_pmid.is_some() && cache_pmid == pmid_key(ext))
            || (cache_title.is_some() && cache_title == title_author_key(ext))
    })
}

fn cached_paper_structure(paper: &CachedPaperDetail, fulltext: &FulltextContent) -> PaperStructure {
    let sections = crate::paper::fallback::build_sections(None, &fulltext.content);
    PaperStructure {
        item_key: paper.paper_id.clone(),
        attachment_key: Some(paper.paper_id.clone()),
        metadata: crate::models::PaperMetadata {
            title: Some(paper.title.clone()),
            authors: paper.authors.clone(),
            abstract_note: None,
            doi: paper.doi.clone(),
            year: paper.year.clone(),
        },
        sections,
        references: Vec::new(),
        figures: Vec::new(),
        source: crate::models::PaperStructureSource::ZoteroFulltext,
    }
}

fn titles_match(a: &str, b: &str) -> bool {
    normalize_search_text(a) == normalize_search_text(b)
}

fn should_surface_cached_hit(query: &str, hit: &PaperHit) -> bool {
    if query_id_matches_hit(query, hit) {
        return true;
    }

    let terms = meaningful_query_terms(query);
    if terms.is_empty() {
        return false;
    }

    let title = normalize_search_text(&hit.title);
    let evidence = normalize_search_text(&cache_evidence_text(hit));
    let phrase = normalize_search_text(query);
    if terms.len() >= 2 && !phrase.is_empty() && evidence.contains(&phrase) {
        return true;
    }

    let title_matches = terms
        .iter()
        .filter(|term| contains_normalized_token(&title, term))
        .count();
    let evidence_matches = terms
        .iter()
        .filter(|term| contains_normalized_token(&evidence, term))
        .count();

    if terms.len() == 1 {
        return title_matches == 1 || (evidence_matches == 1 && cache_score(hit) >= 6_000);
    }

    evidence_matches >= 2 || (title_matches >= 1 && evidence_matches >= 1)
}

fn query_id_matches_hit(query: &str, hit: &PaperHit) -> bool {
    if let Some(query_doi) = normalize_doi(query)
        && hit.doi.as_deref().and_then(normalize_doi).as_deref() == Some(query_doi.as_str())
    {
        return true;
    }
    if let Some(query_arxiv) = normalize_arxiv_id(query) {
        let hit_arxiv = hit
            .arxiv_id
            .as_deref()
            .and_then(normalize_arxiv_id)
            .or_else(|| hit.url.as_deref().and_then(normalize_arxiv_id))
            .or_else(|| hit.oa_pdf_url.as_deref().and_then(normalize_arxiv_id));
        if hit_arxiv.as_deref() == Some(query_arxiv.as_str()) {
            return true;
        }
    }
    false
}

fn cache_evidence_text(hit: &PaperHit) -> String {
    let mut parts = Vec::new();
    parts.push(hit.title.as_str());
    parts.extend(hit.authors.iter().map(String::as_str));
    if let Some(year) = hit.year.as_deref() {
        parts.push(year);
    }
    if let Some(doi) = hit.doi.as_deref() {
        parts.push(doi);
    }
    if let Some(abstract_note) = hit.abstract_note.as_deref() {
        parts.push(abstract_note);
    }
    if let Some(venue) = hit.venue.as_deref() {
        parts.push(venue);
    }
    if let Some(url) = hit.url.as_deref() {
        parts.push(url);
    }
    parts.join(" ")
}

fn meaningful_query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for term in normalize_search_text(query).split_whitespace() {
        if term.len() < 3 || is_weak_cache_query_term(term) {
            continue;
        }
        if !terms.iter().any(|existing| existing == term) {
            terms.push(term.to_string());
        }
    }
    terms
}

fn is_weak_cache_query_term(term: &str) -> bool {
    matches!(
        term,
        "about"
            | "above"
            | "after"
            | "again"
            | "against"
            | "also"
            | "and"
            | "any"
            | "are"
            | "because"
            | "been"
            | "before"
            | "being"
            | "between"
            | "both"
            | "but"
            | "can"
            | "could"
            | "did"
            | "does"
            | "doing"
            | "for"
            | "from"
            | "had"
            | "has"
            | "have"
            | "her"
            | "here"
            | "hers"
            | "him"
            | "his"
            | "how"
            | "into"
            | "its"
            | "itself"
            | "just"
            | "literature"
            | "more"
            | "most"
            | "new"
            | "nor"
            | "not"
            | "now"
            | "off"
            | "only"
            | "our"
            | "ours"
            | "out"
            | "over"
            | "own"
            | "paper"
            | "papers"
            | "research"
            | "same"
            | "she"
            | "should"
            | "show"
            | "some"
            | "such"
            | "than"
            | "that"
            | "the"
            | "their"
            | "them"
            | "then"
            | "there"
            | "these"
            | "they"
            | "this"
            | "those"
            | "through"
            | "topic"
            | "under"
            | "until"
            | "use"
            | "used"
            | "using"
            | "very"
            | "was"
            | "were"
            | "what"
            | "when"
            | "where"
            | "which"
            | "while"
            | "who"
            | "why"
            | "with"
            | "would"
            | "your"
    )
}

fn contains_normalized_token(text: &str, token: &str) -> bool {
    text.split_whitespace().any(|part| part == token)
}

fn cache_score(hit: &PaperHit) -> u32 {
    hit.relevance_score
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1000.0).round().clamp(0.0, u32::MAX as f32) as u32)
        .unwrap_or(0)
}

fn rank_search_hits(query: &str, hits: &mut [PaperHit]) {
    use std::cmp::Reverse;

    let query_title = normalize_search_text(query);
    let query_tokens: Vec<String> = query_title.split_whitespace().map(str::to_string).collect();
    let query_doi = normalize_doi(query);
    let query_arxiv = normalize_arxiv_id(query);

    // Nothing to rank against: query was empty, punctuation-only, or
    // non-Latin enough that normalize_search_text dropped everything and
    // neither DOI nor arXiv shape was detected. Leave merge order intact.
    if query_title.is_empty() && query_doi.is_none() && query_arxiv.is_none() {
        return;
    }

    hits.sort_by_cached_key(|hit| {
        Reverse(search_rank(
            &query_title,
            &query_tokens,
            query_doi.as_deref(),
            query_arxiv.as_deref(),
            hit,
        ))
    });
}

fn search_rank(
    query_title: &str,
    query_tokens: &[String],
    query_doi: Option<&str>,
    query_arxiv: Option<&str>,
    hit: &PaperHit,
) -> (u8, u8, u8, usize, u16, u32, u32, u8) {
    let normalized_title = normalize_search_text(&hit.title);
    let title_tokens: Vec<&str> = normalized_title.split_whitespace().collect();

    let doi_match = query_doi
        .zip(hit.doi.as_deref().and_then(normalize_doi))
        .map(|(query, doi)| query == doi)
        .unwrap_or(false);
    let arxiv_match = query_arxiv
        .zip(hit.arxiv_id.as_deref().and_then(normalize_arxiv_id))
        .map(|(query, arxiv)| query == arxiv)
        .unwrap_or(false);

    let exact_title = !query_title.is_empty() && query_title == normalized_title;
    let exact_phrase =
        !exact_title && !query_title.is_empty() && normalized_title.contains(query_title);
    let token_matches = query_tokens
        .iter()
        .filter(|token| {
            title_tokens
                .iter()
                .any(|title_token| title_token == &token.as_str())
        })
        .count();
    let all_tokens_present = !query_tokens.is_empty() && token_matches == query_tokens.len();

    let title_strength = if exact_title {
        4
    } else if exact_phrase {
        3
    } else if all_tokens_present {
        2
    } else if token_matches > 0 {
        1
    } else {
        0
    };

    let extra_tokens = title_tokens.len().saturating_sub(query_tokens.len());
    let tightness = if title_strength >= 3 {
        u16::MAX.saturating_sub(extra_tokens.min(u16::MAX as usize) as u16)
    } else {
        0
    };

    // Cache hits carry a BM25F score from paperseed-index; surface it as an
    // advisory tiebreaker. Scale + round so the f32 score participates in the
    // Ord tuple. External hits (no cache score) contribute 0 here, which is
    // intentional — citation_count and source_bias still decide their order.
    let relevance_score = hit
        .relevance_score
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1000.0).round().clamp(0.0, u32::MAX as f32) as u32)
        .unwrap_or(0);

    (
        doi_match as u8,
        arxiv_match as u8,
        title_strength,
        token_matches,
        tightness,
        hit.citation_count.unwrap_or(0),
        relevance_score,
        source_rank_bias(hit.source),
    )
}

fn normalize_search_text(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_doi(raw: &str) -> Option<String> {
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

    if validation::looks_like_doi(normalized) {
        Some(normalized.to_string())
    } else {
        None
    }
}

fn normalize_arxiv_id(raw: &str) -> Option<String> {
    let lowered = raw.trim().to_lowercase();
    if lowered.is_empty() {
        return None;
    }

    let id = lowered
        .strip_prefix("https://arxiv.org/abs/")
        .or_else(|| lowered.strip_prefix("http://arxiv.org/abs/"))
        .or_else(|| lowered.strip_prefix("arxiv:"))
        .unwrap_or(lowered.as_str())
        .trim();

    if id.is_empty() {
        return None;
    }

    let normalized = if let Some((base, version)) = id.rsplit_once('v')
        && !base.is_empty()
        && !version.is_empty()
        && version.chars().all(|c| c.is_ascii_digit())
    {
        base.to_string()
    } else {
        id.to_string()
    };

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn source_rank_bias(source: crate::models::PaperSource) -> u8 {
    match source {
        crate::models::PaperSource::Arxiv => 11,
        crate::models::PaperSource::SemanticScholar => 10,
        crate::models::PaperSource::OpenAlex => 9,
        crate::models::PaperSource::Crossref => 8,
        crate::models::PaperSource::OpenReview => 7,
        crate::models::PaperSource::Dblp => 6,
        crate::models::PaperSource::Pubmed => 5,
        crate::models::PaperSource::EuropePmc => 4,
        crate::models::PaperSource::Core => 3,
        crate::models::PaperSource::HuggingFace => 2,
        crate::models::PaperSource::Ads => 1,
        crate::models::PaperSource::Paperseed => 1,
        crate::models::PaperSource::ScholarApi => 0,
    }
}

fn summarize_validation_report(report: &ValidationReport) -> String {
    report
        .issues
        .iter()
        .map(|issue| format!("{}: {}", issue.field, issue.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum MirrorMode {
    Background,
    Awaited,
}

async fn mirror_open_access_hit(
    api: &PaperseedApi,
    hit: &PaperHit,
    mode: MirrorMode,
) -> Result<()> {
    // Prefer the hit's own OA url; otherwise resolve one from its DOI.
    let url = match hit.oa_pdf_url.clone() {
        Some(url) => url,
        None => match resolve_oa_pdf_url(api, hit).await {
            Some(url) => url,
            None => return Ok(()),
        },
    };
    let url = url.as_str();
    let incoming = api.paths().root.join("incoming");
    std::fs::create_dir_all(&incoming).map_err(|e| {
        ZoteroMcpError::Config(format!("Failed to create Paperseed incoming dir: {e}"))
    })?;
    let file = incoming.join(format!("{}.pdf", paperseed_safe_name(hit)));
    if mode == MirrorMode::Background {
        let yams_downloaded = api.download_with_yams_queue(
            url,
            Some(&hit.title),
            hit.doi.as_deref(),
            hit.url.as_deref().or(Some(url)),
        );
        // Background mirroring may hand work to YAMS. The awaited agent path
        // deliberately bypasses the queue and ingests synchronously below.
        if yams_downloaded.is_some() {
            return Ok(());
        }
    }

    // Manual path: download then index into local corpus.
    let bytes = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?
        .get(url)
        .send()
        .await?
        .bytes()
        .await?;
    std::fs::write(&file, bytes).map_err(|e| {
        ZoteroMcpError::Config(format!("Failed to write Paperseed OA cache file: {e}"))
    })?;
    api.ingest_with_metadata(
        &file,
        paperseed::sources::PaperbridgeMetadata {
            title: Some(hit.title.clone()),
            doi: hit.doi.clone(),
            authors: hit.authors.clone(),
            year: hit.year.as_deref().and_then(parse_year),
            venue: hit.venue.clone(),
            license: Some("unknown".to_string()),
            source_url: hit.url.clone().or_else(|| Some(url.to_string())),
        },
        Some("unknown".to_string()),
    )?;
    Ok(())
}

/// Resolve a hit's DOI to an open-access PDF url via Unpaywall → OpenAlex.
/// Returns `None` when the hit has no DOI or no open PDF could be found.
async fn resolve_oa_pdf_url(api: &PaperseedApi, hit: &PaperHit) -> Option<String> {
    let doi = hit.doi.as_deref()?;
    match api.resolve_open_doi(doi, None).await {
        Ok(resolved) => resolved.open_pdf_url,
        Err(error) => {
            debug!("paperseed OA resolve skipped '{}': {}", hit.title, error);
            None
        }
    }
}

fn paperseed_safe_name(hit: &PaperHit) -> String {
    let raw = hit
        .doi
        .as_deref()
        .or(hit.arxiv_id.as_deref())
        .unwrap_or(&hit.title);
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn parse_year(raw: &str) -> Option<u16> {
    raw.split(|c: char| !c.is_ascii_digit())
        .find(|part| part.len() == 4)
        .and_then(|year| year.parse::<u16>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
    use crate::models::{ListCollectionsQuery, PaperSource, SearchItemsQuery};

    struct StubLocalReadOnlyBackend;

    #[async_trait::async_trait]
    impl LibraryBackend for StubLocalReadOnlyBackend {
        fn mode(&self) -> BackendMode {
            BackendMode::Local
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::read_only_local()
        }

        async fn search_items(&self, _query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
            Ok(vec![])
        }

        async fn list_collections(
            &self,
            _query: ListCollectionsQuery,
        ) -> Result<Vec<CollectionSummary>> {
            Ok(vec![])
        }

        async fn get_item(&self, _key: &str) -> Result<ItemDetail> {
            Err(ZoteroMcpError::InvalidInput("unused".to_string()))
        }

        async fn get_item_fulltext(&self, _key: &str) -> Result<FulltextContent> {
            Err(ZoteroMcpError::InvalidInput("unused".to_string()))
        }

        async fn get_pdf_text(&self, _attachment_key: &str) -> Result<FulltextContent> {
            Err(ZoteroMcpError::InvalidInput("unused".to_string()))
        }

        async fn get_attachment_bytes(&self, _attachment_key: &str) -> Result<Vec<u8>> {
            Err(ZoteroMcpError::InvalidInput("unused".to_string()))
        }

        async fn create_collection(
            &self,
            _req: CollectionWriteRequest,
        ) -> Result<CollectionSummary> {
            panic!("backend create_collection should not be reached when write is unsupported")
        }

        async fn update_collection(
            &self,
            _req: CollectionUpdateRequest,
        ) -> Result<CollectionSummary> {
            panic!("backend update_collection should not be reached when write is unsupported")
        }

        async fn delete_collection(&self, _req: DeleteCollectionRequest) -> Result<()> {
            panic!("backend delete_collection should not be reached when write is unsupported")
        }

        async fn create_item(&self, _req: ItemWriteRequest) -> Result<ItemDetail> {
            panic!("backend create_item should not be reached when write is unsupported")
        }

        async fn update_item(&self, _req: ItemUpdateRequest) -> Result<ItemDetail> {
            panic!("backend update_item should not be reached when write is unsupported")
        }

        async fn delete_item(&self, _req: DeleteItemRequest) -> Result<()> {
            panic!("backend delete_item should not be reached when write is unsupported")
        }
    }

    #[tokio::test]
    async fn mirror_open_access_hit_downloads_into_paperseed_corpus() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/paper.pdf"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes("pdf bytes"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::new(dir.path().join("corpus"), None);
        let hit = PaperHit {
            hit_id: None,
            source: PaperSource::OpenAlex,
            title: "Open Paper".to_string(),
            authors: vec!["Ada Lovelace".to_string()],
            year: Some("2024".to_string()),
            doi: Some("10.5555/open".to_string()),
            arxiv_id: None,
            pmid: None,
            abstract_note: None,
            url: Some("https://example.org/open".to_string()),
            pdf_url: None,
            oa_pdf_url: Some(format!("{}/paper.pdf", server.uri())),
            venue: Some("Open Venue".to_string()),
            citation_count: None,
            cache: None,
            relevance_score: None,
            ids: None,
            match_info: None,
            access: None,
            next: Vec::new(),
        };

        mirror_open_access_hit(&api, &hit, MirrorMode::Awaited)
            .await
            .unwrap();
        let db = api.corpus_status().unwrap();
        assert_eq!(db.papers.len(), 1);
        assert_eq!(db.papers[0].paper.metadata.title, "Open Paper");
        assert_eq!(
            db.papers[0].paper.metadata.doi.as_deref(),
            Some("10.5555/open")
        );
    }

    #[test]
    fn cached_paper_fulltext_round_trips_from_paperseed() {
        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::new(dir.path().join("corpus"), None);
        let source = dir.path().join("cached.txt");
        std::fs::write(&source, "cached paper body text for retrieval").unwrap();

        let paper = api
            .import_local_file(
                &source,
                Some("Cached Paper".to_string()),
                Some("cc-by".to_string()),
            )
            .unwrap();
        let detail = api.get_cached_paper(&paper.metadata.id).unwrap();
        assert_eq!(detail.paper_id, paper.metadata.id);
        assert!(detail.has_full_text);

        let fulltext = api.get_cached_paper_fulltext(&paper.metadata.id).unwrap();
        assert_eq!(fulltext.content, "cached paper body text for retrieval");
    }

    #[tokio::test]
    async fn service_prepares_cached_paper_for_vox() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn LibraryBackend> = Arc::new(StubLocalReadOnlyBackend);
        let source = dir.path().join("cache.txt");
        std::fs::write(&source, "cached paper body for vox preparation").unwrap();

        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let paper = api
            .ingest_with_metadata(
                &source,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Graph Learning at Scale".to_string()),
                    doi: Some("10.5555/graph".to_string()),
                    authors: vec!["Grace Hopper".to_string()],
                    year: Some(2024),
                    venue: Some("Systems Journal".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://example.org/graph".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let service = PaperbridgeService::new(backend).with_paperseed(PaperseedMirrorConfig {
            corpus_root: Some(dir.path().join("corpus").display().to_string()),
            unpaywall_email: None,
            auto_download: false,
            yams_enabled: false,
        });

        let payload = service
            .prepare_item_for_vox(PrepareItemForVoxRequest {
                item_key: paper.metadata.id.clone(),
                attachment_key: None,
                max_chars_per_chunk: Some(12),
            })
            .await
            .unwrap();
        assert_eq!(payload.item_key, paper.metadata.id);
        assert!(!payload.vox.chunks.is_empty());
    }

    #[tokio::test]
    async fn service_builds_structure_for_cached_paper() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn LibraryBackend> = Arc::new(StubLocalReadOnlyBackend);
        let source = dir.path().join("structure.txt");
        std::fs::write(&source, "cached structure body text").unwrap();

        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let paper = api
            .ingest_with_metadata(
                &source,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Structured Cached Paper".to_string()),
                    doi: Some("10.5555/structure".to_string()),
                    authors: vec!["Grace Hopper".to_string()],
                    year: Some(2024),
                    venue: Some("Systems Journal".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://example.org/structure".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let service = PaperbridgeService::new(backend).with_paperseed(PaperseedMirrorConfig {
            corpus_root: Some(dir.path().join("corpus").display().to_string()),
            unpaywall_email: None,
            auto_download: false,
            yams_enabled: false,
        });

        let structure = service
            .get_paper_structure(&paper.metadata.id, None)
            .await
            .unwrap();
        assert_eq!(structure.item_key, paper.metadata.id);
        assert_eq!(
            structure.attachment_key.as_deref(),
            Some(paper.metadata.id.as_str())
        );
        assert_eq!(
            structure.metadata.title.as_deref(),
            Some("Structured Cached Paper")
        );
        assert_eq!(
            structure.source,
            crate::models::PaperStructureSource::ZoteroFulltext
        );
        assert_eq!(structure.sections.len(), 1);
        assert_eq!(structure.sections[0].heading, "Body");
        assert_eq!(structure.sections[0].text, "cached structure body text");
    }

    #[tokio::test]
    async fn service_queries_cached_paper_structure() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn LibraryBackend> = Arc::new(StubLocalReadOnlyBackend);
        let source = dir.path().join("query.txt");
        std::fs::write(&source, "cached query body text").unwrap();

        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let paper = api
            .ingest_with_metadata(
                &source,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Queryable Cached Paper".to_string()),
                    doi: Some("10.5555/query".to_string()),
                    authors: vec!["Grace Hopper".to_string()],
                    year: Some(2024),
                    venue: Some("Systems Journal".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://example.org/query".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let service = PaperbridgeService::new(backend).with_paperseed(PaperseedMirrorConfig {
            corpus_root: Some(dir.path().join("corpus").display().to_string()),
            unpaywall_email: None,
            auto_download: false,
            yams_enabled: false,
        });

        let value = service
            .query_paper(&paper.metadata.id, "sections[0].text", None)
            .await
            .unwrap();
        assert_eq!(
            value,
            serde_json::Value::String("cached query body text".to_string())
        );
    }

    #[tokio::test]
    async fn service_returns_real_pdf_fixture_as_structured_json() {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn LibraryBackend> = Arc::new(StubLocalReadOnlyBackend);
        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let paper = api
            .ingest_with_metadata(
                paperseed_fixture_path("arxiv_1408_5939_planar_subgraphs.pdf"),
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Planar Induced Subgraphs of Sparse Graphs".to_string()),
                    doi: Some("10.48550/arXiv.1408.5939".to_string()),
                    authors: vec![
                        "Glencora Borradaile".to_string(),
                        "David Eppstein".to_string(),
                    ],
                    year: Some(2014),
                    venue: Some("arXiv".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://arxiv.org/abs/1408.5939".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let service = PaperbridgeService::new(backend).with_paperseed(PaperseedMirrorConfig {
            corpus_root: Some(dir.path().join("corpus").display().to_string()),
            unpaywall_email: None,
            auto_download: false,
            yams_enabled: false,
        });

        let structure = service
            .get_paper_structure(&paper.metadata.id, None)
            .await
            .unwrap();
        let json = serde_json::to_value(&structure).unwrap();
        let sections = json["sections"].as_array().expect("sections array");
        let combined = sections
            .iter()
            .filter_map(|section| section["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            json["metadata"]["title"],
            "Planar Induced Subgraphs of Sparse Graphs"
        );
        assert!(
            !sections.is_empty(),
            "expected at least one section in structured JSON"
        );
        assert!(combined.contains("Glencora Borradaile"));
        assert!(combined.contains("induced pseudoforest"));
    }

    #[tokio::test]
    async fn search_papers_dedupes_external_matches_against_cache() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/arxiv"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<feed xmlns=\"http://www.w3.org/2005/Atom\">
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <title>Matched External Paper</title>
    <summary>Matched abstract</summary>
    <published>2024-01-01T00:00:00Z</published>
    <author><name>Grace Hopper</name></author>
    <link rel=\"alternate\" href=\"https://example.org/matched\" />
    <link title=\"pdf\" href=\"https://example.org/matched.pdf\" type=\"application/pdf\" />
  </entry>
</feed>"#,
            ))
            .mount(&server)
            .await;

        let base = server.uri();
        let paper_search = crate::external::PaperSearch::with_clients(
            crate::external::ArxivClient::new(Some(&format!("{base}/arxiv"))),
            crate::external::HuggingFaceClient::new(Some(&format!("{base}/hf")), None),
            crate::external::SemanticScholarClient::new(Some(&format!("{base}/s2")), None),
            crate::crossref::CrossrefClient::new(Some(&format!("{base}/crossref"))),
        );

        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );

        let matched_source = dir.path().join("matched.txt");
        std::fs::write(&matched_source, "matched cached body").unwrap();
        let matched = api
            .ingest_with_metadata(
                &matched_source,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Matched External Paper".to_string()),
                    doi: None,
                    authors: vec!["Grace Hopper".to_string()],
                    year: Some(2024),
                    venue: Some("Systems Journal".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://example.org/matched".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let local_source = dir.path().join("local.txt");
        std::fs::write(&local_source, "local cached body").unwrap();
        let local = api
            .ingest_with_metadata(
                &local_source,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Local Only Paper".to_string()),
                    doi: Some("10.5555/local".to_string()),
                    authors: vec!["Ada Lovelace".to_string()],
                    year: Some(2023),
                    venue: Some("Local Venue".to_string()),
                    license: Some("cc-by".to_string()),
                    source_url: Some("https://example.org/local".to_string()),
                },
                Some("cc-by".to_string()),
            )
            .unwrap();

        let backend: Arc<dyn LibraryBackend> = Arc::new(StubLocalReadOnlyBackend);
        let service = PaperbridgeService::with_paper_search(backend, paper_search).with_paperseed(
            PaperseedMirrorConfig {
                corpus_root: Some(dir.path().join("corpus").display().to_string()),
                unpaywall_email: None,
                auto_download: false,
                yams_enabled: false,
            },
        );

        let result = service
            .search_papers(SearchOptions {
                query: "matched external paper".to_string(),
                limit_per_source: 5,
                sources: Some(vec![PaperSource::Arxiv, PaperSource::Paperseed]),
                timeout_ms: 8_000,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();
        let hits = result.hits;

        assert!(!hits.is_empty());

        // The arXiv entry "Matched External Paper" collides on title+author with
        // the paperseed-cached copy of the same paper. merge_prefer_cached replaces
        // the external entry with the cached one in place, so the result list has
        // no Arxiv-source duplicate of a cached paper.
        assert!(
            !hits.iter().any(
                |hit| hit.source == PaperSource::Arxiv && hit.title == "Matched External Paper"
            ),
            "external duplicate of cached paper should be replaced by the cache entry"
        );

        let matched_cached = hits
            .iter()
            .find(|hit| {
                hit.cache.as_ref().map(|c| c.paper_id.as_str())
                    == Some(matched.metadata.id.as_str())
            })
            .expect("matched cached hit present");
        assert_eq!(matched_cached.title, "Matched External Paper");
        assert_eq!(matched_cached.source, PaperSource::Paperseed);

        let local_cached = hits
            .iter()
            .find(|hit| {
                hit.cache.as_ref().map(|c| c.paper_id.as_str()) == Some(local.metadata.id.as_str())
            })
            .expect("local cached hit present");
        assert_eq!(local_cached.title, "Local Only Paper");
    }

    #[tokio::test]
    async fn search_papers_source_filter_excludes_cache_unless_requested() {
        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let source = dir.path().join("graph.txt");
        std::fs::write(&source, "graph neural networks for program analysis").unwrap();
        api.ingest_with_metadata(
            &source,
            paperseed::sources::PaperbridgeMetadata {
                title: Some("Graph Neural Networks for Program Analysis".to_string()),
                doi: Some("10.5555/graph".to_string()),
                authors: vec!["Ada Lovelace".to_string()],
                year: Some(2024),
                venue: Some("Local Venue".to_string()),
                license: Some("cc-by".to_string()),
                source_url: Some("https://example.org/graph".to_string()),
            },
            Some("cc-by".to_string()),
        )
        .unwrap();

        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend)).with_paperseed(
            PaperseedMirrorConfig {
                corpus_root: Some(dir.path().join("corpus").display().to_string()),
                unpaywall_email: None,
                auto_download: false,
                yams_enabled: false,
            },
        );

        let arxiv_only = service
            .search_papers(SearchOptions {
                query: "graph neural networks".to_string(),
                limit_per_source: 5,
                sources: Some(vec![PaperSource::Arxiv]),
                timeout_ms: 200,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();
        assert!(
            arxiv_only
                .hits
                .iter()
                .all(|hit| hit.source != PaperSource::Paperseed),
            "cache should not surface when --sources excludes paperseed: {:?}",
            arxiv_only.hits
        );

        let cache_only = service
            .search_papers(SearchOptions {
                query: "graph neural networks".to_string(),
                limit_per_source: 5,
                sources: Some(vec![PaperSource::Paperseed]),
                timeout_ms: 200,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();
        assert_eq!(cache_only.hits.len(), 1);
        assert_eq!(cache_only.hits[0].source, PaperSource::Paperseed);
    }

    #[tokio::test]
    async fn search_papers_ranks_exact_title_match_ahead_of_source_order() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/crossref/works"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {
                    "items": [
                        {"DOI": "10.1/noisy-1", "title": ["Is Attention All You Need?"]},
                        {"DOI": "10.1/noisy-2", "title": ["Attention Is All You Need for Routing"]}
                    ]
                }
            })))
            .mount(&server)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/arxiv"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<feed xmlns=\"http://www.w3.org/2005/Atom\">
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <published>2017-06-12T00:00:00Z</published>
    <title>Attention Is All You Need</title>
    <summary>Canonical transformer paper.</summary>
    <author><name>Ashish Vaswani</name></author>
    <link href=\"http://arxiv.org/abs/1706.03762v7\" rel=\"alternate\" type=\"text/html\"/>
    <link title=\"pdf\" href=\"http://arxiv.org/pdf/1706.03762v7\" rel=\"related\" type=\"application/pdf\"/>
  </entry>
</feed>"#,
            ))
            .mount(&server)
            .await;

        let base = server.uri();
        let paper_search = crate::external::PaperSearch::with_clients(
            crate::external::ArxivClient::new(Some(&format!("{base}/arxiv"))),
            crate::external::HuggingFaceClient::new(Some(&format!("{base}/hf")), None),
            crate::external::SemanticScholarClient::new(Some(&format!("{base}/s2")), None),
            crate::crossref::CrossrefClient::new(Some(&format!("{base}/crossref"))),
        );

        let service =
            PaperbridgeService::with_paper_search(Arc::new(StubLocalReadOnlyBackend), paper_search);
        let result = service
            .search_papers(SearchOptions {
                query: "attention is all you need".to_string(),
                limit_per_source: 5,
                sources: Some(vec![PaperSource::Crossref, PaperSource::Arxiv]),
                timeout_ms: 8_000,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();

        assert_eq!(result.hits[0].source, PaperSource::Arxiv);
        assert_eq!(result.hits[0].title, "Attention Is All You Need");
    }

    #[tokio::test]
    async fn search_papers_ranks_exact_arxiv_id_match_ahead_of_title_noise() {
        let server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/crossref/works"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {
                    "items": [
                        {"DOI": "10.1/noisy-1", "title": ["Understanding 1706.03762 in Context"]}
                    ]
                }
            })))
            .mount(&server)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/arxiv"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<feed xmlns=\"http://www.w3.org/2005/Atom\">
  <entry>
    <id>http://arxiv.org/abs/1706.03762v7</id>
    <published>2017-06-12T00:00:00Z</published>
    <title>Attention Is All You Need</title>
    <summary>Canonical transformer paper.</summary>
    <author><name>Ashish Vaswani</name></author>
    <link href=\"http://arxiv.org/abs/1706.03762v7\" rel=\"alternate\" type=\"text/html\"/>
    <link title=\"pdf\" href=\"http://arxiv.org/pdf/1706.03762v7\" rel=\"related\" type=\"application/pdf\"/>
  </entry>
</feed>"#,
            ))
            .mount(&server)
            .await;

        let base = server.uri();
        let paper_search = crate::external::PaperSearch::with_clients(
            crate::external::ArxivClient::new(Some(&format!("{base}/arxiv"))),
            crate::external::HuggingFaceClient::new(Some(&format!("{base}/hf")), None),
            crate::external::SemanticScholarClient::new(Some(&format!("{base}/s2")), None),
            crate::crossref::CrossrefClient::new(Some(&format!("{base}/crossref"))),
        );

        let service =
            PaperbridgeService::with_paper_search(Arc::new(StubLocalReadOnlyBackend), paper_search);
        let result = service
            .search_papers(SearchOptions {
                query: "1706.03762".to_string(),
                limit_per_source: 5,
                sources: Some(vec![PaperSource::Crossref, PaperSource::Arxiv]),
                timeout_ms: 8_000,
                offset: 0,
                limit: 0,
                cache_mode: SearchCacheMode::Auto,
                detail: crate::models::SearchDetail::Compact,
                abstract_max_chars: None,
            })
            .await
            .unwrap();

        assert_eq!(result.hits[0].source, PaperSource::Arxiv);
        assert_eq!(result.hits[0].arxiv_id.as_deref(), Some("1706.03762"));
    }

    #[tokio::test]
    async fn local_backend_rejects_create_collection_before_backend_call() {
        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend));
        let err = service
            .create_collection(CollectionWriteRequest {
                name: "Test".to_string(),
                parent_collection: None,
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("not available for the active local backend")
        );
    }

    #[tokio::test]
    async fn local_backend_rejects_delete_item_before_backend_call() {
        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend));
        let err = service
            .delete_item(DeleteItemRequest {
                key: "ABCD1234".to_string(),
                version: Some(1),
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("not available for the active local backend")
        );
    }

    fn make_hit(source: PaperSource, title: &str, doi: Option<&str>) -> PaperHit {
        PaperHit {
            hit_id: None,
            source,
            title: title.to_string(),
            authors: vec!["A. Author".to_string()],
            year: None,
            doi: doi.map(|d| d.to_string()),
            arxiv_id: None,
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

    fn make_cache_hit(title: &str, doi: Option<&str>) -> PaperHit {
        let mut hit = make_hit(PaperSource::Paperseed, title, doi);
        hit.cache = Some(CachedPaperSummary {
            paper_id: "p1".to_string(),
            cached: true,
            has_full_text: true,
        });
        hit
    }

    #[test]
    fn search_rank_prefers_exact_title_over_noisy_variant_even_with_more_citations() {
        let query_title = normalize_search_text("attention is all you need");
        let query_tokens: Vec<String> =
            query_title.split_whitespace().map(str::to_string).collect();
        let mut exact = make_hit(PaperSource::Arxiv, "Attention Is All You Need", None);
        exact.citation_count = Some(10);
        let mut noisy = make_hit(
            PaperSource::OpenAlex,
            "Attention Is All You Need In Speech Separation",
            None,
        );
        noisy.citation_count = Some(10_000);

        assert!(
            search_rank(&query_title, &query_tokens, None, None, &exact)
                > search_rank(&query_title, &query_tokens, None, None, &noisy)
        );
    }

    #[test]
    fn search_rank_normalizes_doi_queries() {
        let query_title = normalize_search_text("https://doi.org/10.1000/XYZ");
        let query_tokens: Vec<String> =
            query_title.split_whitespace().map(str::to_string).collect();
        let query_doi = normalize_doi("https://doi.org/10.1000/XYZ");
        let mut doi_hit = make_hit(PaperSource::Crossref, "Paper A", Some("doi:10.1000/xyz"));
        doi_hit.citation_count = Some(1);
        let title_hit = make_hit(PaperSource::OpenAlex, "10 1000 xyz analysis", None);

        assert!(
            search_rank(
                &query_title,
                &query_tokens,
                query_doi.as_deref(),
                None,
                &doi_hit,
            ) > search_rank(
                &query_title,
                &query_tokens,
                query_doi.as_deref(),
                None,
                &title_hit,
            )
        );
    }

    #[test]
    fn search_rank_normalizes_arxiv_queries() {
        let query_title = normalize_search_text("https://arxiv.org/abs/1706.03762v7");
        let query_tokens: Vec<String> =
            query_title.split_whitespace().map(str::to_string).collect();
        let query_arxiv = normalize_arxiv_id("https://arxiv.org/abs/1706.03762v7");
        let mut arxiv_hit = make_hit(PaperSource::Arxiv, "Attention Is All You Need", None);
        arxiv_hit.arxiv_id = Some("1706.03762".to_string());
        let noisy_hit = make_hit(
            PaperSource::Crossref,
            "Understanding 1706.03762 in Context",
            None,
        );

        assert!(
            search_rank(
                &query_title,
                &query_tokens,
                None,
                query_arxiv.as_deref(),
                &arxiv_hit,
            ) > search_rank(
                &query_title,
                &query_tokens,
                None,
                query_arxiv.as_deref(),
                &noisy_hit,
            )
        );
    }

    #[test]
    fn normalize_doi_rejects_prefix_only_input() {
        // `doi:` alone has no body; should not be considered a DOI shape.
        assert_eq!(normalize_doi("doi:"), None);
        assert_eq!(normalize_doi("https://doi.org/"), None);
    }

    #[test]
    fn normalize_doi_rejects_text_that_lacks_doi_shape() {
        // Falls back to looks_like_doi check; bare words aren't DOIs.
        assert_eq!(normalize_doi("not a doi"), None);
        assert_eq!(normalize_doi("12345"), None);
    }

    #[test]
    fn normalize_doi_accepts_canonical_and_url_forms() {
        assert_eq!(
            normalize_doi("10.1234/Abc"),
            Some("10.1234/abc".to_string())
        );
        assert_eq!(
            normalize_doi("https://doi.org/10.1234/Abc"),
            Some("10.1234/abc".to_string())
        );
        assert_eq!(
            normalize_doi("doi:10.1234/abc"),
            Some("10.1234/abc".to_string())
        );
    }

    #[test]
    fn normalize_arxiv_id_handles_version_suffix() {
        assert_eq!(
            normalize_arxiv_id("1706.03762v7"),
            Some("1706.03762".to_string())
        );
        assert_eq!(
            normalize_arxiv_id("https://arxiv.org/abs/1706.03762"),
            Some("1706.03762".to_string())
        );
        assert_eq!(
            normalize_arxiv_id("arxiv:1706.03762v1"),
            Some("1706.03762".to_string())
        );
    }

    #[test]
    fn normalize_arxiv_id_rejects_empty_after_prefix_strip() {
        // The version-stripper had a partial-id case worth pinning down.
        assert_eq!(normalize_arxiv_id(""), None);
        assert_eq!(normalize_arxiv_id("   "), None);
        // `v7` alone has no base — version-stripper's empty-base guard
        // kicks in and we keep the raw "v7" as the ID. That's deliberate;
        // pin it so future regex edits don't silently accept empty bases.
        assert_eq!(normalize_arxiv_id("v7"), Some("v7".to_string()));
    }

    #[test]
    fn normalize_search_text_handles_non_latin_input() {
        // Non-ASCII alphanumerics survive (they're alphabetic per Unicode).
        // Pin the behavior so a future "ASCII-only filter" pass can't
        // regress non-English queries silently.
        let out = normalize_search_text("中文 paper");
        assert!(out.contains("中文"), "got {out:?}");
        assert!(out.contains("paper"));
    }

    #[test]
    fn merge_prefer_cached_keeps_cache_only_hit_at_tail() {
        let mut hits = vec![
            make_hit(PaperSource::Arxiv, "External A", Some("10.1/a")),
            make_hit(PaperSource::Arxiv, "External B", Some("10.1/b")),
        ];
        let cache_start = hits.len();
        hits.push(make_cache_hit("Cached Only", Some("10.1/c")));

        merge_prefer_cached(&mut hits, cache_start);

        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].title, "External A");
        assert_eq!(hits[1].title, "External B");
        assert_eq!(hits[2].title, "Cached Only");
        assert!(hits[2].cache.is_some());
    }

    #[test]
    fn merge_prefer_cached_replaces_external_at_collision_position() {
        let mut hits = vec![
            make_hit(PaperSource::Arxiv, "External A", Some("10.1/a")),
            make_hit(PaperSource::Arxiv, "External B", Some("10.1/b")),
            make_hit(PaperSource::Arxiv, "External C", Some("10.1/c")),
        ];
        let cache_start = hits.len();
        hits.push(make_cache_hit("Cached B", Some("10.1/b")));

        merge_prefer_cached(&mut hits, cache_start);

        assert_eq!(hits.len(), 3, "collision should replace, not append");
        assert_eq!(hits[0].title, "External A");
        assert_eq!(
            hits[1].title, "Cached B",
            "cache hit must take external B's slot"
        );
        assert!(
            hits[1].cache.is_some(),
            "cached entry should win on DOI collision"
        );
        assert_eq!(hits[2].title, "External C");
    }

    #[test]
    fn merge_prefer_cached_does_not_force_cache_to_top() {
        // Regression: previously a sort_by_key forced all cached hits to the
        // front of the list regardless of relevance. After the fix, cache
        // hits keep their natural position (tail unless they collide).
        let mut hits = vec![make_hit(
            PaperSource::Arxiv,
            "Strong External",
            Some("10.1/strong"),
        )];
        let cache_start = hits.len();
        hits.push(make_cache_hit("Weak Cached", Some("10.1/weak")));

        merge_prefer_cached(&mut hits, cache_start);

        assert_eq!(hits[0].title, "Strong External");
        assert_eq!(hits[1].title, "Weak Cached");
    }

    #[test]
    fn merge_prefer_cached_handles_empty_cache_segment() {
        let mut hits = vec![make_hit(PaperSource::Arxiv, "Only External", None)];
        let cache_start = hits.len();

        merge_prefer_cached(&mut hits, cache_start);

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Only External");
    }

    #[test]
    fn cache_auto_rejects_common_or_generic_queries() {
        let mut hit = make_cache_hit("A survey of transfer learning", Some("10.1/cache"));
        hit.abstract_note = Some(
            "The field of data mining and machine learning has been used in many applications."
                .to_string(),
        );
        hit.relevance_score = Some(4.0);

        assert!(!should_surface_cached_hit("what is this about", &hit));
        assert!(!should_surface_cached_hit("no literature unrelated", &hit));
        assert!(!should_surface_cached_hit("new topic", &hit));
    }

    #[test]
    fn cache_auto_accepts_strong_metadata_matches() {
        let mut hit = make_cache_hit(
            "Graph Neural Networks for Program Analysis",
            Some("10.1/cache"),
        );
        hit.abstract_note = Some("Graph neural models over program structure.".to_string());
        hit.relevance_score = Some(2.0);

        assert!(should_surface_cached_hit("graph neural networks", &hit));
    }

    #[test]
    fn cache_mode_respects_explicit_sources() {
        assert_eq!(
            effective_cache_mode(SearchCacheMode::Auto, Some(&[PaperSource::Arxiv])),
            SearchCacheMode::Off
        );
        assert_eq!(
            effective_cache_mode(SearchCacheMode::Auto, Some(&[PaperSource::Paperseed])),
            SearchCacheMode::Only
        );
        assert_eq!(
            effective_cache_mode(
                SearchCacheMode::Auto,
                Some(&[PaperSource::Arxiv, PaperSource::Paperseed]),
            ),
            SearchCacheMode::Include
        );
    }

    // ---- Phase B1: backend info, validator, and prepare_vox_text coverage ----

    fn service() -> PaperbridgeService {
        PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend))
    }

    fn paperseed_fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/paperseed/tests/fixtures")
            .join(name)
    }

    #[test]
    fn backend_mode_reports_local_for_local_stub() {
        assert_eq!(service().backend_mode(), BackendMode::Local);
    }

    #[test]
    fn backend_capabilities_reports_read_only_for_local_stub() {
        let caps = service().backend_capabilities();
        assert_eq!(caps, BackendCapabilities::read_only_local());
        assert!(caps.read_library);
        assert!(!caps.write_basic);
    }

    #[test]
    fn backend_info_returns_local_shape() {
        let info = service().backend_info();
        assert_eq!(info.mode, "local");
        assert!(info.read_library);
        assert!(!info.write_basic);
        assert!(!info.group_libraries);
    }

    fn item_write_request(title: Option<&str>) -> ItemWriteRequest {
        ItemWriteRequest {
            item_type: "journalArticle".to_string(),
            title: title.map(|s| s.to_string()),
            creators: vec![],
            abstract_note: None,
            date: None,
            url: None,
            doi: None,
            isbn: None,
            tags: vec![],
            collections: vec![],
            extra: None,
            parent_item: None,
        }
    }

    fn item_update_request(key: &str) -> ItemUpdateRequest {
        ItemUpdateRequest {
            key: key.to_string(),
            version: None,
            item_type: None,
            title: None,
            creators: None,
            abstract_note: None,
            date: None,
            url: None,
            doi: None,
            isbn: None,
            tags: None,
            collections: None,
            extra: None,
            parent_item: None,
            clear_parent: false,
        }
    }

    fn collection_update_request(key: &str) -> CollectionUpdateRequest {
        CollectionUpdateRequest {
            key: key.to_string(),
            version: None,
            name: None,
            parent_collection: None,
            clear_parent: false,
        }
    }

    #[test]
    fn validate_item_request_rejects_missing_title() {
        let report = service().validate_item_request(&item_write_request(None));
        assert!(
            !report.valid,
            "missing title must produce at least one issue"
        );
    }

    #[test]
    fn validate_item_request_accepts_well_formed_input() {
        let mut req = item_write_request(Some("Attention Is All You Need"));
        req.doi = Some("10.1234/abc".to_string());
        let report = service().validate_item_request(&req);
        assert!(
            report.valid,
            "well-formed request reported issues: {:?}",
            report.issues
        );
    }

    #[test]
    fn validate_collection_request_rejects_empty_name() {
        let report = service().validate_collection_request(&CollectionWriteRequest {
            name: "   ".to_string(),
            parent_collection: None,
        });
        assert!(!report.valid);
    }

    #[test]
    fn validate_item_update_request_requires_key() {
        let report = service().validate_item_update_request(&item_update_request(""));
        assert!(!report.valid);
    }

    #[test]
    fn validate_collection_update_request_requires_key() {
        let report = service().validate_collection_update_request(&collection_update_request(""));
        assert!(!report.valid);
    }

    #[test]
    fn validate_delete_item_request_requires_key() {
        let report = service().validate_delete_item_request(&DeleteItemRequest {
            key: "".to_string(),
            version: None,
        });
        assert!(!report.valid);
    }

    #[test]
    fn validate_delete_collection_request_requires_key() {
        let report = service().validate_delete_collection_request(&DeleteCollectionRequest {
            key: "".to_string(),
            version: None,
        });
        assert!(!report.valid);
    }

    #[tokio::test]
    async fn prepare_vox_text_chunks_inline_text_with_default_source() {
        let payload = service()
            .prepare_vox_text(PrepareVoxTextRequest {
                text: Some("inline text content for vox preparation".to_string()),
                attachment_key: None,
                source_label: None,
                max_chars_per_chunk: Some(8),
            })
            .await
            .unwrap();
        assert_eq!(payload.source, "manual-text");
        assert!(!payload.chunks.is_empty());
        assert!(payload.chunk_count >= 1);
    }

    #[tokio::test]
    async fn prepare_vox_text_honors_custom_source_label() {
        let payload = service()
            .prepare_vox_text(PrepareVoxTextRequest {
                text: Some("hello world".to_string()),
                attachment_key: None,
                source_label: Some("note:42".to_string()),
                max_chars_per_chunk: None,
            })
            .await
            .unwrap();
        assert_eq!(payload.source, "note:42");
    }

    #[tokio::test]
    async fn prepare_vox_text_errors_when_no_text_or_attachment() {
        let err = service()
            .prepare_vox_text(PrepareVoxTextRequest {
                text: None,
                attachment_key: None,
                source_label: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Provide either"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn with_unpaywall_attaches_client_when_email_present() {
        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend))
            .with_unpaywall(Some("tests@example.com".to_string()));
        assert!(service.unpaywall.is_some());
    }

    #[test]
    fn with_unpaywall_skips_client_when_email_missing() {
        let service =
            PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend)).with_unpaywall(None);
        assert!(service.unpaywall.is_none());
    }

    #[test]
    fn with_paper_config_stores_config() {
        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend))
            .with_paper_config(PaperConfig {
                grobid_url: Some("http://localhost:8070".to_string()),
                grobid_auto_spawn: false,
                grobid_image: "lfoppiano/grobid:latest".to_string(),
                grobid_timeout_secs: 30,
            });
        let cfg = service.paper_config.as_ref().expect("paper_config set");
        assert_eq!(cfg.grobid_url.as_deref(), Some("http://localhost:8070"));
        assert!(!cfg.grobid_auto_spawn);
    }

    #[test]
    fn resolve_open_targets_parses_hit_ids() {
        let r = resolve_open_targets(&OpenPaperRequest {
            hit_id: Some("arxiv:1706.03762v7".into()),
            doi: None,
            arxiv_id: None,
            item_key: None,
            paper_id: None,
            attachment_key: None,
            url: None,
            want: vec![],
            max_chars: None,
            selector: None,
            max_chars_per_chunk: None,
        })
        .unwrap();
        assert_eq!(r.arxiv_id.as_deref(), Some("1706.03762"));

        let r = resolve_open_targets(&OpenPaperRequest {
            hit_id: Some("doi:10.5555/3295222.3295349".into()),
            doi: None,
            arxiv_id: None,
            item_key: None,
            paper_id: None,
            attachment_key: None,
            url: None,
            want: vec![],
            max_chars: None,
            selector: None,
            max_chars_per_chunk: None,
        })
        .unwrap();
        assert_eq!(r.doi.as_deref(), Some("10.5555/3295222.3295349"));

        let r = resolve_open_targets(&OpenPaperRequest {
            hit_id: Some("paperseed:abc123".into()),
            doi: None,
            arxiv_id: None,
            item_key: None,
            paper_id: None,
            attachment_key: None,
            url: None,
            want: vec![],
            max_chars: None,
            selector: None,
            max_chars_per_chunk: None,
        })
        .unwrap();
        assert_eq!(r.paper_id.as_deref(), Some("abc123"));

        let r = resolve_open_targets(&OpenPaperRequest {
            hit_id: Some("url:https://openreview.net/pdf?id=abc123".into()),
            doi: None,
            arxiv_id: None,
            item_key: None,
            paper_id: None,
            attachment_key: None,
            url: None,
            want: vec![],
            max_chars: None,
            selector: None,
            max_chars_per_chunk: None,
        })
        .unwrap();
        assert_eq!(
            r.url.as_deref(),
            Some("https://openreview.net/pdf?id=abc123")
        );
    }

    #[test]
    fn truncate_fulltext_sets_total_and_indexed_chars() {
        let full = FulltextContent {
            item_key: "k".into(),
            content: "abcdefghij".into(),
            indexed_pages: None,
            total_pages: None,
            indexed_chars: None,
            total_chars: None,
        };
        let t = truncate_fulltext(&full, 4);
        assert_eq!(t.content, "abcd");
        assert_eq!(t.indexed_chars, Some(4));
        assert_eq!(t.total_chars, Some(10));
    }

    #[tokio::test]
    async fn open_paper_returns_arxiv_metadata_without_backend() {
        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend));
        let value = service
            .open_paper(OpenPaperRequest {
                hit_id: Some("arxiv:1706.03762".into()),
                doi: None,
                arxiv_id: None,
                item_key: None,
                paper_id: None,
                attachment_key: None,
                url: None,
                want: vec!["metadata".into()],
                max_chars: None,
                selector: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap();
        assert_eq!(value["metadata"]["arxiv_id"].as_str(), Some("1706.03762"));
        assert!(
            value["metadata"]["pdf_url"]
                .as_str()
                .unwrap()
                .contains("arxiv.org/pdf/1706.03762")
        );
    }

    #[tokio::test]
    async fn open_paper_fulltext_from_cached_paper_id() {
        let dir = tempfile::tempdir().unwrap();
        let api = PaperseedApi::with_yams(
            dir.path().join("corpus"),
            None,
            paperseed::yams::YamsConfig::disabled(),
        );
        let src = dir.path().join("body.txt");
        std::fs::write(&src, "hello open paper fulltext body").unwrap();
        let paper = api
            .ingest_with_metadata(
                &src,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Open Paper Cache".into()),
                    doi: Some("10.1/open-paper".into()),
                    authors: vec!["Test".into()],
                    year: Some(2024),
                    venue: None,
                    license: Some("cc-by".into()),
                    source_url: None,
                },
                Some("cc-by".into()),
            )
            .unwrap();

        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend)).with_paperseed(
            PaperseedMirrorConfig {
                corpus_root: Some(dir.path().join("corpus").display().to_string()),
                unpaywall_email: None,
                auto_download: false,
                yams_enabled: false,
            },
        );

        let value = service
            .open_paper(OpenPaperRequest {
                hit_id: None,
                doi: None,
                arxiv_id: None,
                item_key: None,
                paper_id: Some(paper.metadata.id.clone()),
                attachment_key: None,
                url: None,
                want: vec!["fulltext".into()],
                max_chars: Some(12),
                selector: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap();

        let content = value["fulltext"]["content"].as_str().unwrap();
        assert_eq!(content, "hello open p");
        assert_eq!(value["fulltext"]["indexed_chars"], 12);
        assert!(value["fulltext"]["total_chars"].as_u64().unwrap() > 12);
    }

    #[tokio::test]
    async fn open_url_builds_structure_without_paperseed() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let fixture = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates/paperseed/tests/fixtures/arxiv_1408_5939_planar_subgraphs.pdf"),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/paper.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture))
            .expect(1)
            .mount(&server)
            .await;

        let value = service()
            .open_paper(OpenPaperRequest {
                hit_id: Some(format!("url:{}/paper.pdf", server.uri())),
                doi: None,
                arxiv_id: None,
                item_key: None,
                paper_id: None,
                attachment_key: None,
                url: None,
                want: vec!["structure".into()],
                max_chars: Some(2_000),
                selector: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap();

        assert!(
            value["structure"]["sections"]
                .as_array()
                .is_some_and(|s| !s.is_empty())
        );
        assert_eq!(
            value["resolved"]["url"].as_str(),
            Some(format!("{}/paper.pdf", server.uri()).as_str())
        );
    }

    #[tokio::test]
    async fn open_url_does_not_accept_unrelated_cached_search_result() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let fixture = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("crates/paperseed/tests/fixtures/arxiv_1408_5939_planar_subgraphs.pdf"),
        )
        .unwrap();
        Mock::given(method("GET"))
            .and(path("/paper.pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let corpus_root = dir.path().join("corpus");
        let api =
            PaperseedApi::with_yams(&corpus_root, None, paperseed::yams::YamsConfig::disabled());
        let wrong = dir.path().join("paper-pdf.txt");
        std::fs::write(&wrong, "wrong cached document").unwrap();
        let wrong_paper = api
            .ingest_with_metadata(
                &wrong,
                paperseed::sources::PaperbridgeMetadata {
                    title: Some("Paper PDF Download".into()),
                    doi: Some("10.6028/NIST.AI.100-2e2023".into()),
                    authors: vec!["NIST".into()],
                    year: Some(2024),
                    venue: None,
                    license: Some("cc-by".into()),
                    source_url: Some("https://example.test/unrelated.pdf".into()),
                },
                Some("cc-by".into()),
            )
            .unwrap();

        let service = PaperbridgeService::new(Arc::new(StubLocalReadOnlyBackend)).with_paperseed(
            PaperseedMirrorConfig {
                corpus_root: Some(corpus_root.display().to_string()),
                unpaywall_email: None,
                auto_download: false,
                yams_enabled: false,
            },
        );
        let value = service
            .open_paper(OpenPaperRequest {
                hit_id: Some(format!("url:{}/paper.pdf", server.uri())),
                doi: None,
                arxiv_id: None,
                item_key: None,
                paper_id: None,
                attachment_key: None,
                url: None,
                want: vec!["fulltext".into()],
                max_chars: Some(2_000),
                selector: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap();

        assert_ne!(
            value["fulltext"]["content"].as_str(),
            Some("wrong cached document")
        );
        assert_ne!(
            value["resolved"]["paper_id"].as_str(),
            Some(wrong_paper.metadata.id.as_str())
        );
    }

    #[tokio::test]
    async fn open_paper_errors_without_identifiers() {
        let err = service()
            .open_paper(OpenPaperRequest {
                hit_id: None,
                doi: None,
                arxiv_id: None,
                item_key: None,
                paper_id: None,
                attachment_key: None,
                url: None,
                want: vec!["metadata".into()],
                max_chars: None,
                selector: None,
                max_chars_per_chunk: None,
            })
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("requires hit_id"),
            "unexpected: {err}"
        );
    }
}
