use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    FulltextContent, ItemDetail, ItemSummary, ItemVoxPayload, ListCollectionsQuery,
    SearchItemsQuery, SearchVoxPayload, VoxTextPayload,
};
use crate::pdf;
use crate::zotero_api::ZoteroApiClient;
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
    api: Arc<ZoteroApiClient>,
}

impl PaperbridgeService {
    pub fn new(api: ZoteroApiClient) -> Self {
        Self { api: Arc::new(api) }
    }

    pub async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
        self.api.search_items(query).await
    }

    pub async fn list_collections(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<Vec<crate::models::CollectionSummary>> {
        self.api.list_collections(query).await
    }

    pub async fn get_item(&self, key: &str) -> Result<ItemDetail> {
        self.api.get_item(key).await
    }

    pub async fn get_item_fulltext(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.api.get_item_fulltext(attachment_key).await
    }

    pub async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.api.get_pdf_text(attachment_key).await
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
            let fulltext = self.api.get_pdf_text(&attachment_key).await?;
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
        let item = self.api.get_item(&req.item_key).await?;

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

        let fulltext = self.api.get_pdf_text(&attachment.key).await?;
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
