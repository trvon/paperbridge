use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SearchItemsQuery {
    pub q: Option<String>,
    pub qmode: Option<String>,
    pub item_type: Option<String>,
    pub tag: Option<String>,
    pub limit: u32,
    pub start: u32,
}

impl Default for SearchItemsQuery {
    fn default() -> Self {
        Self {
            q: None,
            qmode: None,
            item_type: None,
            tag: None,
            limit: 25,
            start: 0,
        }
    }
}

impl SearchItemsQuery {
    pub fn normalized(mut self) -> Self {
        self.limit = self.limit.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ListCollectionsQuery {
    pub top_only: bool,
    pub limit: u32,
    pub start: u32,
}

impl Default for ListCollectionsQuery {
    fn default() -> Self {
        Self {
            top_only: false,
            limit: 50,
            start: 0,
        }
    }
}

impl ListCollectionsQuery {
    pub fn normalized(mut self) -> Self {
        self.limit = self.limit.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemSummary {
    pub key: String,
    pub item_type: String,
    pub title: String,
    pub creators: Vec<String>,
    pub year: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct AttachmentSummary {
    pub key: String,
    pub title: String,
    pub content_type: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemDetail {
    pub key: String,
    pub item_type: String,
    pub title: String,
    pub creators: Vec<String>,
    pub year: Option<String>,
    pub abstract_note: Option<String>,
    pub url: Option<String>,
    pub attachments: Vec<AttachmentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CollectionSummary {
    pub key: String,
    pub name: String,
    pub parent_collection: Option<String>,
    pub item_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct FulltextContent {
    pub item_key: String,
    pub content: String,
    pub indexed_pages: Option<u32>,
    pub total_pages: Option<u32>,
    pub indexed_chars: Option<u32>,
    pub total_chars: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct VoxTextPayload {
    pub source: String,
    pub chunk_count: usize,
    pub chunks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemVoxPayload {
    pub item_key: String,
    pub item_title: String,
    pub attachment: AttachmentSummary,
    pub indexed_pages: Option<u32>,
    pub total_pages: Option<u32>,
    pub indexed_chars: Option<u32>,
    pub total_chars: Option<u32>,
    pub vox: VoxTextPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SearchVoxPayload {
    pub query: String,
    pub result_index: usize,
    pub result_count: usize,
    pub selected_item: ItemSummary,
    pub prepared: ItemVoxPayload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_limit_clamps_to_valid_range() {
        assert_eq!(
            SearchItemsQuery {
                limit: 0,
                ..SearchItemsQuery::default()
            }
            .normalized()
            .limit,
            1
        );
        assert_eq!(
            SearchItemsQuery {
                limit: 999,
                ..SearchItemsQuery::default()
            }
            .normalized()
            .limit,
            100
        );
    }

    #[test]
    fn collection_query_limit_clamps_to_valid_range() {
        assert_eq!(
            ListCollectionsQuery {
                limit: 0,
                ..ListCollectionsQuery::default()
            }
            .normalized()
            .limit,
            1
        );
        assert_eq!(
            ListCollectionsQuery {
                limit: 1000,
                ..ListCollectionsQuery::default()
            }
            .normalized()
            .limit,
            100
        );
    }
}
