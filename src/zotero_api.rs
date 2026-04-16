use crate::backend::{BackendMode, LibraryBackend};
use crate::backends::cloud::CloudZoteroBackend;
use crate::backends::local::LocalZoteroBackend;
use crate::config::{BackendModeConfig, Config};
use crate::error::Result;
use std::sync::Arc;

pub fn build_backend(config: Config) -> Result<Arc<dyn LibraryBackend>> {
    match config.backend_mode {
        BackendModeConfig::Cloud => Ok(Arc::new(CloudZoteroBackend::new(config)?)),
        BackendModeConfig::Local => Ok(Arc::new(LocalZoteroBackend::new(config)?)),
        BackendModeConfig::Hybrid => Ok(Arc::new(HybridZoteroBackend::new(config)?)),
    }
}

pub fn detect_backend_mode(config: &Config) -> BackendMode {
    match config.backend_mode {
        BackendModeConfig::Cloud => BackendMode::Cloud,
        BackendModeConfig::Local => BackendMode::Local,
        BackendModeConfig::Hybrid => BackendMode::Hybrid,
    }
}

#[derive(Clone)]
struct HybridZoteroBackend {
    read_backend: Arc<dyn LibraryBackend>,
    write_backend: Arc<dyn LibraryBackend>,
}

impl HybridZoteroBackend {
    fn new(config: Config) -> Result<Self> {
        let mut local_cfg = config.clone();
        local_cfg.backend_mode = BackendModeConfig::Local;
        let mut cloud_cfg = config;
        cloud_cfg.backend_mode = BackendModeConfig::Cloud;

        Ok(Self {
            read_backend: Arc::new(LocalZoteroBackend::new(local_cfg)?),
            write_backend: Arc::new(CloudZoteroBackend::new(cloud_cfg)?),
        })
    }
}

#[async_trait::async_trait]
impl LibraryBackend for HybridZoteroBackend {
    fn mode(&self) -> BackendMode {
        BackendMode::Hybrid
    }

    fn capabilities(&self) -> crate::backend::BackendCapabilities {
        let mut caps = self.write_backend.capabilities();
        caps.read_library = self.read_backend.capabilities().read_library;
        caps
    }

    async fn search_items(
        &self,
        query: crate::models::SearchItemsQuery,
    ) -> Result<Vec<crate::models::ItemSummary>> {
        self.read_backend.search_items(query).await
    }

    async fn list_collections(
        &self,
        query: crate::models::ListCollectionsQuery,
    ) -> Result<Vec<crate::models::CollectionSummary>> {
        self.read_backend.list_collections(query).await
    }

    async fn get_item(&self, key: &str) -> Result<crate::models::ItemDetail> {
        self.read_backend.get_item(key).await
    }

    async fn get_item_fulltext(&self, key: &str) -> Result<crate::models::FulltextContent> {
        self.read_backend.get_item_fulltext(key).await
    }

    async fn get_pdf_text(&self, attachment_key: &str) -> Result<crate::models::FulltextContent> {
        self.read_backend.get_pdf_text(attachment_key).await
    }

    async fn get_attachment_bytes(&self, attachment_key: &str) -> Result<Vec<u8>> {
        self.read_backend.get_attachment_bytes(attachment_key).await
    }

    async fn create_collection(
        &self,
        req: crate::models::CollectionWriteRequest,
    ) -> Result<crate::models::CollectionSummary> {
        self.write_backend.create_collection(req).await
    }

    async fn update_collection(
        &self,
        req: crate::models::CollectionUpdateRequest,
    ) -> Result<crate::models::CollectionSummary> {
        self.write_backend.update_collection(req).await
    }

    async fn delete_collection(&self, req: crate::models::DeleteCollectionRequest) -> Result<()> {
        self.write_backend.delete_collection(req).await
    }

    async fn create_item(
        &self,
        req: crate::models::ItemWriteRequest,
    ) -> Result<crate::models::ItemDetail> {
        self.write_backend.create_item(req).await
    }

    async fn update_item(
        &self,
        req: crate::models::ItemUpdateRequest,
    ) -> Result<crate::models::ItemDetail> {
        self.write_backend.update_item(req).await
    }

    async fn delete_item(&self, req: crate::models::DeleteItemRequest) -> Result<()> {
        self.write_backend.delete_item(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendModeConfig, LibraryType};

    #[test]
    fn detect_backend_mode_uses_explicit_config_mode() {
        let mut cfg = Config {
            library_type: LibraryType::User,
            user_id: Some(1),
            ..Config::default()
        };
        cfg.backend_mode = BackendModeConfig::Local;
        assert_eq!(detect_backend_mode(&cfg), BackendMode::Local);
        cfg.backend_mode = BackendModeConfig::Cloud;
        assert_eq!(detect_backend_mode(&cfg), BackendMode::Cloud);
        cfg.backend_mode = BackendModeConfig::Hybrid;
        assert_eq!(detect_backend_mode(&cfg), BackendMode::Hybrid);
    }
}
