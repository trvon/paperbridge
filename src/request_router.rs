use crate::error::{Result, ZoteroMcpError};
use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use reqwest::{RequestBuilder, Response, StatusCode};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, sleep};

const DEFAULT_GLOBAL_LIMIT: usize = 8;
const DEFAULT_PER_ORIGIN_LIMIT: usize = 2;
const RETRY_DELAY_DEFAULT_MS: u64 = 500;
const RETRY_DELAY_CAP_MS: u64 = 2_000;
const BACKOFF: HeaderName = HeaderName::from_static("backoff");

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct RequestRouterEvent {
    pub request_id: u64,
    pub component: String,
    pub origin: String,
    pub attempt: u8,
    pub status: u16,
    pub queue_ms: u64,
    pub ttfb_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct RequestRouterSnapshot {
    pub requests: u64,
    pub attempts: u64,
    pub retries: u64,
    pub rate_limited: u64,
    pub transport_failures: u64,
    pub in_flight: u64,
    pub last_event: Option<RequestRouterEvent>,
}

#[derive(Debug, Default)]
struct RouterMetrics {
    requests: AtomicU64,
    attempts: AtomicU64,
    retries: AtomicU64,
    rate_limited: AtomicU64,
    transport_failures: AtomicU64,
    in_flight: AtomicU64,
}

struct OriginState {
    permits: Arc<Semaphore>,
    cooldown: Mutex<Option<Instant>>,
}

impl OriginState {
    fn new(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            cooldown: Mutex::new(None),
        }
    }
}

pub struct RequestRouter {
    global_permits: Arc<Semaphore>,
    per_origin_limit: usize,
    origins: Mutex<HashMap<String, Arc<OriginState>>>,
    metrics: Arc<RouterMetrics>,
    last_event: StdMutex<Option<RequestRouterEvent>>,
    next_request_id: AtomicU64,
}

impl Default for RequestRouter {
    fn default() -> Self {
        Self::new(DEFAULT_GLOBAL_LIMIT, DEFAULT_PER_ORIGIN_LIMIT)
    }
}

impl RequestRouter {
    pub fn new(global_limit: usize, per_origin_limit: usize) -> Self {
        Self {
            global_permits: Arc::new(Semaphore::new(global_limit.max(1))),
            per_origin_limit: per_origin_limit.max(1),
            origins: Mutex::new(HashMap::new()),
            metrics: Arc::new(RouterMetrics::default()),
            last_event: StdMutex::new(None),
            next_request_id: AtomicU64::new(1),
        }
    }

    pub fn snapshot(&self) -> RequestRouterSnapshot {
        RequestRouterSnapshot {
            requests: self.metrics.requests.load(Ordering::Relaxed),
            attempts: self.metrics.attempts.load(Ordering::Relaxed),
            retries: self.metrics.retries.load(Ordering::Relaxed),
            rate_limited: self.metrics.rate_limited.load(Ordering::Relaxed),
            transport_failures: self.metrics.transport_failures.load(Ordering::Relaxed),
            in_flight: self.metrics.in_flight.load(Ordering::Relaxed),
            last_event: self
                .last_event
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        }
    }

    pub async fn send(
        &self,
        component: &'static str,
        request: RequestBuilder,
    ) -> Result<RoutedResponse> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        let origin = request_origin(&request)?;
        let origin_state = self.origin(&origin).await;
        let mut attempt = 0_u8;

        loop {
            attempt += 1;
            self.wait_for_cooldown(&origin_state).await;
            let queued_at = Instant::now();
            let origin_permit = origin_state
                .permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ZoteroMcpError::Http("request router origin closed".to_string()))?;
            let global_permit = self
                .global_permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ZoteroMcpError::Http("request router closed".to_string()))?;

            if self.cooldown_remaining(&origin_state).await > Duration::ZERO {
                drop(global_permit);
                drop(origin_permit);
                continue;
            }

            let outbound = request.try_clone().ok_or_else(|| {
                ZoteroMcpError::Http("request not cloneable for retry".to_string())
            })?;
            self.metrics.attempts.fetch_add(1, Ordering::Relaxed);
            self.metrics.in_flight.fetch_add(1, Ordering::Relaxed);
            let started_at = Instant::now();
            let response = match outbound.send().await {
                Ok(response) => response,
                Err(error) => {
                    self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                    self.metrics
                        .transport_failures
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(error.into());
                }
            };
            let status = response.status();
            *self
                .last_event
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RequestRouterEvent {
                request_id,
                component: component.to_string(),
                origin: origin.clone(),
                attempt,
                status: status.as_u16(),
                queue_ms: queued_at.elapsed().as_millis() as u64,
                ttfb_ms: started_at.elapsed().as_millis() as u64,
            });

            if matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            ) {
                self.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                let delay = retry_delay(response.headers());
                self.extend_cooldown(&origin_state, delay).await;
                if attempt == 1 {
                    self.metrics.retries.fetch_add(1, Ordering::Relaxed);
                    // Do not buffer or drain a provider-controlled error body.
                    // Dropping it may forfeit connection reuse, but keeps retry
                    // memory bounded for chunked or compressed responses.
                    drop(response);
                    self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
                    drop(global_permit);
                    drop(origin_permit);
                    continue;
                }
            }

            return Ok(RoutedResponse {
                response,
                _origin_permit: origin_permit,
                _global_permit: global_permit,
                _in_flight: InFlightGuard {
                    metrics: Arc::clone(&self.metrics),
                },
            });
        }
    }

    async fn origin(&self, origin: &str) -> Arc<OriginState> {
        let mut origins = self.origins.lock().await;
        Arc::clone(
            origins
                .entry(origin.to_string())
                .or_insert_with(|| Arc::new(OriginState::new(self.per_origin_limit))),
        )
    }

    async fn wait_for_cooldown(&self, origin: &OriginState) {
        let delay = self.cooldown_remaining(origin).await;
        if delay > Duration::ZERO {
            sleep(delay).await;
        }
    }

    async fn cooldown_remaining(&self, origin: &OriginState) -> Duration {
        let cooldown = origin.cooldown.lock().await;
        cooldown
            .as_ref()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }

    async fn extend_cooldown(&self, origin: &OriginState, delay: Duration) {
        let candidate = Instant::now() + delay;
        let mut cooldown = origin.cooldown.lock().await;
        if cooldown.as_ref().is_none_or(|current| candidate > *current) {
            *cooldown = Some(candidate);
        }
    }
}

#[derive(Debug)]
struct InFlightGuard {
    metrics: Arc<RouterMetrics>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub struct RoutedResponse {
    response: Response,
    _origin_permit: OwnedSemaphorePermit,
    _global_permit: OwnedSemaphorePermit,
    _in_flight: InFlightGuard,
}

impl RoutedResponse {
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    pub fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    pub async fn json<T: DeserializeOwned>(self) -> std::result::Result<T, reqwest::Error> {
        self.response.json().await
    }

    pub async fn text(self) -> std::result::Result<String, reqwest::Error> {
        self.response.text().await
    }

    pub async fn bytes(self) -> std::result::Result<Vec<u8>, reqwest::Error> {
        self.response.bytes().await.map(|bytes| bytes.to_vec())
    }

    pub async fn bytes_limited(mut self, max_bytes: usize) -> Result<Vec<u8>> {
        let mut body = Vec::with_capacity(max_bytes.min(16 * 1024));
        while let Some(chunk) = self.response.chunk().await? {
            if body.len().saturating_add(chunk.len()) > max_bytes {
                return Err(ZoteroMcpError::Http(format!(
                    "response body exceeded {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

pub fn global_request_router() -> &'static RequestRouter {
    static ROUTER: OnceLock<RequestRouter> = OnceLock::new();
    ROUTER.get_or_init(RequestRouter::default)
}

fn request_origin(request: &RequestBuilder) -> Result<String> {
    let clone = request
        .try_clone()
        .ok_or_else(|| ZoteroMcpError::Http("request not cloneable for routing".to_string()))?;
    let request = clone.build()?;
    let url = request.url();
    let host = url.host_str().ok_or_else(|| {
        ZoteroMcpError::InvalidInput("outbound request URL has no host".to_string())
    })?;
    let port = url.port_or_known_default().ok_or_else(|| {
        ZoteroMcpError::InvalidInput("outbound request URL has no effective port".to_string())
    })?;
    Ok(format!(
        "{}://{}:{port}",
        url.scheme(),
        host.to_ascii_lowercase()
    ))
}

fn retry_delay(headers: &HeaderMap) -> Duration {
    let retry_after = parse_seconds_header(headers, RETRY_AFTER);
    let backoff = parse_seconds_header(headers, BACKOFF);
    let delay_ms = retry_after
        .into_iter()
        .chain(backoff)
        .max()
        .map(|seconds| seconds.saturating_mul(1_000))
        .unwrap_or(RETRY_DELAY_DEFAULT_MS)
        .min(RETRY_DELAY_CAP_MS);
    Duration::from_millis(delay_ms)
}

fn parse_seconds_header(headers: &HeaderMap, name: HeaderName) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn router_retries_once_and_records_metrics() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/paper"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/paper"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;

        let router = RequestRouter::new(2, 1);
        let response = router
            .send(
                "test_source",
                reqwest::Client::new().get(format!("{}/paper", server.uri())),
            )
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let snapshot = router.snapshot();
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.attempts, 2);
        assert_eq!(snapshot.retries, 1);
        assert_eq!(snapshot.rate_limited, 1);
        assert_eq!(snapshot.in_flight, 0);
        let event = snapshot.last_event.expect("last event");
        assert_eq!(event.component, "test_source");
        assert!(event.origin.starts_with("http://"));
        assert!(!event.origin.contains("/paper"));
    }

    #[tokio::test]
    async fn router_serializes_requests_to_same_origin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(75))
                    .set_body_string("ok"),
            )
            .expect(2)
            .mount(&server)
            .await;

        let router = Arc::new(RequestRouter::new(4, 1));
        let client = reqwest::Client::new();
        let started = Instant::now();
        let first_router = Arc::clone(&router);
        let first_client = client.clone();
        let first_url = server.uri();
        let first = async move {
            first_router
                .send("first", first_client.get(first_url))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        };
        let second_router = Arc::clone(&router);
        let second_url = server.uri();
        let second = async move {
            second_router
                .send("second", client.get(second_url))
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        };
        let (first_body, second_body) = tokio::join!(first, second);

        assert_eq!(first_body, "ok");
        assert_eq!(second_body, "ok");
        assert!(started.elapsed() >= Duration::from_millis(120));
        assert_eq!(router.snapshot().attempts, 2);
    }

    #[test]
    fn retry_delay_honors_longest_supported_header_and_cap() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "1".parse().unwrap());
        headers.insert(BACKOFF, "9".parse().unwrap());
        assert_eq!(retry_delay(&headers), Duration::from_millis(2_000));
    }

    #[tokio::test]
    async fn limited_body_reader_stops_oversized_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 4096]))
            .mount(&server)
            .await;
        let router = RequestRouter::new(1, 1);
        let response = router
            .send(
                "limited",
                reqwest::Client::new().get(format!("{}/large", server.uri())),
            )
            .await
            .unwrap();
        let error = response.bytes_limited(1024).await.unwrap_err();
        assert!(error.to_string().contains("exceeded 1024 bytes"));
        assert_eq!(router.snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn router_rejects_hostless_request() {
        let router = RequestRouter::new(1, 1);
        let error = router
            .send(
                "test_source",
                reqwest::Client::new().get("file:///tmp/paper"),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ZoteroMcpError::Http(_) | ZoteroMcpError::InvalidInput(_)
        ));
    }
}
