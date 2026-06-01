use crate::external::SearchOptions;
use crate::models::{
    CollectionUpdateRequest, CollectionWriteRequest, DeleteCollectionRequest, DeleteItemRequest,
    ItemUpdateRequest, ItemWriteRequest, ListCollectionsQuery, PaperSource, SearchItemsQuery,
};
use crate::service::{
    DEFAULT_CHUNK_SIZE, DEFAULT_PIPELINE_SEARCH_LIMIT, PaperbridgeService,
    PrepareItemForVoxRequest, PrepareSearchResultForVoxRequest, PrepareVoxTextRequest,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, GetPromptRequestParams, GetPromptResult, ListPromptsResult,
    PaginatedRequestParams, Prompt, PromptMessage, PromptMessageRole, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::task_manager::OperationProcessor;
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

pub const SKILL_MD: &str = include_str!("../docs/skill.md");
const SKILL_PROMPT_NAME: &str = "paperbridge_skill";

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
pub struct GetPaperStructureParams {
    #[schemars(description = "Zotero item key for the paper")]
    pub item_key: String,

    #[schemars(
        description = "Optional attachment key. If omitted, paperbridge picks the best PDF attachment."
    )]
    pub attachment_key: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct QueryPaperParams {
    #[schemars(description = "Zotero item key for the paper")]
    pub item_key: String,

    #[schemars(
        description = "Dotted-path selector against the PaperStructure JSON. Examples: 'metadata.title', 'sections[0].heading', 'references[3].doi'."
    )]
    pub selector: String,

    #[schemars(description = "Optional attachment key override")]
    pub attachment_key: Option<String>,
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

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateCollectionParams {
    #[schemars(description = "Collection name")]
    pub name: String,

    #[schemars(description = "Optional parent collection key")]
    pub parent_collection: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ResolveDoiParams {
    #[schemars(description = "DOI string to resolve (e.g. 10.1038/nature12373)")]
    pub doi: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ValidateItemParams {
    #[schemars(description = "Item payload to validate")]
    pub item: ItemWriteRequest,

    #[schemars(
        description = "If true, also validate DOI against Crossref (slower, requires network)"
    )]
    pub online: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateItemParams {
    #[schemars(description = "Item payload to create")]
    pub item: ItemWriteRequest,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UpdateCollectionParams {
    #[schemars(description = "Collection payload to update")]
    pub collection: CollectionUpdateRequest,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UpdateItemParams {
    #[schemars(description = "Item payload to update")]
    pub item: ItemUpdateRequest,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BackendInfoParams {}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeleteCollectionParams {
    #[schemars(description = "Collection deletion payload")]
    pub collection: DeleteCollectionRequest,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeleteItemParams {
    #[schemars(description = "Item deletion payload")]
    pub item: DeleteItemRequest,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchPapersParams {
    #[schemars(description = "Free-text search query")]
    pub query: String,

    #[schemars(description = "Max hits per source (default 10)")]
    pub limit_per_source: Option<u32>,

    #[schemars(
        description = "Optional scoping to specific sources; defaults to all enabled sources (arxiv, crossref, openalex, europe_pmc, dblp, openreview, pubmed, hugging_face, semantic_scholar, core, ads)"
    )]
    pub sources: Option<Vec<PaperSource>>,

    #[schemars(description = "Per-source timeout in milliseconds (default 8000)")]
    pub timeout_ms: Option<u64>,

    #[schemars(description = "Zero-based offset into the merged result list (default 0)")]
    pub offset: Option<u32>,

    #[schemars(description = "Maximum results to return; 0 means all (default 0)")]
    pub limit: Option<u32>,
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
        description = "Get indexed full-text content for a Zotero attachment key. Falls back to searching the local Paperseed cache by the attachment key as a natural-language query when the backend is unavailable."
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
        description = "Get PDF text for a Zotero attachment key (via Zotero full-text index). Falls back to searching the local Paperseed cache by the attachment key as a natural-language query when the backend is unavailable."
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
        name = "get_paper_structure",
        description = "Return a structured tree for a paper in the Zotero library (metadata, sections, references, figures). Without GROBID, Zotero indexed fulltext is split best-effort into common paper sections such as Abstract, Design, Evaluation, Results, and Conclusion; otherwise the body is returned as one section."
    )]
    async fn get_paper_structure(
        &self,
        Parameters(params): Parameters<GetPaperStructureParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let structure = self
            .service
            .get_paper_structure(&params.item_key, params.attachment_key.as_deref())
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&structure)
    }

    #[tool(
        name = "query_paper",
        description = "Evaluate a dotted-path selector against PaperStructure and return the matching subtree. Top-level keys: item_key, attachment_key, metadata, sections, references, figures, source. metadata sub-keys: title, authors, abstract, doi, year. Section sub-keys include id, heading, kind, level, text. Examples: 'metadata.title', 'metadata.abstract', 'sections[0].heading', 'sections[2].kind', 'references[3].doi'."
    )]
    async fn query_paper(
        &self,
        Parameters(params): Parameters<QueryPaperParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let value = self
            .service
            .query_paper(
                &params.item_key,
                &params.selector,
                params.attachment_key.as_deref(),
            )
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&value)
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

    #[tool(
        name = "create_collection",
        description = "Create a Zotero collection when backend write support is available"
    )]
    async fn create_collection(
        &self,
        Parameters(params): Parameters<CreateCollectionParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let created = self
            .service
            .create_collection(CollectionWriteRequest {
                name: params.name,
                parent_collection: params.parent_collection,
            })
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&created)
    }

    #[tool(
        name = "resolve_doi",
        description = "Resolve a DOI via Crossref and return structured citation metadata (title, authors, year, journal, abstract)"
    )]
    async fn resolve_doi(
        &self,
        Parameters(params): Parameters<ResolveDoiParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let work = self
            .service
            .resolve_doi(&params.doi)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&work)
    }

    #[tool(
        name = "validate_item",
        description = "Validate a Zotero item payload before attempting a write. Set online=true to also cross-check DOI metadata against Crossref."
    )]
    async fn validate_item(
        &self,
        Parameters(params): Parameters<ValidateItemParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let report = if params.online.unwrap_or(false) {
            self.service
                .validate_item_online(&params.item)
                .await
                .map_err(Self::map_error)?
        } else {
            self.service.validate_item_request(&params.item)
        };
        Self::ok_json(&report)
    }

    #[tool(
        name = "create_item",
        description = "Create a Zotero item when backend write support is available"
    )]
    async fn create_item(
        &self,
        Parameters(params): Parameters<CreateItemParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let created = self
            .service
            .create_item(params.item)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&created)
    }

    #[tool(
        name = "update_collection",
        description = "Update a Zotero collection when backend write support is available"
    )]
    async fn update_collection(
        &self,
        Parameters(params): Parameters<UpdateCollectionParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let updated = self
            .service
            .update_collection(params.collection)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&updated)
    }

    #[tool(
        name = "update_item",
        description = "Update a Zotero item when backend write support is available"
    )]
    async fn update_item(
        &self,
        Parameters(params): Parameters<UpdateItemParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let updated = self
            .service
            .update_item(params.item)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&updated)
    }

    #[tool(
        name = "backend_info",
        description = "Show active backend mode and current capability flags"
    )]
    async fn backend_info(
        &self,
        Parameters(_params): Parameters<BackendInfoParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        Self::ok_json(&self.service.backend_info())
    }

    #[tool(
        name = "delete_collection",
        description = "Delete a Zotero collection when backend write support is available"
    )]
    async fn delete_collection(
        &self,
        Parameters(params): Parameters<DeleteCollectionParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        self.service
            .delete_collection(params.collection)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&serde_json::json!({"deleted": true}))
    }

    #[tool(
        name = "delete_item",
        description = "Delete a Zotero item when backend write support is available"
    )]
    async fn delete_item(
        &self,
        Parameters(params): Parameters<DeleteItemParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        self.service
            .delete_item(params.item)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&serde_json::json!({"deleted": true}))
    }

    #[tool(
        name = "search_papers",
        description = "Search external paper sources (arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed, HuggingFace, Semantic Scholar, CORE, NASA ADS, ScholarAPI) and the local Paperseed cache in parallel. Returns paginated results with total_count, offset, limit, and hits. Locally cached papers are prioritized first."
    )]
    async fn search_papers(
        &self,
        Parameters(params): Parameters<SearchPapersParams>,
    ) -> std::result::Result<CallToolResult, McpError> {
        let opts = SearchOptions {
            query: params.query,
            limit_per_source: params.limit_per_source.unwrap_or(10),
            sources: params.sources,
            timeout_ms: params.timeout_ms.unwrap_or(8000),
            offset: params.offset.unwrap_or(0),
            limit: params.limit.unwrap_or(0),
        };
        let result = self
            .service
            .search_papers(opts)
            .await
            .map_err(Self::map_error)?;
        Self::ok_json(&result)
    }
}

#[tool_handler]
#[allow(deprecated)]
impl ServerHandler for PaperbridgeServer {
    fn get_info(&self) -> ServerInfo {
        let _ = &self.processor;
        let _ = &self.tool_router;

        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .build(),
        )
        .with_protocol_version(rmcp::model::ProtocolVersion::V_2024_11_05)
        .with_server_info(rmcp::model::Implementation::from_build_env())
        .with_instructions(
            "Search Zotero libraries, retrieve full-text content, search external paper sources, and access the local Paperseed cache. Use prepare_vox_text to build read-aloud chunks for Vox. Fetch the prompt 'paperbridge_skill' for the full operating guide.",
        )
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            SKILL_PROMPT_NAME,
            Some(
                "Operating guide for the paperbridge MCP server (canonical CLI recipes, config keys, gotchas)",
            ),
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        if request.name != SKILL_PROMPT_NAME {
            return Err(McpError::invalid_params(
                format!("unknown prompt '{}'", request.name),
                None,
            ));
        }
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::User,
            SKILL_MD,
        )])
        .with_description("paperbridge operating guide"))
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

    #[test]
    fn skill_is_embedded_with_stable_sentinel() {
        // include_str! pulls SKILL.md into the binary at compile time; this asserts the
        // canonical opening sentence stays present so connected hosts get a usable guide.
        assert!(
            SKILL_MD.contains("Rust CLI + MCP server bridging Zotero"),
            "embedded SKILL.md missing canonical opening sentence"
        );
        assert!(SKILL_MD.contains("paperbridge library query"));
        assert!(SKILL_MD.contains("paperbridge papers search"));
    }

    #[test]
    fn skill_prompt_name_is_stable() {
        assert_eq!(SKILL_PROMPT_NAME, "paperbridge_skill");
    }

    #[test]
    fn skill_prompt_messages_carry_user_role_text() {
        let msg = PromptMessage::new_text(PromptMessageRole::User, SKILL_MD);
        match msg.content {
            rmcp::model::PromptMessageContent::Text { text } => {
                assert!(text.contains("paperbridge"));
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    #[test]
    fn search_papers_params_deserializes() {
        let json = serde_json::json!({"query": "transformers"});
        let params: SearchPapersParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.query, "transformers");
        assert!(params.limit_per_source.is_none());
        assert!(params.sources.is_none());
        assert!(params.timeout_ms.is_none());

        let json = serde_json::json!({
            "query": "q",
            "limit_per_source": 3,
            "sources": ["arxiv", "crossref"],
            "timeout_ms": 5000
        });
        let params: SearchPapersParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.limit_per_source, Some(3));
        assert_eq!(params.timeout_ms, Some(5000));
        assert_eq!(
            params.sources,
            Some(vec![PaperSource::Arxiv, PaperSource::Crossref])
        );
    }

    // ---- Phase B2: MCP handler round-trip coverage ----

    use crate::config::{BackendModeConfig, Config, LibraryType};
    use crate::models::{ItemDetail, ItemSummary};
    use crate::service::PaperbridgeService;
    use crate::zotero_api::build_backend;
    use serde::de::DeserializeOwned;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cloud_test_config(api_base: String) -> Config {
        Config {
            backend_mode: BackendModeConfig::Cloud,
            cloud_api_base: api_base,
            local_api_base: "http://127.0.0.1:23119/api".to_string(),
            user_id: Some(123),
            library_type: LibraryType::User,
            ..Config::default()
        }
    }

    /// Spin up a Zotero cloud mock with the minimum endpoints needed by the
    /// read-side MCP handlers, wrap it in PaperbridgeServer, and return both
    /// so individual tests can assert on the response shape.
    async fn server_with_mocked_cloud() -> (PaperbridgeServer, MockServer) {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "key": "ITEMA",
                    "data": {
                        "itemType": "journalArticle",
                        "title": "Graph Learning at Scale",
                        "date": "2024-08-01",
                        "creators": [{"firstName": "Grace", "lastName": "Hopper"}],
                        "url": "https://example.org/graph"
                    }
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/users/123/collections/top"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "key": "COLL1",
                    "data": {"name": "Research", "parentCollection": null},
                    "meta": {"numItems": 7}
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/users/123/items/ITEMA"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "ITEMA",
                "data": {
                    "itemType": "journalArticle",
                    "title": "Graph Learning at Scale",
                    "date": "2024-08-01",
                    "abstractNote": "A practical systems paper.",
                    "creators": [{"firstName": "Grace", "lastName": "Hopper"}],
                    "url": "https://example.org/graph"
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/users/123/items/ITEMA/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "key": "PDFA",
                    "data": {
                        "itemType": "attachment",
                        "title": "Paper PDF",
                        "contentType": "application/pdf",
                        "path": "storage:paper.pdf"
                    }
                }
            ])))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/users/123/items/PDFA/fulltext"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": "Abstract\nA practical systems paper.\nIntroduction\nFirst sentence.\nEvaluation\nSecond sentence.",
                "indexedPages": 2,
                "totalPages": 2,
                "indexedChars": 92,
                "totalChars": 92
            })))
            .mount(&server)
            .await;

        let backend = build_backend(cloud_test_config(server.uri())).unwrap();
        let service = PaperbridgeService::new(backend);
        (PaperbridgeServer::new(service), server)
    }

    /// Parse the JSON payload out of a successful CallToolResult.
    fn parse_call_tool_result<T: DeserializeOwned>(result: &CallToolResult) -> T {
        let first = result
            .content
            .first()
            .expect("CallToolResult should contain at least one content item");
        let text = match &first.raw {
            rmcp::model::RawContent::Text(text_content) => text_content.text.as_str(),
            other => panic!("expected text content, got {other:?}"),
        };
        serde_json::from_str(text).expect("CallToolResult payload should be JSON")
    }

    #[tokio::test]
    async fn search_items_handler_returns_mocked_results() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .search_items(Parameters(SearchItemsParams {
                q: Some("graph".to_string()),
                qmode: None,
                item_type: None,
                tag: None,
                limit: Some(10),
                start: None,
            }))
            .await
            .unwrap();
        let items: Vec<ItemSummary> = parse_call_tool_result(&result);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "ITEMA");
    }

    #[tokio::test]
    async fn list_collections_handler_returns_mocked_results() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .list_collections(Parameters(ListCollectionsParams {
                top_only: Some(true),
                limit: None,
                start: None,
            }))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        let arr = json.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["key"], "COLL1");
    }

    #[tokio::test]
    async fn get_item_handler_round_trips_through_service() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .get_item(Parameters(GetItemParams {
                key: "ITEMA".to_string(),
            }))
            .await
            .unwrap();
        let item: ItemDetail = parse_call_tool_result(&result);
        assert_eq!(item.key, "ITEMA");
        assert_eq!(item.title, "Graph Learning at Scale");
    }

    #[tokio::test]
    async fn get_item_fulltext_handler_returns_content() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .get_item_fulltext(Parameters(GetItemFulltextParams {
                attachment_key: "PDFA".to_string(),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        assert!(
            json["content"]
                .as_str()
                .unwrap_or("")
                .contains("First sentence")
        );
    }

    #[tokio::test]
    async fn backend_info_handler_reports_cloud_mode() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .backend_info(Parameters(BackendInfoParams {}))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        assert_eq!(json["mode"], "cloud");
        assert_eq!(json["read_library"], true);
    }

    #[tokio::test]
    async fn validate_item_handler_flags_missing_title() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .validate_item(Parameters(ValidateItemParams {
                item: ItemWriteRequest {
                    item_type: "journalArticle".to_string(),
                    title: None,
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
                },
                online: Some(false),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        assert_eq!(json["valid"], false);
        assert!(!json["issues"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn prepare_vox_text_handler_chunks_inline_text() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .prepare_vox_text(Parameters(PrepareVoxTextParams {
                text: Some("inline content for vox handler".to_string()),
                attachment_key: None,
                source_label: Some("test".to_string()),
                max_chars_per_chunk: Some(8),
            }))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        assert_eq!(json["source"], "test");
        assert!(json["chunk_count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn get_paper_structure_handler_returns_structured_json() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .get_paper_structure(Parameters(GetPaperStructureParams {
                item_key: "ITEMA".to_string(),
                attachment_key: None,
            }))
            .await
            .unwrap();
        let json: serde_json::Value = parse_call_tool_result(&result);
        let sections = json["sections"].as_array().expect("sections array");
        assert_eq!(json["metadata"]["title"], "Graph Learning at Scale");
        assert_eq!(sections[0]["heading"], "Abstract");
        assert_eq!(sections[1]["heading"], "Introduction");
        assert_eq!(sections[2]["kind"], "evaluation");
    }

    #[tokio::test]
    async fn query_paper_handler_returns_section_kind() {
        let (srv, _mock) = server_with_mocked_cloud().await;
        let result = srv
            .query_paper(Parameters(QueryPaperParams {
                item_key: "ITEMA".to_string(),
                selector: "sections[2].kind".to_string(),
                attachment_key: None,
            }))
            .await
            .unwrap();
        let value: serde_json::Value = parse_call_tool_result(&result);
        assert_eq!(value, serde_json::Value::String("evaluation".to_string()));
    }

    #[tokio::test]
    async fn create_item_handler_rejects_unsupported_write_with_invalid_params() {
        // Local backend doesn't support writes — the handler must surface
        // ZoteroMcpError::InvalidInput as an MCP "invalid params" error so
        // clients can distinguish capability errors from server faults.
        use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
        use std::sync::Arc;

        struct ReadOnlyStub;
        #[async_trait::async_trait]
        impl LibraryBackend for ReadOnlyStub {
            fn mode(&self) -> BackendMode {
                BackendMode::Local
            }
            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities::read_only_local()
            }
            async fn search_items(
                &self,
                _: crate::models::SearchItemsQuery,
            ) -> crate::Result<Vec<crate::models::ItemSummary>> {
                Ok(vec![])
            }
            async fn list_collections(
                &self,
                _: crate::models::ListCollectionsQuery,
            ) -> crate::Result<Vec<crate::models::CollectionSummary>> {
                Ok(vec![])
            }
            async fn get_item(&self, _: &str) -> crate::Result<ItemDetail> {
                Err(ZoteroMcpError::InvalidInput("unused".into()))
            }
            async fn get_item_fulltext(
                &self,
                _: &str,
            ) -> crate::Result<crate::models::FulltextContent> {
                Err(ZoteroMcpError::InvalidInput("unused".into()))
            }
            async fn get_pdf_text(&self, _: &str) -> crate::Result<crate::models::FulltextContent> {
                Err(ZoteroMcpError::InvalidInput("unused".into()))
            }
            async fn get_attachment_bytes(&self, _: &str) -> crate::Result<Vec<u8>> {
                Err(ZoteroMcpError::InvalidInput("unused".into()))
            }
            async fn create_collection(
                &self,
                _: crate::models::CollectionWriteRequest,
            ) -> crate::Result<crate::models::CollectionSummary> {
                panic!("not reached: handler must gate on capabilities first")
            }
            async fn update_collection(
                &self,
                _: crate::models::CollectionUpdateRequest,
            ) -> crate::Result<crate::models::CollectionSummary> {
                panic!("not reached")
            }
            async fn delete_collection(
                &self,
                _: crate::models::DeleteCollectionRequest,
            ) -> crate::Result<()> {
                panic!("not reached")
            }
            async fn create_item(&self, _: ItemWriteRequest) -> crate::Result<ItemDetail> {
                panic!("not reached")
            }
            async fn update_item(
                &self,
                _: crate::models::ItemUpdateRequest,
            ) -> crate::Result<ItemDetail> {
                panic!("not reached")
            }
            async fn delete_item(&self, _: crate::models::DeleteItemRequest) -> crate::Result<()> {
                panic!("not reached")
            }
        }

        let srv = PaperbridgeServer::new(PaperbridgeService::new(Arc::new(ReadOnlyStub)));
        let err = srv
            .create_item(Parameters(CreateItemParams {
                item: ItemWriteRequest {
                    item_type: "journalArticle".to_string(),
                    title: Some("Test".to_string()),
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
                },
            }))
            .await
            .unwrap_err();
        // McpError surfaces the underlying message; "local backend" string
        // confirms `ensure_write_supported` triggered the InvalidInput path.
        assert!(format!("{err}").contains("local backend"));
    }
}
