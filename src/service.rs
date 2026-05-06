use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::crossref::CrossrefClient;
use crate::error::{Result, ZoteroMcpError};
use crate::external::{PaperSearch, SearchOptions, UnpaywallClient};
use crate::models::{
    BackendInfo, CachedPaperDetail, CachedPaperSummary, CollectionSummary, CollectionUpdateRequest,
    CollectionWriteRequest, CrossrefWork, DeleteCollectionRequest, DeleteItemRequest,
    FulltextContent, ItemDetail, ItemSummary, ItemUpdateRequest, ItemVoxPayload, ItemWriteRequest,
    ListCollectionsQuery, PaperHit, PaperStructure, SearchItemsQuery, SearchPapersResult,
    SearchVoxPayload, ValidationIssue, ValidationIssueLevel, ValidationReport, VoxTextPayload,
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
        let query = opts.query.clone();
        let mut hits = self.paper_search.search(opts.clone()).await?;
        if let Some(api) = &self.paperseed {
            let mut cached_hits =
                api.search_cached_papers(&opts.query, opts.limit_per_source as usize)?;
            hits.append(&mut cached_hits);
        }
        self.annotate_cached_hits(&mut hits);
        hits.sort_by_key(|hit| {
            !hit.cache
                .as_ref()
                .map(|cache| cache.cached)
                .unwrap_or(false)
        });
        // Clamp to u32 via .min() — safe, bounded cast.
        let total_count = hits.len().min(u32::MAX as usize) as u32;
        self.mirror_open_access_hits(&hits);

        let offset = opts.offset;
        let limit = opts.limit;
        if limit > 0 || offset > 0 {
            let start = offset.min(total_count) as usize;
            let end = if limit > 0 {
                (start + limit as usize).min(total_count as usize)
            } else {
                total_count as usize
            };
            hits = hits[start..end].to_vec();
        }

        Ok(SearchPapersResult {
            query,
            total_count,
            offset,
            limit,
            hits,
        })
    }

    fn mirror_open_access_hits(&self, hits: &[PaperHit]) {
        if !self.paperseed_auto_download {
            return;
        }
        let Some(api) = &self.paperseed else {
            return;
        };
        let api = api.clone();
        let hits: Vec<PaperHit> = hits
            .iter()
            .filter(|hit| hit.oa_pdf_url.is_some() && hit.cache.is_none())
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
                    if let Err(error) = mirror_open_access_hit(&api, &hit).await {
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

    pub async fn list_collections(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<Vec<crate::models::CollectionSummary>> {
        self.backend.list_collections(query).await
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

fn cached_paper_structure(paper: &CachedPaperDetail, fulltext: &FulltextContent) -> PaperStructure {
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
        sections: if fulltext.content.trim().is_empty() {
            Vec::new()
        } else {
            vec![crate::models::PaperSection {
                id: "body".to_string(),
                heading: "Body".to_string(),
                level: 1,
                text: fulltext.content.clone(),
                subsections: Vec::new(),
            }]
        },
        references: Vec::new(),
        figures: Vec::new(),
        source: crate::models::PaperStructureSource::ZoteroFulltext,
    }
}

fn titles_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
    };
    norm(a) == norm(b)
}

fn summarize_validation_report(report: &ValidationReport) -> String {
    report
        .issues
        .iter()
        .map(|issue| format!("{}: {}", issue.field, issue.message))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn mirror_open_access_hit(api: &PaperseedApi, hit: &PaperHit) -> Result<()> {
    let Some(url) = hit.oa_pdf_url.as_deref() else {
        return Ok(());
    };
    let incoming = api.paths().root.join("incoming");
    std::fs::create_dir_all(&incoming).map_err(|e| {
        ZoteroMcpError::Config(format!("Failed to create Paperseed incoming dir: {e}"))
    })?;
    let file = incoming.join(format!("{}.pdf", paperseed_safe_name(hit)));
    let yams_downloaded = api.download_with_yams_queue(
        url,
        Some(&hit.title),
        hit.doi.as_deref(),
        hit.url.as_deref().or(hit.oa_pdf_url.as_deref()),
    );
    // YAMS queue handles download + indexing, nothing else needed.
    if yams_downloaded.is_some() {
        return Ok(());
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
            source_url: hit.url.clone().or_else(|| hit.oa_pdf_url.clone()),
        },
        Some("unknown".to_string()),
    )?;
    Ok(())
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
        };

        mirror_open_access_hit(&api, &hit).await.unwrap();
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
    async fn search_papers_prioritizes_cached_hits_and_annotates_external_matches() {
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
                sources: Some(vec![PaperSource::Arxiv]),
                timeout_ms: 8_000,
                offset: 0,
                limit: 0,
            })
            .await
            .unwrap();
        let hits = result.hits;

        assert!(!hits.is_empty());
        assert!(
            hits.iter()
                .all(|hit| hit.cache.as_ref().map(|c| c.cached).unwrap_or(false))
        );

        let first_non_cached = hits
            .iter()
            .position(|hit| hit.cache.is_none())
            .unwrap_or(hits.len());
        assert_eq!(first_non_cached, hits.len());

        let local_cached = hits
            .iter()
            .find(|hit| {
                hit.cache.as_ref().map(|c| c.paper_id.as_str()) == Some(local.metadata.id.as_str())
            })
            .expect("local cached hit present");
        assert_eq!(local_cached.title, "Local Only Paper");
        assert_eq!(
            local_cached.cache.as_ref().map(|c| c.paper_id.as_str()),
            Some(local.metadata.id.as_str())
        );

        let matched_external = hits
            .iter()
            .find(|hit| hit.source == PaperSource::Arxiv)
            .expect("external hit present");
        assert_eq!(matched_external.title, "Matched External Paper");
        assert_eq!(
            matched_external.cache.as_ref().map(|c| c.paper_id.as_str()),
            Some(matched.metadata.id.as_str())
        );
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
}
