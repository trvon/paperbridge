//! Chrome DevTools Protocol client for a browser the operator launched.
//!
//! Paperbridge attaches to `--remote-debugging-port` on loopback, drives one
//! tab, and reads response bodies through `Runtime.evaluate`. It never reads
//! cookies, storage, or form fields, and never types into a page: the
//! institutional session lives only in the operator's browser profile.

use crate::{Result, ZoteroMcpError};
use async_trait::async_trait;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ops::Not;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

pub const DEFAULT_CDP_URL: &str = "http://127.0.0.1:9222";

/// The launch line documented for operators. Chrome refuses remote debugging
/// on its default profile, so the profile directory is a dedicated one.
pub const CHROME_LAUNCH_LINE: &str = "google-chrome-stable --remote-debugging-port=9222 --no-first-run --no-default-browser-check --user-data-dir=\"$HOME/.local/share/paperbridge/chrome-profile\"";

const CALL_TIMEOUT: Duration = Duration::from_secs(90);
/// Raw bytes per `Runtime.evaluate` round trip when reading a fetched body.
const CHUNK_BYTES: usize = 512 * 1024;

/// One CDP request/response exchange. The seam tests replace.
#[async_trait]
pub trait CdpTransport: Send {
    async fn call(
        &mut self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value>;
}

fn connect_hint(detail: &str) -> ZoteroMcpError {
    ZoteroMcpError::Config(format!(
        "{detail}. Start Chrome yourself with a dedicated profile, sign in to your institution there, keep that window open, and retry:\n  {CHROME_LAUNCH_LINE}"
    ))
}

/// Accept only loopback DevTools endpoints: the debugging port grants full
/// control of the browser, so it must never be reached over a network.
pub fn validate_cdp_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim()).map_err(|error| {
        ZoteroMcpError::InvalidInput(format!("--cdp-url is not a valid URL: {error}"))
    })?;
    let loopback = match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if matches!(url.scheme(), "http" | "ws").not() || loopback.not() {
        return Err(ZoteroMcpError::InvalidInput(format!(
            "--cdp-url must be a loopback http:// or ws:// address such as {DEFAULT_CDP_URL}; Paperbridge does not attach to a remote browser"
        )));
    }
    if url.username().is_empty().not() || url.password().is_some() {
        return Err(ZoteroMcpError::InvalidInput(
            "--cdp-url must not contain credentials".to_string(),
        ));
    }
    Ok(url)
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// WebSocket transport to a running Chrome.
pub struct WsCdpTransport {
    socket: WsStream,
    next_id: u64,
}

#[derive(Deserialize)]
struct VersionInfo {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

impl WsCdpTransport {
    pub async fn connect(cdp_url: &str) -> Result<Self> {
        let endpoint = validate_cdp_url(cdp_url)?;
        let ws_url = if endpoint.scheme() == "ws" {
            endpoint
        } else {
            let version_url = endpoint
                .join("/json/version")
                .map_err(|error| ZoteroMcpError::InvalidInput(error.to_string()))?;
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|error| ZoteroMcpError::Http(error.to_string()))?;
            let response = client.get(version_url).send().await.map_err(|_| {
                connect_hint(&format!(
                    "No Chrome DevTools endpoint answered at {cdp_url}"
                ))
            })?;
            let info: VersionInfo = response.json().await.map_err(|_| {
                connect_hint(&format!(
                    "{cdp_url} answered, but not as a Chrome DevTools endpoint"
                ))
            })?;
            validate_cdp_url(&info.web_socket_debugger_url)?
        };
        let (socket, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|error| {
                connect_hint(&format!(
                    "Chrome DevTools WebSocket refused the connection ({error})"
                ))
            })?;
        Ok(Self { socket, next_id: 0 })
    }
}

#[async_trait]
impl CdpTransport for WsCdpTransport {
    async fn call(
        &mut self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            request["sessionId"] = Value::String(session_id.to_string());
        }
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .map_err(|error| ZoteroMcpError::Http(format!("CDP send failed: {error}")))?;
        let wait = async {
            loop {
                let Some(message) = self.socket.next().await else {
                    return Err(ZoteroMcpError::Http(
                        "Chrome closed the DevTools connection".to_string(),
                    ));
                };
                let message = message
                    .map_err(|error| ZoteroMcpError::Http(format!("CDP read failed: {error}")))?;
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                // Events and replies to abandoned calls carry no matching id.
                if value.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown CDP error");
                    return Err(ZoteroMcpError::Http(format!("{method}: {message}")));
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
        };
        tokio::time::timeout(CALL_TIMEOUT, wait)
            .await
            .map_err(|_| ZoteroMcpError::Http(format!("{method} timed out")))?
    }
}

/// How long to wait for a page to load and stop redirecting.
#[derive(Debug, Clone, Copy)]
pub struct BrowserTiming {
    pub poll: Duration,
    /// Consecutive polls with an unchanged URL before a page counts as settled.
    pub settle_polls: u32,
    pub load_timeout: Duration,
}

impl Default for BrowserTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(250),
            settle_polls: 6,
            load_timeout: Duration::from_secs(45),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PageState {
    pub url: String,
    #[serde(default)]
    pub ready: String,
    #[serde(default, rename = "contentType")]
    pub content_type: String,
    #[serde(default)]
    pub title: String,
}

/// A response body read through the page's own `fetch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedBody {
    pub status: u16,
    pub content_type: String,
    pub final_url: String,
    /// Empty when the body did not start with `%PDF-`; non-PDF bodies are
    /// never transferred out of the browser.
    pub bytes: Vec<u8>,
    pub length: usize,
}

#[derive(Deserialize)]
struct FetchMeta {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    status: u16,
    #[serde(default, rename = "contentType")]
    content_type: String,
    #[serde(default, rename = "finalUrl")]
    final_url: String,
    #[serde(default)]
    length: usize,
    #[serde(default)]
    pdf: bool,
}

pub const STATE_MARKER: &str = "/*paperbridge:state*/";
pub const HTML_MARKER: &str = "/*paperbridge:html*/";
pub const FETCH_MARKER: &str = "/*paperbridge:fetch*/";
pub const CHUNK_MARKER: &str = "/*paperbridge:chunk*/";
pub const RELEASE_MARKER: &str = "/*paperbridge:release*/";

/// Page scripts. Arguments travel as one JSON literal on its own line and are
/// never spliced into code, so a hostile URL cannot inject script. The
/// function wrapper keeps `args` out of the page's global scope.
fn script(marker: &str, args: &Value, body: &str) -> String {
    format!("{marker}\n(function (args) {{ return {body}; }})(\n{args}\n)")
}

/// The marker and arguments of a page script built by this module. Lets a
/// replacement [`CdpTransport`] recognise what the browser was asked to do.
pub fn script_parts(expression: &str) -> Option<(&str, Value)> {
    let marker = expression.lines().next()?;
    let args = expression.lines().rev().nth(1)?;
    Some((marker, serde_json::from_str(args).ok()?))
}

const STATE_BODY: &str = "JSON.stringify({url: location.href, ready: document.readyState, contentType: document.contentType || '', title: document.title || ''})";
const HTML_BODY: &str = "document.documentElement ? document.documentElement.outerHTML : ''";
const FETCH_BODY: &str = r#"(async () => {
  try {
    const response = await fetch(args.url === null ? location.href : args.url, {credentials: 'include', redirect: 'follow'});
    const declared = Number(response.headers.get('content-length') || 0);
    if (declared > args.maxBytes) return JSON.stringify({error: 'response is larger than ' + args.maxBytes + ' bytes'});
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length > args.maxBytes) return JSON.stringify({error: 'response is larger than ' + args.maxBytes + ' bytes'});
    const pdf = bytes.length >= 5 && String.fromCharCode(...bytes.subarray(0, 5)) === '%PDF-';
    globalThis.__paperbridgeBody = pdf ? bytes : null;
    return JSON.stringify({status: response.status, contentType: response.headers.get('content-type') || '', finalUrl: response.url, length: bytes.length, pdf});
  } catch (error) {
    return JSON.stringify({error: String(error)});
  }
})()"#;
const CHUNK_BODY: &str = r#"(() => {
  const slice = globalThis.__paperbridgeBody.subarray(args.start, args.end);
  let binary = '';
  for (let i = 0; i < slice.length; i += 0x8000) binary += String.fromCharCode.apply(null, slice.subarray(i, i + 0x8000));
  return btoa(binary);
})()"#;
const RELEASE_BODY: &str = "delete globalThis.__paperbridgeBody";

/// One tab opened by Paperbridge in the operator's browser.
pub struct CdpTab<'a> {
    transport: &'a mut dyn CdpTransport,
    target_id: String,
    session_id: String,
    timing: BrowserTiming,
}

impl<'a> CdpTab<'a> {
    /// Open a new tab, navigate it, and wait for the page to settle.
    pub async fn open(
        transport: &'a mut dyn CdpTransport,
        url: &str,
        timing: BrowserTiming,
    ) -> Result<(Self, PageState)> {
        let created = transport
            .call(None, "Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let target_id = string_field(&created, "targetId")?;
        let attached = transport
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session_id = string_field(&attached, "sessionId")?;
        let mut tab = Self {
            transport,
            target_id,
            session_id,
            timing,
        };
        match tab.navigate(url).await {
            Ok(state) => Ok((tab, state)),
            Err(error) => {
                let _ = tab.close().await;
                Err(error)
            }
        }
    }

    async fn session_call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.transport
            .call(Some(&self.session_id), method, params)
            .await
    }

    async fn evaluate(&mut self, expression: String) -> Result<Value> {
        let result = self
            .session_call(
                "Runtime.evaluate",
                json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details
                .pointer("/exception/description")
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("page script failed");
            return Err(ZoteroMcpError::Http(format!("page script failed: {text}")));
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn evaluate_string(&mut self, expression: String) -> Result<String> {
        match self.evaluate(expression).await? {
            Value::String(value) => Ok(value),
            other => Err(ZoteroMcpError::Http(format!(
                "page script returned {other} instead of a string"
            ))),
        }
    }

    pub async fn state(&mut self) -> Result<PageState> {
        let raw = self
            .evaluate_string(script(STATE_MARKER, &Value::Null, STATE_BODY))
            .await?;
        serde_json::from_str(&raw).map_err(|error| ZoteroMcpError::Serde(error.to_string()))
    }

    /// Navigate and wait until the document is complete and its URL has
    /// stopped changing, so proxy and identity-provider redirects finish.
    pub async fn navigate(&mut self, url: &str) -> Result<PageState> {
        let result = self
            .session_call("Page.navigate", json!({"url": url}))
            .await?;
        if let Some(error) = result.get("errorText").and_then(Value::as_str)
            && error.is_empty().not()
        {
            return Err(ZoteroMcpError::Http(format!("navigation failed: {error}")));
        }
        let deadline = tokio::time::Instant::now() + self.timing.load_timeout;
        let mut last = PageState::default();
        let mut stable = 0;
        loop {
            // Evaluation fails while a navigation swaps the document; retry.
            if let Ok(state) = self.state().await {
                let settled = state.ready == "complete" && state.url != "about:blank";
                stable = if settled && state.url == last.url {
                    stable + 1
                } else {
                    0
                };
                last = state;
                if settled && stable >= self.timing.settle_polls {
                    return Ok(last);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return if last.url.is_empty() || last.url == "about:blank" {
                    Err(ZoteroMcpError::Http(format!(
                        "page did not load within {} seconds",
                        self.timing.load_timeout.as_secs()
                    )))
                } else {
                    Ok(last)
                };
            }
            tokio::time::sleep(self.timing.poll).await;
        }
    }

    pub async fn html(&mut self) -> Result<String> {
        self.evaluate_string(script(HTML_MARKER, &Value::Null, HTML_BODY))
            .await
    }

    /// Fetch `url` (or the tab's own URL when `None`) from page context with
    /// the browser's session, then read the body out in base64 chunks.
    pub async fn fetch(&mut self, url: Option<&str>, max_bytes: usize) -> Result<FetchedBody> {
        let args = json!({"url": url, "maxBytes": max_bytes});
        let raw = self
            .evaluate_string(script(FETCH_MARKER, &args, FETCH_BODY))
            .await?;
        let meta: FetchMeta =
            serde_json::from_str(&raw).map_err(|error| ZoteroMcpError::Serde(error.to_string()))?;
        if let Some(error) = meta.error {
            return Err(ZoteroMcpError::Http(format!("page fetch failed: {error}")));
        }
        let mut bytes = Vec::new();
        if meta.pdf {
            bytes.reserve(meta.length);
            let mut start = 0;
            while start < meta.length {
                let end = (start + CHUNK_BYTES).min(meta.length);
                let args = json!({"start": start, "end": end});
                let encoded = self
                    .evaluate_string(script(CHUNK_MARKER, &args, CHUNK_BODY))
                    .await?;
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(encoded.as_bytes())
                    .map_err(|error| {
                        ZoteroMcpError::Serde(format!("browser returned invalid base64: {error}"))
                    })?;
                if chunk.len() != end - start {
                    return Err(ZoteroMcpError::Http(format!(
                        "browser returned {} bytes for a {}-byte chunk",
                        chunk.len(),
                        end - start
                    )));
                }
                bytes.extend_from_slice(&chunk);
                start = end;
            }
            let _ = self
                .evaluate(script(RELEASE_MARKER, &Value::Null, RELEASE_BODY))
                .await;
        }
        Ok(FetchedBody {
            status: meta.status,
            content_type: meta.content_type,
            final_url: meta.final_url,
            bytes,
            length: meta.length,
        })
    }

    /// Bring the tab to the front so the operator can sign in. The tab is
    /// left open; Paperbridge never fills in a login form.
    pub async fn hand_to_operator(mut self) {
        let _ = self
            .transport
            .call(
                None,
                "Target.activateTarget",
                json!({"targetId": self.target_id}),
            )
            .await;
        self.detach().await;
    }

    async fn detach(&mut self) {
        let _ = self
            .transport
            .call(
                None,
                "Target.detachFromTarget",
                json!({"sessionId": self.session_id}),
            )
            .await;
    }

    pub async fn close(self) -> Result<()> {
        self.transport
            .call(
                None,
                "Target.closeTarget",
                json!({"targetId": self.target_id}),
            )
            .await
            .map(|_| ())
    }
}

fn string_field(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| ZoteroMcpError::Http(format!("CDP reply is missing {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_url_must_be_loopback_without_credentials() {
        for raw in [
            "http://127.0.0.1:9222",
            "http://localhost:9222",
            "http://[::1]:9222",
            "ws://127.0.0.1:9222/devtools/browser/abc",
        ] {
            assert!(validate_cdp_url(raw).is_ok(), "{raw}");
        }
        for raw in [
            "http://192.168.1.20:9222",
            "http://chrome.example.org:9222",
            "https://127.0.0.1:9222",
            "http://user:pw@127.0.0.1:9222",
            "not a url",
        ] {
            assert!(validate_cdp_url(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn page_scripts_carry_arguments_as_one_json_literal() {
        let hostile = "https://x.example/a.pdf*/;alert(1)//\n</script>";
        let rendered = script(
            FETCH_MARKER,
            &json!({"url": hostile, "maxBytes": 9}),
            FETCH_BODY,
        );
        let (marker, parsed) = script_parts(&rendered).unwrap();
        assert_eq!(marker, FETCH_MARKER);
        assert_eq!(parsed["url"], hostile);
        assert!(rendered.contains("credentials: 'include'"));
    }

    #[test]
    fn launch_line_uses_a_dedicated_profile() {
        assert!(CHROME_LAUNCH_LINE.contains("--remote-debugging-port=9222"));
        assert!(CHROME_LAUNCH_LINE.contains("--user-data-dir="));
        // A fresh profile stalls on first-run screens and opens no DevTools
        // port without these.
        assert!(CHROME_LAUNCH_LINE.contains("--no-first-run"));
        assert!(CHROME_LAUNCH_LINE.contains("--no-default-browser-check"));
    }
}
