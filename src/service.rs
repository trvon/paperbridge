use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::crossref::CrossrefClient;
use crate::error::{Result, ZoteroMcpError};
use crate::external::{PaperSearch, SearchOptions, UnpaywallClient};
use crate::models::{
    BackendInfo, CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest, CrossrefWork,
    DeleteCollectionRequest, DeleteItemRequest, FulltextContent, ItemDetail, ItemSummary,
    ItemUpdateRequest, ItemVoxPayload, ItemWriteRequest, ListCollectionsQuery, PaperHit,
    PaperStructure, SearchItemsQuery, SearchVoxPayload, ValidationIssue, ValidationIssueLevel,
    ValidationReport, VoxTextPayload,
};
use crate::paper;
use crate::paper::docker::DEFAULT_PORT as GROBID_DEFAULT_PORT;
use crate::paper::grobid::GrobidClient;
use crate::paper::tei;
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

    pub async fn search_papers(&self, opts: SearchOptions) -> Result<Vec<PaperHit>> {
        self.paper_search.search(opts).await
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

    pub async fn get_item_fulltext(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.backend.get_item_fulltext(attachment_key).await
    }

    pub async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.backend.get_pdf_text(attachment_key).await
    }

    pub async fn get_paper_structure(
        &self,
        item_key: &str,
        attachment_key: Option<&str>,
    ) -> Result<PaperStructure> {
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
            let fulltext = self.backend.get_pdf_text(&attachment_key).await?;
            let source = req
                .source_label
                .unwrap_or_else(|| format!("attachment:{attachment_key}"));
            return Ok(pdf::prepare_vox_payload_from_fulltext(
                &source, &fulltext, max_chars,
            ));
        }

        Err(ZoteroMcpError::InvalidInput(
            "Provide either 'text' or 'attachment_key'".to_string(),
        ))
    }

    pub async fn prepare_item_for_vox(
        &self,
        req: PrepareItemForVoxRequest,
    ) -> Result<ItemVoxPayload> {
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

        let result_index = req.result_index.unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
    use crate::models::{ListCollectionsQuery, SearchItemsQuery};

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
