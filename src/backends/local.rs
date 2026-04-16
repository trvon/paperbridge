use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::backends::cloud::CloudZoteroBackend;
use crate::config::Config;
use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest, DeleteCollectionRequest,
    DeleteItemRequest, FulltextContent, ItemDetail, ItemSummary, ItemUpdateRequest,
    ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
};

#[derive(Clone)]
pub struct LocalZoteroBackend {
    inner: CloudZoteroBackend,
}

impl LocalZoteroBackend {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            inner: CloudZoteroBackend::new(config)?,
        })
    }
}

#[async_trait::async_trait]
impl LibraryBackend for LocalZoteroBackend {
    fn mode(&self) -> BackendMode {
        BackendMode::Local
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::read_only_local()
    }

    async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
        self.inner.search_items(query).await
    }

    async fn list_collections(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<Vec<CollectionSummary>> {
        self.inner.list_collections(query).await
    }

    async fn get_item(&self, key: &str) -> Result<ItemDetail> {
        self.inner.get_item(key).await
    }

    async fn get_item_fulltext(&self, key: &str) -> Result<FulltextContent> {
        self.inner.get_item_fulltext(key).await
    }

    async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.inner.get_pdf_text(attachment_key).await
    }

    async fn get_attachment_bytes(&self, attachment_key: &str) -> Result<Vec<u8>> {
        self.inner.get_attachment_bytes(attachment_key).await
    }

    async fn create_collection(&self, _req: CollectionWriteRequest) -> Result<CollectionSummary> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }

    async fn update_collection(&self, _req: CollectionUpdateRequest) -> Result<CollectionSummary> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }

    async fn delete_collection(&self, _req: DeleteCollectionRequest) -> Result<()> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }

    async fn create_item(&self, _req: ItemWriteRequest) -> Result<ItemDetail> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }

    async fn update_item(&self, _req: ItemUpdateRequest) -> Result<ItemDetail> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }

    async fn delete_item(&self, _req: DeleteItemRequest) -> Result<()> {
        Err(ZoteroMcpError::InvalidInput(
            "local backend write support is not implemented yet".to_string(),
        ))
    }
}
