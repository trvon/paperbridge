use crate::models::{ListCollectionsQuery, SearchItemsQuery};
use crate::service::{
    DEFAULT_CHUNK_SIZE, DEFAULT_PIPELINE_SEARCH_LIMIT, PaperbridgeService,
    PrepareItemForVoxRequest, PrepareSearchResultForVoxRequest, PrepareVoxTextRequest,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::task_manager::OperationProcessor;
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchItemsParams {
    #[schemars(description = "Quick search query")]
    pub q: Option<String>,

    #[schemars(description = "Query mode (e.g. titleCreatorYear, everything)")]
    pub qmode: Option<String>,

    #[schemars(description = "Item type filter (e.g. journalArticle)")]
    pub item_type: Option<String>,

    #[schemars(description = "Tag filter")]
    pub tag: Option<String>,

    #[schemars(description = "Result limit (1-100, default 25)")]
    pub limit: Option<u32>,

    #[schemars(description = "Pagination start index (default 0)")]
    pub start: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListCollectionsParams {
    #[schemars(description = "If true, list only top-level collections")]
    pub top_only: Option<bool>,

    #[schemars(description = "Result limit (1-100, default 50)")]
    pub limit: Option<u32>,

    #[schemars(description = "Pagination start index (default 0)")]
    pub start: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetItemParams {
    #[schemars(description = "Zotero item key")]
    pub key: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetItemFulltextParams {
    #[schemars(description = "Attachment item key")]
    pub attachment_key: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PrepareVoxTextParams {
    #[schemars(description = "Raw text to split for Vox")]
    pub text: Option<String>,

    #[schemars(description = "Attachment key to fetch fulltext from Zotero")]
    pub attachment_key: Option<String>,

    #[schemars(description = "Optional source label")]
    pub source_label: Option<String>,

    #[schemars(description = "Maximum characters per chunk (default 1200)")]
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PrepareItemForVoxParams {
    #[schemars(description = "Zotero item key")]
    pub item_key: String,

    #[schemars(description = "Optional specific attachment key to use")]
    pub attachment_key: Option<String>,

    #[schemars(description = "Maximum characters per chunk (default 1200)")]
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PrepareSearchResultForVoxParams {
    #[schemars(description = "Search query")]
    pub q: String,

    #[schemars(description = "Query mode (e.g. titleCreatorYear, everything)")]
    pub qmode: Option<String>,

    #[schemars(description = "Item type filter (e.g. journalArticle)")]
    pub item_type: Option<String>,

    #[schemars(description = "Tag filter")]
    pub tag: Option<String>,

    #[schemars(description = "0-based index within search results (default 0)")]
    pub result_index: Option<usize>,

    #[schemars(description = "How many items to fetch from search (default 5)")]
    pub search_limit: Option<u32>,

    #[schemars(description = "Maximum characters per chunk (default 1200)")]
    pub max_chars_per_chunk: Option<usize>,
}

#[derive(Clone)]
pub struct PaperbridgeServer {
    service: Arc<PaperbridgeService>,
    processor: Arc<TokioMutex<OperationProcessor>>,
    tool_router: ToolRouter<Self>,
}

impl PaperbridgeServer {
    pub fn new(service: PaperbridgeService) -> Self {
        Self {
            service: Arc::new(service),
            processor: Arc::new(TokioMutex::new(OperationProcessor::new())),
            tool_router: Self::tool_router(),
        }
    }

    fn ok_json<T: Serialize>(value: &T) -> std::result::Result<CallToolResult, McpError> {
        let json = serde_json::to_string_pretty(value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    fn map_error(err: crate::ZoteroMcpError) -> McpError {
        match err {
            crate::ZoteroMcpError::InvalidInput(msg) => McpError::invalid_params(msg, None),
            other => McpError::internal_error(other.to_string(), None),
        }
    }
}

#[tool_router]
impl PaperbridgeServer {
    #[tool(
        name = "search_items",
        description = "Search items in the configured Zotero library by query and filters"
    )]
    async fn search_items(
        &self,
        Parameters(params): Parameters<SearchItemsParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let query = SearchItemsQuery {
            q: params.q,
            qmode: params.qmode,
            item_type: params.item_type,
            tag: params.tag,
            limit: params.limit.unwrap_or(25),
            start: params.start.unwrap_or(0),
        };
        let results = self
            .service
            .search_items(query)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&results)
    }

    #[tool(
        name = "list_collections",
        description = "List collections in the configured Zotero library"
    )]
    async fn list_collections(
        &self,
        Parameters(params): Parameters<ListCollectionsParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let results = self
            .service
            .list_collections(ListCollectionsQuery {
                top_only: params.top_only.unwrap_or(false),
                limit: params.limit.unwrap_or(50),
                start: params.start.unwrap_or(0),
            })
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&results)
    }

    #[tool(
        name = "get_item",
        description = "Get one Zotero item with metadata and attachment references"
    )]
    async fn get_item(
        &self,
        Parameters(params): Parameters<GetItemParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let item = self
            .service
            .get_item(&params.key)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&item)
    }

    #[tool(
        name = "get_item_fulltext",
        description = "Get indexed full-text content for a Zotero attachment key"
    )]
    async fn get_item_fulltext(
        &self,
        Parameters(params): Parameters<GetItemFulltextParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let text = self
            .service
            .get_item_fulltext(&params.attachment_key)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&text)
    }

    #[tool(
        name = "get_pdf_text",
        description = "Get PDF text for a Zotero attachment key (via Zotero full-text index)"
    )]
    async fn get_pdf_text(
        &self,
        Parameters(params): Parameters<GetItemFulltextParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let text = self
            .service
            .get_pdf_text(&params.attachment_key)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&text)
    }

    #[tool(
        name = "prepare_vox_text",
        description = "Prepare normalized text chunks for Vox read-aloud without calling Vox directly"
    )]
    async fn prepare_vox_text(
        &self,
        Parameters(params): Parameters<PrepareVoxTextParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let payload = self
            .service
            .prepare_vox_text(PrepareVoxTextRequest {
                text: params.text,
                attachment_key: params.attachment_key,
                source_label: params.source_label,
                max_chars_per_chunk: params.max_chars_per_chunk.or(Some(DEFAULT_CHUNK_SIZE)),
            })
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&payload)
    }

    #[tool(
        name = "prepare_item_for_vox",
        description = "Select an attachment for a Zotero item, fetch text, and return Vox-ready chunks"
    )]
    async fn prepare_item_for_vox(
        &self,
        Parameters(params): Parameters<PrepareItemForVoxParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let payload = self
            .service
            .prepare_item_for_vox(PrepareItemForVoxRequest {
                item_key: params.item_key,
                attachment_key: params.attachment_key,
                max_chars_per_chunk: params.max_chars_per_chunk.or(Some(DEFAULT_CHUNK_SIZE)),
            })
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&payload)
    }

    #[tool(
        name = "prepare_search_result_for_vox",
        description = "Search Zotero, pick one result, and return Vox-ready chunks from its best attachment"
    )]
    async fn prepare_search_result_for_vox(
        &self,
        Parameters(params): Parameters<PrepareSearchResultForVoxParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let payload = self
            .service
            .prepare_search_result_for_vox(PrepareSearchResultForVoxRequest {
                q: params.q,
                qmode: params.qmode,
                item_type: params.item_type,
                tag: params.tag,
                result_index: params.result_index,
                search_limit: params.search_limit.or(Some(DEFAULT_PIPELINE_SEARCH_LIMIT)),
                max_chars_per_chunk: params.max_chars_per_chunk.or(Some(DEFAULT_CHUNK_SIZE)),
            })
            .await
            .map_err(Self::map_error)?;

        Self::ok_json(&payload)
    }
}

#[tool_handler]
#[allow(deprecated)]
impl ServerHandler for PaperbridgeServer {
    fn get_info(&self) -> ServerInfo {
        let _ = &self.processor;
        let _ = &self.tool_router;

        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: rmcp::model::Implementation::from_build_env(),
            instructions: Some(
                "Search Zotero libraries and retrieve full-text content. Use prepare_vox_text to build read-aloud chunks for Vox."
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ZoteroMcpError;

    #[test]
    fn search_params_defaults_to_none() {
        let json = serde_json::json!({});
        let params: SearchItemsParams = serde_json::from_value(json).unwrap();
        assert!(params.limit.is_none());
        assert!(params.q.is_none());
    }

    #[test]
    fn list_collections_params_defaults_to_none() {
        let json = serde_json::json!({});
        let params: ListCollectionsParams = serde_json::from_value(json).unwrap();
        assert!(params.top_only.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn prepare_item_for_vox_params_deserializes() {
        let json = serde_json::json!({
            "item_key": "ITEM123",
            "attachment_key": "ATTACH456",
            "max_chars_per_chunk": 800
        });
        let params: PrepareItemForVoxParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.item_key, "ITEM123");
        assert_eq!(params.attachment_key.as_deref(), Some("ATTACH456"));
        assert_eq!(params.max_chars_per_chunk, Some(800));
    }

    #[test]
    fn map_error_uses_invalid_params_for_input_errors() {
        let err = PaperbridgeServer::map_error(ZoteroMcpError::InvalidInput("bad".to_string()));
        let rendered = format!("{err}");
        assert!(rendered.contains("bad"));
    }
}
