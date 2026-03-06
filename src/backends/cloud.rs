use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::config::Config;
use crate::error::{Result, ZoteroMcpError};
use crate::models::{
    AttachmentSummary, CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest,
    CreatorInput, DeleteCollectionRequest, DeleteItemRequest, FulltextContent, ItemDetail,
    ItemSummary, ItemUpdateRequest, ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
    TagInput,
};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;

const ZOTERO_API_VERSION: &str = "3";
const MAX_RETRIES: u32 = 2;

#[derive(Clone)]
pub struct CloudZoteroBackend {
    config: Config,
    http: reqwest::Client,
}

impl CloudZoteroBackend {
    pub fn new(config: Config) -> Result<Self> {
        let timeout = Duration::from_secs(config.timeout_secs);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ZoteroMcpError::Http(format!("Failed to build HTTP client: {e}")))?;

        Ok(Self { config, http })
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

            let response = match req.send().await {
                Ok(resp) => resp,
                Err(err) => {
                    if attempt < MAX_RETRIES {
                        attempt += 1;
                        sleep(retry_delay_for_attempt(attempt)).await;
                        continue;
                    }

                    return Err(ZoteroMcpError::Http(format!(
                        "request failed after retries: {err}"
                    )));
                }
            };
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

    async fn send_json_write(
        &self,
        method: reqwest::Method,
        suffix: &str,
        body: serde_json::Value,
        version: Option<u64>,
    ) -> Result<String> {
        let mut request = self
            .http
            .request(method, self.build_url(suffix)?)
            .header("Zotero-API-Version", ZOTERO_API_VERSION)
            .header(reqwest::header::CONTENT_TYPE, "application/json");

        if let Some(api_key) = &self.config.api_key {
            request = request.header("Zotero-API-Key", api_key);
        }

        if let Some(version) = version {
            request = request.header("If-Unmodified-Since-Version", version.to_string());
        } else {
            request = request.header("Zotero-Write-Token", generate_write_token());
        }

        let response = request.json(&body).send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(text)
    }

    async fn send_delete(&self, suffix: &str, version: u64) -> Result<()> {
        let mut request = self
            .http
            .delete(self.build_url(suffix)?)
            .header("Zotero-API-Version", ZOTERO_API_VERSION)
            .header("If-Unmodified-Since-Version", version.to_string());

        if let Some(api_key) = &self.config.api_key {
            request = request.header("Zotero-API-Key", api_key);
        }

        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl LibraryBackend for CloudZoteroBackend {
    fn mode(&self) -> BackendMode {
        BackendMode::Cloud
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::read_only_cloud()
    }

    async fn search_items(&self, query: SearchItemsQuery) -> Result<Vec<ItemSummary>> {
        let query = query.normalized();
        let raw: Vec<RawItemRecord> = self.get_json("/items", &build_search_query(&query)).await?;
        Ok(raw.into_iter().map(ItemSummary::from).collect())
    }

    async fn list_collections(
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

    async fn get_item(&self, key: &str) -> Result<ItemDetail> {
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

    async fn get_item_fulltext(&self, key: &str) -> Result<FulltextContent> {
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

    async fn get_pdf_text(&self, attachment_key: &str) -> Result<FulltextContent> {
        self.get_item_fulltext(attachment_key).await
    }

    async fn create_collection(&self, req: CollectionWriteRequest) -> Result<CollectionSummary> {
        let mut object = serde_json::Map::new();
        object.insert("name".to_string(), serde_json::Value::String(req.name));
        if let Some(parent) = req.parent_collection {
            object.insert(
                "parentCollection".to_string(),
                serde_json::Value::String(parent),
            );
        }
        let body = serde_json::Value::Array(vec![serde_json::Value::Object(object)]);
        let text = self
            .send_json_write(reqwest::Method::POST, "/collections", body, None)
            .await?;
        let result: MultiWriteResponse = serde_json::from_str(&text)?;
        let saved = result.first_successful().ok_or_else(|| {
            ZoteroMcpError::Serde(
                "create_collection response missing successful object".to_string(),
            )
        })?;
        Ok(CollectionSummary {
            key: saved.key,
            name: saved
                .data
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "(untitled collection)".to_string()),
            parent_collection: saved
                .data
                .get("parentCollection")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            item_count: None,
        })
    }

    async fn create_item(&self, req: ItemWriteRequest) -> Result<ItemDetail> {
        let payload = serde_json::json!([item_write_json(req)]);
        let text = self
            .send_json_write(reqwest::Method::POST, "/items", payload, None)
            .await?;
        let result: MultiWriteResponse = serde_json::from_str(&text)?;
        let saved = result.first_successful().ok_or_else(|| {
            ZoteroMcpError::Serde("create_item response missing successful object".to_string())
        })?;

        Ok(item_detail_from_saved(saved))
    }

    async fn update_collection(&self, req: CollectionUpdateRequest) -> Result<CollectionSummary> {
        let version = req.version.unwrap_or(0);
        let payload = collection_update_json(&req);
        let text = self
            .send_json_write(
                reqwest::Method::PUT,
                &format!("/collections/{}", req.key),
                payload,
                Some(version),
            )
            .await?;
        let raw: RawCollectionRecord = serde_json::from_str(&text)?;
        Ok(CollectionSummary::from(raw))
    }

    async fn delete_collection(&self, req: DeleteCollectionRequest) -> Result<()> {
        let version = req.version.unwrap_or(0);
        self.send_delete(&format!("/collections/{}", req.key), version)
            .await
    }

    async fn update_item(&self, req: ItemUpdateRequest) -> Result<ItemDetail> {
        let version = req.version.unwrap_or(0);
        let key = req.key.clone();
        let payload = item_update_json(req);
        let text = self
            .send_json_write(
                reqwest::Method::PUT,
                &format!("/items/{key}"),
                payload,
                Some(version),
            )
            .await?;
        let raw: RawItemRecord = serde_json::from_str(&text)?;
        let mut item = ItemDetail::from(raw);

        let children_path = format!("/items/{key}/children");
        let children: Vec<RawItemRecord> = self
            .get_json(&children_path, &[("format", "json".to_string())])
            .await?;
        item.attachments = children
            .into_iter()
            .filter(|entry| entry.data.item_type.as_deref() == Some("attachment"))
            .map(AttachmentSummary::from)
            .collect();

        Ok(item)
    }

    async fn delete_item(&self, req: DeleteItemRequest) -> Result<()> {
        let version = req.version.unwrap_or(0);
        self.send_delete(&format!("/items/{}", req.key), version)
            .await
    }
}

fn item_detail_from_saved(saved: RawSavedObject) -> ItemDetail {
    ItemDetail {
        key: saved.key,
        item_type: saved
            .data
            .get("itemType")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string()),
        title: saved
            .data
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "(untitled)".to_string()),
        creators: saved
            .data
            .get("creators")
            .and_then(|v| serde_json::from_value::<Vec<RawCreator>>(v.clone()).ok())
            .map(|v| creators_to_strings(&v))
            .unwrap_or_default(),
        year: extract_year(saved.data.get("date").and_then(|v| v.as_str())),
        abstract_note: saved
            .data
            .get("abstractNote")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        url: saved
            .data
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        attachments: Vec::new(),
    }
}

pub(crate) fn item_write_json(req: ItemWriteRequest) -> serde_json::Value {
    serde_json::json!({
        "itemType": req.item_type,
        "title": req.title.unwrap_or_default(),
        "creators": req.creators.into_iter().map(creator_input_json).collect::<Vec<_>>(),
        "abstractNote": req.abstract_note.unwrap_or_default(),
        "date": req.date.unwrap_or_default(),
        "url": req.url.unwrap_or_default(),
        "DOI": req.doi.unwrap_or_default(),
        "ISBN": req.isbn.unwrap_or_default(),
        "tags": req.tags.into_iter().map(tag_input_json).collect::<Vec<_>>(),
        "collections": req.collections,
        "extra": req.extra.unwrap_or_default(),
        "parentItem": req.parent_item.unwrap_or_default(),
    })
}

fn collection_update_json(req: &CollectionUpdateRequest) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "key".to_string(),
        serde_json::Value::String(req.key.clone()),
    );
    if let Some(version) = req.version {
        object.insert(
            "version".to_string(),
            serde_json::Value::Number(version.into()),
        );
    }
    if let Some(name) = req.name.as_ref() {
        object.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }
    if req.clear_parent {
        object.insert(
            "parentCollection".to_string(),
            serde_json::Value::Bool(false),
        );
    } else if let Some(parent) = req.parent_collection.as_ref() {
        object.insert(
            "parentCollection".to_string(),
            serde_json::Value::String(parent.clone()),
        );
    }
    serde_json::Value::Object(object)
}

pub(crate) fn item_update_json(req: ItemUpdateRequest) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("key".to_string(), serde_json::Value::String(req.key));
    if let Some(version) = req.version {
        object.insert(
            "version".to_string(),
            serde_json::Value::Number(version.into()),
        );
    }
    if let Some(item_type) = req.item_type {
        object.insert("itemType".to_string(), serde_json::Value::String(item_type));
    }
    if let Some(title) = req.title {
        object.insert("title".to_string(), serde_json::Value::String(title));
    }
    if let Some(creators) = req.creators {
        object.insert(
            "creators".to_string(),
            serde_json::Value::Array(creators.into_iter().map(creator_input_json).collect()),
        );
    }
    if let Some(abstract_note) = req.abstract_note {
        object.insert(
            "abstractNote".to_string(),
            serde_json::Value::String(abstract_note),
        );
    }
    if let Some(date) = req.date {
        object.insert("date".to_string(), serde_json::Value::String(date));
    }
    if let Some(url) = req.url {
        object.insert("url".to_string(), serde_json::Value::String(url));
    }
    if let Some(doi) = req.doi {
        object.insert("DOI".to_string(), serde_json::Value::String(doi));
    }
    if let Some(isbn) = req.isbn {
        object.insert("ISBN".to_string(), serde_json::Value::String(isbn));
    }
    if let Some(tags) = req.tags {
        object.insert(
            "tags".to_string(),
            serde_json::Value::Array(tags.into_iter().map(tag_input_json).collect()),
        );
    }
    if let Some(collections) = req.collections {
        object.insert(
            "collections".to_string(),
            serde_json::Value::Array(
                collections
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
    }
    if let Some(extra) = req.extra {
        object.insert("extra".to_string(), serde_json::Value::String(extra));
    }
    if req.clear_parent {
        object.insert("parentItem".to_string(), serde_json::Value::Bool(false));
    } else if let Some(parent) = req.parent_item {
        object.insert("parentItem".to_string(), serde_json::Value::String(parent));
    }
    serde_json::Value::Object(object)
}

fn creator_input_json(creator: CreatorInput) -> serde_json::Value {
    serde_json::json!({
        "creatorType": creator.creator_type,
        "firstName": creator.first_name.unwrap_or_default(),
        "lastName": creator.last_name.unwrap_or_default(),
        "name": creator.name.unwrap_or_default(),
    })
}

fn tag_input_json(tag: TagInput) -> serde_json::Value {
    serde_json::json!({
        "tag": tag.tag,
        "type": tag.tag_type.unwrap_or(0),
    })
}

fn generate_write_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let n = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:032x}", now ^ n as u128)
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

fn retry_delay_for_attempt(attempt: u32) -> Duration {
    let clamped = attempt.min(8);
    let secs = 1u64.checked_shl(clamped).unwrap_or(1).max(1);
    Duration::from_secs(secs)
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

#[derive(Debug, Deserialize)]
struct MultiWriteResponse {
    #[serde(default)]
    successful: std::collections::HashMap<String, RawSavedObject>,
}

impl MultiWriteResponse {
    fn first_successful(self) -> Option<RawSavedObject> {
        let mut entries = self.successful.into_iter().collect::<Vec<_>>();
        entries.sort_by_key(|(idx, _)| idx.parse::<usize>().unwrap_or(usize::MAX));
        entries.into_iter().map(|(_, value)| value).next()
    }
}

#[derive(Debug, Deserialize)]
struct RawSavedObject {
    key: String,
    data: serde_json::Value,
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
