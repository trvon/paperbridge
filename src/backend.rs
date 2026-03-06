use crate::error::Result;
use crate::models::{
    CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest, DeleteCollectionRequest,
    DeleteItemRequest, FulltextContent, ItemDetail, ItemSummary, ItemUpdateRequest,
    ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BackendMode {
    Cloud,
    Local,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct BackendCapabilities {
    pub read_library: bool,
    pub write_basic: bool,
    pub file_upload: bool,
    pub group_libraries: bool,
}

impl BackendCapabilities {
    pub const fn read_only_cloud() -> Self {
        Self {
            read_library: true,
            write_basic: true,
            file_upload: false,
            group_libraries: true,
        }
    }

    pub const fn read_only_local() -> Self {
        Self {
            read_library: true,
            write_basic: false,
            file_upload: false,
            group_libraries: false,
        }
    }
}

#[async_trait::async_trait]
pub trait LibraryBackend: Send + Sync {
    fn mode(&self) -> BackendMode;

    fn capabilities(&self) -> BackendCapabilities;

    async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>>;

    async fn list_collections(&self, query: ListCollectionsQuery)
    -> Result<Vec<CollectionSummary>>;

    async fn get_item(&self, key: &str) -> Result<ItemDetail>;

    async fn get_item_fulltext(&self, key: &str) -> Result<FulltextContent>;

    async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent>;

    async fn create_collection(&self, req: CollectionWriteRequest) -> Result<CollectionSummary>;

    async fn update_collection(&self, req: CollectionUpdateRequest) -> Result<CollectionSummary>;

    async fn delete_collection(&self, req: DeleteCollectionRequest) -> Result<()>;

    async fn create_item(&self, req: ItemWriteRequest) -> Result<ItemDetail>;

    async fn update_item(&self, req: ItemUpdateRequest) -> Result<ItemDetail>;

    async fn delete_item(&self, req: DeleteItemRequest) -> Result<()>;
}
