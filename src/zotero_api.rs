use crate::config::Config;
use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    AttachmentSummary, CollectionSummary, FulltextContent, ItemDetail, ItemSummary,
    ListCollectionsQuery, SearchItemsQuery,
};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;

const ZOTERO_API_VERSION: &str = "3";
const MAX_RETRIES: u32 = 2;

#[derive(Clone)]
pub struct ZoteroApiClient {
    config: Config,
    http: reqwest::Client,
}

impl ZoteroApiClient {
    pub fn new(config: Config) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_secs);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ZoteroMcpError::Http(format!("Failed to build HTTP client: {e}")))?;

        Ok(Self { config, http })
    }

    pub async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
        let query = query.normalized();
        let raw: Vec<RawItemRecord> = self.get_json("/items", &build_search_query(&query)).await?;
        Ok(raw.into_iter().map(ItemSummary::from).collect())
    }

    pub async fn list_collections(
        &self,
        query: ListCollectionsQuery,
    ) -> Result<Vec<CollectionSummary>> {
        let query = query.normalized();
        let path = if query.top_only {
            "/collections/top"
        } else {
            "/collections"
        };
        let raw: Vec<RawCollectionRecord> =
            self.get_json(path, &build_collection_query(&query)).await?;
        Ok(raw.into_iter().map(CollectionSummary::from).collect())
    }

    pub async fn get_item(&self, key: &str) -> Result<ItemDetail> {
        let path = format!("/items/{key}");
        let raw: RawItemRecord = self
            .get_json(&path, &[("format", "json".to_string())])
            .await?;

        let children_path = format!("/items/{key}/children");
        let children: Vec<RawItemRecord> = self
            .get_json(&children_path, &[("format", "json".to_string())])
            .await?;

        let attachments = children
            .into_iter()
            .filter(|item| item.data.item_type.as_deref() == Some("attachment"))
            .map(AttachmentSummary::from)
            .collect::<Vec<_>>();

        let mut item = ItemDetail::from(raw);
        item.attachments = attachments;
        Ok(item)
    }

    pub async fn get_item_fulltext(&self, key: &str) -> Result<FulltextContent> {
        let path = format!("/items/{key}/fulltext");
        let raw: RawFulltext = self.get_json(&path, &[] as &[(&str, String)]).await?;

        Ok(FulltextContent {
            item_key: key.to_string(),
            content: raw.content,
            indexed_pages: raw.indexed_pages,
            total_pages: raw.total_pages,
            indexed_chars: raw.indexed_chars,
            total_chars: raw.total_chars,
        })
    }

    pub async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.get_item_fulltext(attachment_key).await
    }

    async fn get_json<T>(&self, suffix: &str, query: &[(&str, String)]) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut attempt = 0;

        loop {
            let url = self.build_url(suffix)?;
            let mut req = self
                .http
                .get(url)
                .query(query)
                .header("Zotero-API-Version", ZOTERO_API_VERSION);

            if let Some(key) = &self.config.api_key {
                req = req.header("Zotero-API-Key", key);
            }

            let response = req.send().await?;
            let status = response.status();

            if is_retryable(status)
                && attempt < MAX_RETRIES
                && let Some(delay) = retry_delay(response.headers(), attempt)
            {
                attempt += 1;
                sleep(delay).await;
                continue;
            }

            if !status.is_success() {
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<failed to read error body>".to_string());
                return Err(ZoteroMcpError::Api {
                    status: status.as_u16(),
                    message: body,
                });
            }

            let backoff = parse_backoff_secs(response.headers());
            let body = response
                .text()
                .await
                .map_err(|e| ZoteroMcpError::Http(format!("Failed to read response body: {e}")))?;
            let parsed = serde_json::from_str::<T>(&body).map_err(|e| {
                let preview: String = body.chars().take(220).collect();
                ZoteroMcpError::Serde(format!(
                    "Failed to parse API JSON: {e}. Body preview: {preview}"
                ))
            })?;
            if let Some(secs) = backoff {
                sleep(Duration::from_secs(secs)).await;
            }

            return Ok(parsed);
        }
    }

    fn build_url(&self, suffix: &str) -> Result<String> {
        let base = self.config.api_base.trim_end_matches('/');
        let prefix = self.config.library_prefix()?;
        Ok(format!("{base}{prefix}{suffix}"))
    }
}

pub(crate) fn build_search_query(query: &SearchItemsQuery) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("format", "json".to_string()),
        ("include", "data".to_string()),
        ("limit", query.limit.to_string()),
        ("start", query.start.to_string()),
    ];

    if let Some(v) = query.q.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        out.push(("q", v.to_string()));
    }
    if let Some(v) = query
        .qmode
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        out.push(("qmode", v.to_string()));
    }
    if let Some(v) = query
        .item_type
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        out.push(("itemType", v.to_string()));
    }
    if let Some(v) = query
        .tag
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        out.push(("tag", v.to_string()));
    }

    out
}

pub(crate) fn build_collection_query(query: &ListCollectionsQuery) -> Vec<(&'static str, String)> {
    vec![
        ("format", "json".to_string()),
        ("include", "data".to_string()),
        ("limit", query.limit.to_string()),
        ("start", query.start.to_string()),
    ]
}

fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
}

fn retry_delay(headers: &HeaderMap, attempt: u32) -> Option<Duration> {
    if let Some(secs) = parse_retry_after_secs(headers) {
        return Some(Duration::from_secs(secs));
    }
    if let Some(secs) = parse_backoff_secs(headers) {
        return Some(Duration::from_secs(secs));
    }
    let base = 1u64.checked_shl(attempt).unwrap_or(1);
    Some(Duration::from_secs(base.max(1)))
}

fn parse_retry_after_secs(headers: &HeaderMap) -> Option<u64> {
    parse_u64_header(headers, RETRY_AFTER)
}

fn parse_backoff_secs(headers: &HeaderMap) -> Option<u64> {
    parse_u64_header(headers, HeaderName::from_static("backoff"))
}

fn parse_u64_header(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
}

#[derive(Debug, Deserialize)]
struct RawItemRecord {
    key: String,
    data: RawItemData,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawItemData {
    #[serde(default)]
    item_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    abstract_note: Option<String>,
    #[serde(default)]
    creators: Vec<RawCreator>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCreator {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFulltext {
    content: String,
    #[serde(default)]
    indexed_pages: Option<u32>,
    #[serde(default)]
    total_pages: Option<u32>,
    #[serde(default)]
    indexed_chars: Option<u32>,
    #[serde(default)]
    total_chars: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RawCollectionRecord {
    key: String,
    data: RawCollectionData,
    #[serde(default)]
    meta: Option<RawCollectionMeta>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCollectionData {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_false")]
    parent_collection: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCollectionMeta {
    #[serde(default)]
    num_items: Option<u32>,
}

fn deserialize_optional_string_or_false<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => Ok(Some(s)),
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Bool(false) => Ok(None),
        other => Err(serde::de::Error::custom(format!(
            "expected string/null/false for optional string field, got {other}"
        ))),
    }
}

impl From<RawItemRecord> for ItemSummary {
    fn from(value: RawItemRecord) -> Self {
        Self {
            key: value.key,
            item_type: value
                .data
                .item_type
                .unwrap_or_else(|| "unknown".to_string()),
            title: value
                .data
                .title
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "(untitled)".to_string()),
            creators: creators_to_strings(&value.data.creators),
            year: extract_year(value.data.date.as_deref()),
            url: value.data.url,
        }
    }
}

impl From<RawItemRecord> for ItemDetail {
    fn from(value: RawItemRecord) -> Self {
        Self {
            key: value.key,
            item_type: value
                .data
                .item_type
                .unwrap_or_else(|| "unknown".to_string()),
            title: value
                .data
                .title
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "(untitled)".to_string()),
            creators: creators_to_strings(&value.data.creators),
            year: extract_year(value.data.date.as_deref()),
            abstract_note: value.data.abstract_note,
            url: value.data.url,
            attachments: Vec::new(),
        }
    }
}

impl From<RawItemRecord> for AttachmentSummary {
    fn from(value: RawItemRecord) -> Self {
        Self {
            key: value.key,
            title: value
                .data
                .title
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "(attachment)".to_string()),
            content_type: value.data.content_type,
            path: value.data.path,
        }
    }
}

impl From<RawCollectionRecord> for CollectionSummary {
    fn from(value: RawCollectionRecord) -> Self {
        Self {
            key: value.key,
            name: value
                .data
                .name
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "(untitled collection)".to_string()),
            parent_collection: value.data.parent_collection,
            item_count: value.meta.and_then(|meta| meta.num_items),
        }
    }
}

fn creators_to_strings(creators: &[RawCreator]) -> Vec<String> {
    creators
        .iter()
        .filter_map(|c| {
            if let Some(name) = c.name.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                return Some(name.to_string());
            }

            match (
                c.first_name.as_deref().map(str::trim),
                c.last_name.as_deref().map(str::trim),
            ) {
                (Some(first), Some(last)) if !first.is_empty() && !last.is_empty() => {
                    Some(format!("{first} {last}"))
                }
                (None, Some(last)) if !last.is_empty() => Some(last.to_string()),
                (Some(first), None) if !first.is_empty() => Some(first.to_string()),
                _ => None,
            }
        })
        .collect()
}

fn extract_year(date: Option<&str>) -> Option<String> {
    let date = date?.trim();
    if date.is_empty() {
        return None;
    }

    let chars: String = date.chars().take(4).collect();
    if chars.len() == 4 && chars.chars().all(|c| c.is_ascii_digit()) {
        Some(chars)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, LibraryType};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(api_base: String) -> Config {
        Config {
            api_base,
            user_id: Some(123),
            library_type: LibraryType::User,
            ..Config::default()
        }
    }

    #[test]
    fn build_search_query_includes_optional_filters() {
        let query = SearchItemsQuery {
            q: Some("vision transformers".to_string()),
            qmode: Some("everything".to_string()),
            item_type: Some("journalArticle".to_string()),
            tag: Some("ml".to_string()),
            limit: 10,
            start: 20,
        };
        let params = build_search_query(&query);
        assert!(params.contains(&("q", "vision transformers".to_string())));
        assert!(params.contains(&("qmode", "everything".to_string())));
        assert!(params.contains(&("itemType", "journalArticle".to_string())));
        assert!(params.contains(&("tag", "ml".to_string())));
    }

    #[test]
    fn build_collection_query_has_expected_defaults() {
        let params = build_collection_query(&ListCollectionsQuery::default());
        assert!(params.contains(&("format", "json".to_string())));
        assert!(params.contains(&("include", "data".to_string())));
        assert!(params.contains(&("limit", "50".to_string())));
    }

    #[test]
    fn extract_year_parses_first_four_digits() {
        assert_eq!(extract_year(Some("2024-11-05")), Some("2024".to_string()));
        assert_eq!(extract_year(Some("forthcoming")), None);
    }

    #[tokio::test]
    async fn search_items_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
          {
            "key": "ABCD1234",
            "data": {
              "itemType": "journalArticle",
              "title": "A Great Paper",
              "date": "2025-01-01",
              "creators": [{"firstName": "Ada", "lastName": "Lovelace"}],
              "url": "https://example.org/paper"
            }
          }
        ]);
        Mock::given(method("GET"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = ZoteroApiClient::new(test_config(server.uri())).unwrap();
        let result = client
            .search_items(SearchItemsQuery::default())
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "ABCD1234");
        assert_eq!(result[0].title, "A Great Paper");
        assert_eq!(result[0].year.as_deref(), Some("2025"));
        assert_eq!(result[0].creators, vec!["Ada Lovelace"]);
    }

    #[tokio::test]
    async fn get_item_fulltext_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
          "content": "This is extracted text.",
          "indexedPages": 4,
          "totalPages": 4
        });

        Mock::given(method("GET"))
            .and(path("/users/123/items/ATTACH123/fulltext"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = ZoteroApiClient::new(test_config(server.uri())).unwrap();
        let result = client.get_item_fulltext("ATTACH123").await.unwrap();

        assert_eq!(result.item_key, "ATTACH123");
        assert_eq!(result.content, "This is extracted text.");
        assert_eq!(result.indexed_pages, Some(4));
    }

    #[tokio::test]
    async fn list_collections_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            {
                "key": "COLL1",
                "data": {
                    "name": "Research",
                    "parentCollection": null
                },
                "meta": {
                    "numItems": 12
                }
            }
        ]);

        Mock::given(method("GET"))
            .and(path("/users/123/collections/top"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = ZoteroApiClient::new(test_config(server.uri())).unwrap();
        let result = client
            .list_collections(ListCollectionsQuery {
                top_only: true,
                ..ListCollectionsQuery::default()
            })
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].key, "COLL1");
        assert_eq!(result[0].name, "Research");
        assert_eq!(result[0].item_count, Some(12));
    }
}
