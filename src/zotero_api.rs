use crate::backend::{BackendMode, LibraryBackend};
use crate::backends::cloud::CloudZoteroBackend;
use crate::backends::local::LocalZoteroBackend;
use crate::config::Config;
use crate::error::Result;
use std::sync::Arc;

pub fn build_backend(config: Config) -> Result<Arc<dyn LibraryBackend>> {
    if looks_like_local_api_base(&config.api_base) {
        return Ok(Arc::new(LocalZoteroBackend::new(config)?));
    }

    Ok(Arc::new(CloudZoteroBackend::new(config)?))
}

pub fn detect_backend_mode(api_base: &str) -> BackendMode {
    if looks_like_local_api_base(api_base) {
        BackendMode::Local
    } else {
        BackendMode::Cloud
    }
}

fn looks_like_local_api_base(api_base: &str) -> bool {
    let lower = api_base.to_ascii_lowercase();
    lower.contains("127.0.0.1:23119") || lower.contains("localhost:23119")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_backend_mode_recognizes_local_endpoints() {
        assert_eq!(
            detect_backend_mode("http://127.0.0.1:23119/api"),
            BackendMode::Local
        );
        assert_eq!(
            detect_backend_mode("http://localhost:23119/api"),
            BackendMode::Local
        );
        assert_eq!(
            detect_backend_mode("https://api.zotero.org"),
            BackendMode::Cloud
        );
    }
}
