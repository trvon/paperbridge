use crate::backend::{BackendCapabilities, BackendMode, LibraryBackend};
use crate::config::Config;
use crate::error::{Result, ZoteroMcpError, sanitize_message};
use crate::models::{
    AttachmentSummary, CollectionSummary, CollectionUpdateRequest, CollectionWriteRequest,
    CreatorInput, DeleteCollectionRequest, DeleteItemRequest, FulltextContent, ItemDetail,
    ItemSummary, ItemUpdateRequest, ItemWriteRequest, ListCollectionsQuery, SearchItemsQuery,
    TagInput,
};
use crate::security::ensure_secure_transport;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;
use tracing::debug;

const ZOTERO_API_VERSION: &str = "3";
const MAX_RETRIES: u32 = 5;
const ERROR_BODY_LIMIT: usize = 4096;

#[derive(Clone)]
pub struct CloudZoteroBackend {
    config: Config,
    http: reqwest::Client,
}

impl CloudZoteroBackend {
    pub fn new(config: Config) -> Result<Self> {
        if config.api_key.is_some() {
            ensure_secure_transport(config.active_cloud_api_base())?;
            ensure_secure_transport(config.active_write_api_base())?;
        }

        let timeout = Duration::from_secs(config.timeout_secs);
        let http = reqwest::Client::builder()
            // Fail fast on bad DNS targets; we'll retry.
            .connect_timeout(Duration::from_secs(8))
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
            ensure_secure_transport(&url)?;
            debug!(attempt, url = %self.sanitize(&url), "zotero request start");
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
                    debug!(
                        attempt,
                        has_api_key = self.config.api_key.is_some(),
                        error = %self.sanitize(&err.to_string()),
                        is_timeout = err.is_timeout(),
                        is_connect = err.is_connect(),
                        status = ?err.status(),
                        "zotero request send failed"
                    );
                    if attempt < MAX_RETRIES {
                        attempt += 1;
                        sleep(retry_delay_for_attempt(attempt)).await;
                        continue;
                    }

                    return Err(self.http_error(format!("request failed after retries: {err}")));
                }
            };
            let status = response.status();
            debug!(attempt, status=%status, "zotero response received");

            if is_retryable(status)
                && attempt < MAX_RETRIES
                && let Some(delay) = retry_delay(response.headers(), attempt)
            {
                attempt += 1;
                sleep(delay).await;
                continue;
            }

            if !status.is_success() {
                let body = self.error_body(response).await;
                debug!(attempt, status=%status, body_preview=%body.chars().take(200).collect::<String>(), "zotero error response");
                return Err(ZoteroMcpError::Api {
                    status: status.as_u16(),
                    message: body,
                });
            }

            let backoff = parse_backoff_secs(response.headers());
            let body = response
                .text()
                .await
                .map_err(|e| self.http_error(format!("Failed to read response body: {e}")))?;
            let parsed = self.parse_json::<T>(&body)?;
            if let Some(secs) = backoff {
                sleep(Duration::from_secs(secs)).await;
            }

            return Ok(parsed);
        }
    }

    fn sanitize(&self, message: &str) -> String {
        sanitize_message(
            message,
            &[self.config.api_key.as_deref().unwrap_or_default()],
        )
    }

    fn http_error(&self, message: impl AsRef<str>) -> ZoteroMcpError {
        ZoteroMcpError::Http(self.sanitize(message.as_ref()))
    }

    fn parse_json<T: for<'de> Deserialize<'de>>(&self, body: &str) -> Result<T> {
        serde_json::from_str(body).map_err(|error| {
            // Sanitize before shortening so a preview cannot split a credential.
            let preview: String = self.sanitize(body).chars().take(220).collect();
            ZoteroMcpError::Serde(self.sanitize(&format!(
                "Failed to parse API JSON: {error}. Body preview: {preview}"
            )))
        })
    }

    async fn error_body(&self, mut response: reqwest::Response) -> String {
        // Read at most one byte beyond the cap to distinguish exact-length bodies.
        // Do not trust Content-Length or buffer the complete response first.
        let mut bytes = Vec::with_capacity(ERROR_BODY_LIMIT + 1);
        while bytes.len() <= ERROR_BODY_LIMIT {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let remaining = ERROR_BODY_LIMIT + 1 - bytes.len();
                    bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                }
                Ok(None) => break,
                Err(_) => return self.sanitize("<failed to read error body>"),
            }
        }
        let truncated = bytes.len() > ERROR_BODY_LIMIT;
        bytes.truncate(ERROR_BODY_LIMIT);
        if truncated
            && let Err(error) = std::str::from_utf8(&bytes)
            && error.error_len().is_none()
        {
            bytes.truncate(error.valid_up_to());
        }
        let mut message = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            // The cap may split a known key before the sanitizer can match it.
            if let Some(key) = self.config.api_key.as_deref()
                && let Some(end) = key
                    .char_indices()
                    .map(|(i, _)| i)
                    .rev()
                    .find(|&end| end > 0 && message.ends_with(&key[..end]))
            {
                message.truncate(message.len() - end);
                message.push_str("<redacted>");
            }
            message.push_str(" [truncated]");
        }
        self.sanitize(&message)
    }

    fn build_url(&self, suffix: &str) -> Result<String> {
        let base = self.config.active_write_api_base().trim_end_matches('/');
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
        let url = self.build_url(suffix)?;
        ensure_secure_transport(&url)?;
        let mut request = self
            .http
            .request(method, url)
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

        let response = request
            .json(&body)
            .send()
            .await
            .map_err(|e| self.http_error(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: self.error_body(response).await,
            });
        }

        response
            .text()
            .await
            .map_err(|e| self.http_error(e.to_string()))
    }

    async fn send_delete(&self, suffix: &str, version: u64) -> Result<()> {
        let url = self.build_url(suffix)?;
        ensure_secure_transport(&url)?;
        let mut request = self
            .http
            .delete(url)
            .header("Zotero-API-Version", ZOTERO_API_VERSION)
            .header("If-Unmodified-Since-Version", version.to_string());

        if let Some(api_key) = &self.config.api_key {
            request = request.header("Zotero-API-Key", api_key);
        }

        let response = request
            .send()
            .await
            .map_err(|e| self.http_error(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: self.error_body(response).await,
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

    async fn get_attachment_bytes(&self, attachment_key: &str) -> Result<Vec<u8>> {
        let suffix = format!("/items/{attachment_key}/file");
        let url = self.build_url(&suffix)?;
        ensure_secure_transport(&url)?;
        let mut req = self
            .http
            .get(url)
            .header("Zotero-API-Version", ZOTERO_API_VERSION);
        if let Some(key) = &self.config.api_key {
            req = req.header("Zotero-API-Key", key);
        }
        let response = req
            .send()
            .await
            .map_err(|e| self.http_error(format!("attachment fetch failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = self.error_body(response).await;
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: body,
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| self.http_error(format!("attachment body read failed: {e}")))?;
        Ok(bytes.to_vec())
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
        let result: MultiWriteResponse = self.parse_json(&text)?;
        let saved = result.first_successful().ok_or_else(|| {
            ZoteroMcpError::Serde(
                "create_collection response missing successful object".to_string(),
            )
        })?;
        Ok(CollectionSummary {
            key: saved.key,
            version: saved.version,
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
        let result: MultiWriteResponse = self.parse_json(&text)?;
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
        let raw: RawCollectionRecord = self.parse_json(&text)?;
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
        let raw: RawItemRecord = self.parse_json(&text)?;
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
    let creators = saved
        .data
        .get("creators")
        .and_then(|v| serde_json::from_value::<Vec<RawCreator>>(v.clone()).ok())
        .unwrap_or_default();
    ItemDetail {
        key: saved.key,
        version: saved.version,
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
        creators: creators_to_strings(&creators),
        creator_details: creators.into_iter().map(CreatorInput::from).collect(),
        doi: saved
            .data
            .get("DOI")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        venue: saved
            .data
            .get("publicationTitle")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        isbn: saved
            .data
            .get("ISBN")
            .and_then(|v| v.as_str())
            .map(str::to_string),
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
        date: saved
            .data
            .get("date")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        tags: saved
            .data
            .get("tags")
            .and_then(|v| serde_json::from_value::<Vec<TagInput>>(v.clone()).ok())
            .unwrap_or_default(),
        collections: saved
            .data
            .get("collections")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .unwrap_or_default(),
        extra: saved
            .data
            .get("extra")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        parent_item: saved
            .data
            .get("parentItem")
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
    #[serde(default)]
    version: Option<u64>,
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
    #[serde(default, rename = "DOI")]
    doi: Option<String>,
    #[serde(default)]
    publication_title: Option<String>,
    #[serde(default, rename = "ISBN")]
    isbn: Option<String>,
    #[serde(default)]
    creators: Vec<RawCreator>,
    #[serde(default)]
    tags: Vec<TagInput>,
    #[serde(default)]
    collections: Vec<String>,
    #[serde(default)]
    extra: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_or_false")]
    parent_item: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCreator {
    #[serde(default)]
    creator_type: String,
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
    #[serde(default)]
    version: Option<u64>,
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
    // Zotero has historically returned either `successful` (documented) or `success` depending on
    // endpoint/version. Accept both so create/update flows are resilient.
    #[serde(default)]
    success: std::collections::HashMap<String, RawSavedObject>,
}

impl MultiWriteResponse {
    fn first_successful(self) -> Option<RawSavedObject> {
        let map = if !self.successful.is_empty() {
            self.successful
        } else {
            self.success
        };
        let mut entries = map.into_iter().collect::<Vec<_>>();
        entries.sort_by_key(|(idx, _)| idx.parse::<usize>().unwrap_or(usize::MAX));
        entries.into_iter().map(|(_, value)| value).next()
    }
}

#[derive(Debug, Deserialize)]
struct RawSavedObject {
    key: String,
    #[serde(default)]
    version: Option<u64>,
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
            doi: value.data.doi,
        }
    }
}

impl From<RawItemRecord> for ItemDetail {
    fn from(value: RawItemRecord) -> Self {
        Self {
            key: value.key,
            version: value.version,
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
            creator_details: value
                .data
                .creators
                .into_iter()
                .map(CreatorInput::from)
                .collect(),
            doi: value.data.doi,
            venue: value.data.publication_title,
            isbn: value.data.isbn,
            year: extract_year(value.data.date.as_deref()),
            abstract_note: value.data.abstract_note,
            url: value.data.url,
            date: value.data.date,
            tags: value.data.tags,
            collections: value.data.collections,
            extra: value.data.extra,
            parent_item: value.data.parent_item,
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
            version: value.version,
        }
    }
}

impl From<RawCollectionRecord> for CollectionSummary {
    fn from(value: RawCollectionRecord) -> Self {
        Self {
            key: value.key,
            version: value.version,
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

impl From<RawCreator> for CreatorInput {
    fn from(value: RawCreator) -> Self {
        Self {
            creator_type: value.creator_type,
            first_name: value.first_name,
            last_name: value.last_name,
            name: value.name,
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
    use crate::config::{BackendModeConfig, LibraryType};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(api_base: String) -> Config {
        Config {
            backend_mode: BackendModeConfig::Cloud,
            cloud_api_base: api_base,
            local_api_base: "http://127.0.0.1:23119/api".to_string(),
            user_id: Some(123),
            library_type: LibraryType::User,
            api_key: Some("test-key".to_string()),
            ..Config::default()
        }
    }

    fn item_request(title: &str) -> ItemWriteRequest {
        ItemWriteRequest {
            item_type: "journalArticle".to_string(),
            title: Some(title.to_string()),
            creators: vec![CreatorInput {
                creator_type: "author".to_string(),
                first_name: Some("Grace".to_string()),
                last_name: Some("Hopper".to_string()),
                name: None,
            }],
            abstract_note: Some("Body.".to_string()),
            date: Some("2024".to_string()),
            url: None,
            doi: Some("10.1/test".to_string()),
            isbn: None,
            tags: vec![],
            collections: vec![],
            extra: None,
            parent_item: None,
        }
    }

    fn citation_record() -> serde_json::Value {
        serde_json::json!({
            "key": "CITE1", "version": 42,
            "data": {
                "itemType": "journalArticle", "title": "Original citation",
                "DOI": "10.1234/Original.DOI", "publicationTitle": "Original Venue",
                "ISBN": "978-1-23456-789-0",
                "creators": [{"creatorType": "editor", "firstName": "Grace", "lastName": "Hopper"}]
            }
        })
    }

    fn assert_citation(item: ItemDetail) {
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json["doi"], "10.1234/Original.DOI");
        assert_eq!(json["venue"], "Original Venue");
        assert_eq!(json["isbn"], "978-1-23456-789-0");
        assert_eq!(json["creators"], serde_json::json!(["Grace Hopper"]));
        assert_eq!(json["creator_details"][0]["creator_type"], "editor");
    }

    #[tokio::test]
    async fn citation_metadata_survives_search_get_create_and_update() {
        let server = MockServer::start().await;
        for verb in ["GET", "PUT"] {
            Mock::given(method(verb))
                .and(path("/users/123/items/CITE1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(citation_record()))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/users/123/items/CITE1/children"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![citation_record()]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "successful": {"0": citation_record()}
            })))
            .mount(&server)
            .await;
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        let hits = backend
            .search_items(SearchItemsQuery::default())
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(hits).unwrap()[0]["doi"],
            "10.1234/Original.DOI"
        );
        assert_citation(backend.get_item("CITE1").await.unwrap());
        assert_citation(
            backend
                .create_item(item_request("Original citation"))
                .await
                .unwrap(),
        );
        let request = serde_json::from_value(serde_json::json!({
            "key": "CITE1", "version": 41, "title": "Original citation"
        }))
        .unwrap();
        assert_citation(backend.update_item(request).await.unwrap());
    }

    #[tokio::test]
    async fn collection_version_survives_list_create_and_update() {
        let server = MockServer::start().await;
        let record = serde_json::json!({
            "key": "COLL1", "version": 73, "data": {"name": "Research", "parentCollection": false}
        });
        Mock::given(method("GET"))
            .and(path("/users/123/collections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![record.clone()]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/users/123/collections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": {"0": record.clone()}
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/users/123/collections/COLL1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(record))
            .mount(&server)
            .await;
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        let collections = backend
            .list_collections(ListCollectionsQuery::default())
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(collections).unwrap()[0]["version"], 73);
        let created = backend
            .create_collection(CollectionWriteRequest {
                name: "Research".to_string(),
                parent_collection: None,
            })
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(created).unwrap()["version"], 73);
        let request = serde_json::from_value(serde_json::json!({
            "key": "COLL1", "version": 72, "name": "Research"
        }))
        .unwrap();
        let updated = backend.update_collection(request).await.unwrap();
        assert_eq!(serde_json::to_value(updated).unwrap()["version"], 73);
    }

    #[tokio::test]
    async fn upstream_error_bodies_are_bounded_and_redacted_for_all_request_paths() {
        let server = MockServer::start().await;
        let prefix = "test-key https://example.test/?api_key=url-secret ";
        let body = format!(
            "{prefix}{}界TRAILING-MARKER",
            "x".repeat(4095 - prefix.len())
        );
        for verb in ["GET", "POST", "DELETE"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(404).set_body_string(body.clone()))
                .mount(&server)
                .await;
        }
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        let errors = [
            backend.get_item("MISSING").await.unwrap_err(),
            backend.create_item(item_request("X")).await.unwrap_err(),
            backend
                .delete_item(DeleteItemRequest {
                    key: "MISSING".to_string(),
                    version: Some(1),
                })
                .await
                .unwrap_err(),
            backend.get_attachment_bytes("MISSING").await.unwrap_err(),
        ];
        for error in errors {
            let ZoteroMcpError::Api { status, message } = error else {
                panic!("expected API error")
            };
            assert_eq!(status, 404);
            assert!(!message.contains("test-key"));
            assert!(!message.contains("url-secret"));
            assert!(!message.contains("TRAILING-MARKER"));
            assert!(!message.contains('\u{fffd}'));
            assert!(message.contains("[truncated]"));
            assert!(message.len() < 4300);
        }
    }

    #[tokio::test]
    async fn error_body_cap_does_not_wait_for_chunked_response_to_finish() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            socket.read(&mut request).await.unwrap();
            let body = "x".repeat(5000);
            socket.write_all(format!(
                "HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n",
                body.len()
            ).as_bytes()).await.unwrap();
            // Keep the stream open without a terminal chunk until the client returns.
            let _ = done_rx.await;
        });
        let mut config = test_config(format!("http://{address}"));
        config.timeout_secs = 2;
        let backend = CloudZoteroBackend::new(config).unwrap();
        let error = backend.get_item("MISSING").await.unwrap_err();
        let _ = done_tx.send(());
        server.await.unwrap();
        assert!(error.to_string().contains("[truncated]"), "{error}");
    }

    #[tokio::test]
    async fn error_body_does_not_expose_configured_key_split_by_read_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_string(format!("{}test-key trailing", "x".repeat(4090))),
            )
            .mount(&server)
            .await;
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        let message = backend.get_item("MISSING").await.unwrap_err().to_string();
        assert!(!message.contains("test-k"));
        assert!(message.ends_with("[truncated]"));
    }

    #[tokio::test]
    async fn error_body_exact_limit_and_short_unicode_are_not_marked_truncated() {
        for body in ["x".repeat(4096), "é界".to_string()] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(404).set_body_string(body.clone()))
                .mount(&server)
                .await;
            let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
            let ZoteroMcpError::Api { message, .. } =
                backend.get_item("MISSING").await.unwrap_err()
            else {
                panic!("expected API error")
            };
            assert_eq!(message, body);
        }
    }

    #[tokio::test]
    async fn successful_response_parse_errors_redact_preview_and_serde_error() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "key": "BAD1", "data": {"parentItem": {
                "secret": "test-key", "url": "https://example.test/?token=url-secret"
            }}
        })
        .to_string();
        for verb in ["GET", "PUT", "POST"] {
            Mock::given(method(verb))
                .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
                .mount(&server)
                .await;
        }
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        let request =
            serde_json::from_value(serde_json::json!({"key": "BAD1", "version": 1})).unwrap();
        let errors = [
            backend.get_item("BAD1").await.unwrap_err(),
            backend.update_item(request).await.unwrap_err(),
        ];
        for error in errors {
            let message = error.to_string();
            assert!(message.contains("parse API JSON"));
            assert!(!message.contains("test-key"));
            assert!(!message.contains("url-secret"));
        }
    }

    #[tokio::test]
    async fn successful_fulltext_is_not_limited_by_error_body_cap() {
        let server = MockServer::start().await;
        let content = "界".repeat(5000);
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"content": content})),
            )
            .mount(&server)
            .await;
        let backend = CloudZoteroBackend::new(test_config(server.uri())).unwrap();
        assert_eq!(
            backend.get_item_fulltext("TEXT1").await.unwrap().content,
            content
        );
    }

    #[test]
    fn mode_reports_cloud() {
        let server_base = "https://api.zotero.org".to_string();
        let backend = CloudZoteroBackend::new(test_config(server_base)).unwrap();
        assert_eq!(backend.mode(), BackendMode::Cloud);
    }

    #[test]
    fn capabilities_match_read_only_cloud_constants() {
        let server_base = "https://api.zotero.org".to_string();
        let backend = CloudZoteroBackend::new(test_config(server_base)).unwrap();
        assert_eq!(
            backend.capabilities(),
            BackendCapabilities::read_only_cloud()
        );
    }

    #[test]
    fn new_requires_secure_transport_when_api_key_present() {
        // ensure_secure_transport rejects http:// when an api_key is configured —
        // we should not leak a Zotero key over plaintext to a non-loopback host.
        let mut cfg = test_config("http://api.zotero.org".to_string());
        cfg.api_key = Some("test-key".to_string());
        let err = match CloudZoteroBackend::new(cfg) {
            Ok(_) => panic!("expected transport rejection for plaintext non-loopback"),
            Err(e) => e,
        };
        assert!(
            format!("{err}").to_lowercase().contains("http"),
            "expected transport error, got: {err}"
        );
    }

    #[tokio::test]
    async fn get_attachment_bytes_returns_response_body() {
        let server = MockServer::start().await;
        let pdf_marker: Vec<u8> = vec![0x25, 0x50, 0x44, 0x46]; // "%PDF"
        Mock::given(method("GET"))
            .and(path("/users/123/items/ATTACH/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(pdf_marker.clone()))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None; // allow plaintext for wiremock loopback
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        let bytes = backend.get_attachment_bytes("ATTACH").await.unwrap();
        assert_eq!(bytes, pdf_marker);
    }

    #[tokio::test]
    async fn get_attachment_bytes_surfaces_api_error_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/123/items/MISSING/file"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        let err = backend.get_attachment_bytes("MISSING").await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 404),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_item_parses_multiwrite_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "successful": {
                    "0": {
                        "key": "NEW1",
                        "version": 42,
                        "data": {
                            "itemType": "journalArticle",
                            "title": "Created Paper",
                            "abstractNote": "Body."
                        }
                    }
                },
                "unchanged": {},
                "failed": {}
            })))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        let item = backend
            .create_item(item_request("Created Paper"))
            .await
            .unwrap();
        assert_eq!(item.key, "NEW1");
        assert_eq!(item.title, "Created Paper");
    }

    #[tokio::test]
    async fn create_collection_parses_multiwrite_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/users/123/collections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "successful": {
                    "0": {
                        "key": "COLLNEW",
                        "version": 1,
                        "data": {"name": "Research"}
                    }
                }
            })))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        let collection = backend
            .create_collection(CollectionWriteRequest {
                name: "Research".to_string(),
                parent_collection: None,
            })
            .await
            .unwrap();
        assert_eq!(collection.key, "COLLNEW");
        assert_eq!(collection.name, "Research");
    }

    #[tokio::test]
    async fn create_item_surfaces_api_error_on_412() {
        // 412 Precondition Failed is what Zotero returns when an
        // If-Unmodified-Since-Version check fails on a write.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/users/123/items"))
            .respond_with(ResponseTemplate::new(412).set_body_string("version mismatch"))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        let err = backend.create_item(item_request("X")).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 412),
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_item_completes_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/users/123/items/DEL1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        backend
            .delete_item(DeleteItemRequest {
                key: "DEL1".to_string(),
                version: Some(5),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_collection_completes_on_204() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/users/123/collections/COLL1"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let mut cfg = test_config(server.uri());
        cfg.api_key = None;
        let backend = CloudZoteroBackend::new(cfg).unwrap();
        backend
            .delete_collection(DeleteCollectionRequest {
                key: "COLL1".to_string(),
                version: Some(3),
            })
            .await
            .unwrap();
    }
}
