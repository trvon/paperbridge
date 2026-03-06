use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    BackendInfo, CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest,
    DeleteCollectionRequest, DeleteItemRequest, FulltextContent, ItemDetail, ItemSummary,
    ItemUpdateRequest, ItemVoxPayload, ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
    SearchVoxPayload, ValidationReport, VoxTextPayload,
};
use crate::pdf;
use crate::validation;
use std::sync::Arc;

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

#[derive(Clone)]
pub struct PaperbridgeService {
    backend: Arc<dyn LibraryBackend>,
}

impl PaperbridgeService {
    pub fn new(backend: Arc<dyn LibraryBackend>) -> Self {
        Self { backend }
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
