pub mod docker;
pub mod fallback;
pub mod grobid;
pub mod selector;
pub mod tei;

use crate::error::Result;
use crate::models::{FulltextContent, ItemDetail, PaperStructure};

pub fn build_from_fulltext(item: &ItemDetail, fulltext: &FulltextContent) -> PaperStructure {
    fallback::build(item, fulltext)
}

pub fn query(structure: &PaperStructure, selector_path: &str) -> Result<serde_json::Value> {
    let root = serde_json::to_value(structure)
        .map_err(|e| crate::error::ZoteroMcpError::Serde(e.to_string()))?;
    selector::evaluate(&root, selector_path)
}
