//! `papers fetch` end to end against a simulated browser.
//!
//! No publisher and no network: every `https://<host>/<path>` the simulated
//! browser opens is served by a local wiremock server at `/<host>/<path>`.
//! The simulation sits behind the real `CdpTransport` seam, so the tab
//! driver, page scripts, chunked transfer, provider rules, verification,
//! ingest, dedupe, and extraction fallback all run as shipped.

use async_trait::async_trait;
use base64::Engine;
use paperbridge::access::FetchRouteHints;
use paperbridge::config::{BackendModeConfig, Config, LibraryType};
use paperbridge::fetch::cdp::{self, BrowserTiming, CdpTransport, WsCdpTransport, script_parts};
use paperbridge::fetch::extract::Extractor;
use paperbridge::fetch::providers::Provider;
use paperbridge::fetch::{
    CdpConnector, FetchOptions, FetchRoute, FetchSession, FetchStatus, fetch_batch, fetch_doi,
    fetch_resolved,
};
use paperbridge::models::CrossrefWork;
use paperbridge::paperseed_api::PaperseedApi;
use paperbridge::service::PaperbridgeService;
use paperbridge::zotero_api::build_backend;
use paperseed::sources::PaperbridgeMetadata;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use url::Url;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DOI: &str = "10.1145/3460120.3484766";
const ACM_LANDING: &str = include_str!("fixtures/fetch/acm_landing.html");
const SCIENCEDIRECT_LANDING: &str = include_str!("fixtures/fetch/sciencedirect_landing.html");
const GENERIC_LANDING: &str = include_str!("fixtures/fetch/generic_landing.html");
const MICROSOFT_LOGIN: &str = include_str!("fixtures/fetch/microsoft_login.html");
const EZPROXY_LOGIN: &str = include_str!("fixtures/fetch/ezproxy_login.html");
const DATABASE_MENU: &str = include_str!("fixtures/fetch/ezproxy_database_menu.html");

#[derive(Default)]
struct Tab {
    url: String,
    content_type: String,
    html: String,
    title: String,
    closed: bool,
}

/// A browser simulation behind the CDP seam. It keeps tabs, follows
/// redirects, enforces the same-origin rule for page `fetch`, and records
/// every CDP method it is asked to run.
struct MockBrowser {
    origin: String,
    http: reqwest::Client,
    tabs: HashMap<String, Tab>,
    sessions: HashMap<String, String>,
    next_tab: u32,
    methods: Vec<String>,
    scripts: Vec<String>,
    activated: Vec<String>,
    body: Option<Vec<u8>>,
    chunks_served: usize,
}

struct Loaded {
    url: String,
    status: u16,
    content_type: String,
    bytes: Vec<u8>,
}

impl MockBrowser {
    fn new(server: &MockServer) -> Self {
        Self {
            origin: server.uri(),
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            tabs: HashMap::new(),
            sessions: HashMap::new(),
            next_tab: 0,
            methods: Vec::new(),
            scripts: Vec::new(),
            activated: Vec::new(),
            body: None,
            chunks_served: 0,
        }
    }

    fn real_url(&self, virtual_url: &Url) -> String {
        let mut real = format!(
            "{}/{}{}",
            self.origin,
            virtual_url.host_str().unwrap(),
            virtual_url.path()
        );
        if let Some(query) = virtual_url.query() {
            real.push('?');
            real.push_str(query);
        }
        real
    }

    async fn load(&self, start: &str) -> Loaded {
        let mut current = Url::parse(start).unwrap();
        for _ in 0..10 {
            let response = self.http.get(self.real_url(&current)).send().await.unwrap();
            if response.status().is_redirection() {
                let location = response.headers()["location"].to_str().unwrap().to_string();
                current = current.join(&location).unwrap();
                continue;
            }
            return Loaded {
                url: current.to_string(),
                status: response.status().as_u16(),
                content_type: response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
                bytes: response.bytes().await.unwrap().to_vec(),
            };
        }
        panic!("redirect loop at {start}");
    }

    fn open_tabs(&self) -> Vec<&Tab> {
        self.tabs.values().filter(|tab| !tab.closed).collect()
    }

    fn count(&self, name: &str) -> usize {
        self.methods.iter().filter(|method| *method == name).count()
    }

    async fn evaluate(&mut self, tab_id: &str, expression: &str) -> Value {
        let (marker, args) = script_parts(expression).expect("a paperbridge page script");
        self.scripts.push(marker.to_string());
        let tab_url = self.tabs[tab_id].url.clone();
        let value = match marker {
            cdp::STATE_MARKER => json!({
                "url": tab_url,
                "ready": "complete",
                "contentType": self.tabs[tab_id].content_type,
                "title": self.tabs[tab_id].title,
            })
            .to_string(),
            cdp::HTML_MARKER => self.tabs[tab_id].html.clone(),
            cdp::FETCH_MARKER => {
                let page = Url::parse(&tab_url).unwrap();
                let target = match args["url"].as_str() {
                    Some(url) => page.join(url).unwrap(),
                    None => page.clone(),
                };
                let loaded = self.load(target.as_str()).await;
                let landed = Url::parse(&loaded.url).unwrap();
                if landed.origin() != page.origin() {
                    // No CORS headers in this simulation: cross-origin reads fail.
                    json!({"error": "TypeError: Failed to fetch"}).to_string()
                } else {
                    let pdf = loaded.bytes.starts_with(b"%PDF-");
                    let length = loaded.bytes.len();
                    self.body = pdf.then_some(loaded.bytes);
                    json!({
                        "status": loaded.status,
                        "contentType": loaded.content_type,
                        "finalUrl": loaded.url,
                        "length": length,
                        "pdf": pdf,
                    })
                    .to_string()
                }
            }
            cdp::CHUNK_MARKER => {
                let start = usize::try_from(args["start"].as_u64().unwrap()).unwrap();
                let end = usize::try_from(args["end"].as_u64().unwrap()).unwrap();
                self.chunks_served += 1;
                base64::engine::general_purpose::STANDARD
                    .encode(&self.body.as_ref().expect("a fetched body")[start..end])
            }
            cdp::RELEASE_MARKER => {
                self.body = None;
                return json!({"result": {"type": "boolean", "value": true}});
            }
            other => panic!("unexpected page script {other}"),
        };
        json!({"result": {"type": "string", "value": value}})
    }
}

#[async_trait]
impl CdpTransport for MockBrowser {
    async fn call(
        &mut self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> paperbridge::Result<Value> {
        self.methods.push(method.to_string());
        let tab_of =
            |browser: &Self| browser.sessions[session_id.expect("a session-scoped call")].clone();
        Ok(match method {
            "Target.createTarget" => {
                self.next_tab += 1;
                let id = format!("TAB{}", self.next_tab);
                self.tabs.insert(
                    id.clone(),
                    Tab {
                        url: "about:blank".to_string(),
                        ..Tab::default()
                    },
                );
                json!({"targetId": id})
            }
            "Target.attachToTarget" => {
                let id = params["targetId"].as_str().unwrap().to_string();
                let session = format!("SESSION-{id}");
                self.sessions.insert(session.clone(), id);
                json!({"sessionId": session})
            }
            "Page.navigate" => {
                let id = tab_of(self);
                let loaded = self.load(params["url"].as_str().unwrap()).await;
                let html = if loaded.content_type.contains("html") {
                    String::from_utf8_lossy(&loaded.bytes).into_owned()
                } else {
                    String::new()
                };
                let tab = self.tabs.get_mut(&id).unwrap();
                tab.url = loaded.url;
                tab.content_type = loaded
                    .content_type
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                tab.title = html
                    .split_once("<title>")
                    .and_then(|(_, rest)| rest.split_once("</title>"))
                    .map(|(title, _)| title.to_string())
                    .unwrap_or_default();
                tab.html = html;
                json!({"frameId": "FRAME"})
            }
            "Runtime.evaluate" => {
                let id = tab_of(self);
                self.evaluate(&id, params["expression"].as_str().unwrap())
                    .await
            }
            "Target.activateTarget" => {
                self.activated
                    .push(params["targetId"].as_str().unwrap().to_string());
                json!({})
            }
            "Target.detachFromTarget" => json!({}),
            "Target.closeTarget" => {
                let id = params["targetId"].as_str().unwrap();
                self.tabs.get_mut(id).unwrap().closed = true;
                json!({"success": true})
            }
            other => panic!("paperbridge sent an unexpected CDP method: {other}"),
        })
    }
}

struct MockConnector(MockBrowser);

#[async_trait]
impl CdpConnector for MockConnector {
    async fn transport(&mut self) -> paperbridge::Result<&mut dyn CdpTransport> {
        Ok(&mut self.0)
    }
}

fn fast_options() -> FetchOptions {
    FetchOptions {
        timing: BrowserTiming {
            poll: Duration::from_millis(1),
            settle_polls: 2,
            load_timeout: Duration::from_secs(5),
        },
        // Deterministic on machines with and without poppler.
        pdftotext_command: "paperbridge-test-no-pdftotext".to_string(),
        ..FetchOptions::default()
    }
}

fn real_pdf(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/paperseed/tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

fn paper_a() -> Vec<u8> {
    real_pdf("arxiv_1408_5939_planar_subgraphs.pdf")
}

fn corpus(dir: &tempfile::TempDir) -> PaperseedApi {
    PaperseedApi::with_yams(
        dir.path().join("corpus"),
        None,
        paperseed::yams::YamsConfig::disabled(),
    )
}

fn work(url: &str) -> CrossrefWork {
    CrossrefWork {
        doi: DOI.to_string(),
        title: Some("Synthetic ACM landing fixture".to_string()),
        authors: vec!["Ada Lovelace".to_string()],
        year: Some("2021".to_string()),
        journal: Some("Fixture Conference".to_string()),
        abstract_note: None,
        url: Some(url.to_string()),
        resource_url: None,
        publisher: None,
        item_type: Some("proceedings-article".to_string()),
        oa_pdf_url: None,
    }
}

fn html(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/html; charset=utf-8")
}

fn pdf(bytes: Vec<u8>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(bytes, "application/pdf")
}

async fn mount_acm(server: &MockServer, pdf_response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("/dl.acm.org/doi/{DOI}")))
        .respond_with(html(ACM_LANDING))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/dl.acm.org/doi/pdf/{DOI}")))
        .and(query_param("download", "true"))
        .respond_with(pdf_response)
        .mount(server)
        .await;
}

async fn requests_to(server: &MockServer, fragment: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().contains(fragment))
        .count()
}

const ACM_ROUTE: &str = "https://dl.acm.org/doi/10.1145/3460120.3484766";

#[tokio::test]
async fn fetch_doi_ingests_the_pdf_under_its_doi_and_skips_it_next_time() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/crossref/works/10\.1145(%2F|/)3460120\.3484766$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": {
            "DOI": DOI,
            "title": ["Synthetic ACM landing fixture"],
            "author": [{"given": "Ada", "family": "Lovelace"}],
            "container-title": ["Fixture Conference"],
            "published-print": {"date-parts": [[2021, 11, 12]]},
            "URL": format!("https://doi.org/{DOI}"),
            "type": "proceedings-article"
        }})))
        .mount(&server)
        .await;
    // doi.org redirects to the (proxied) publisher landing page.
    Mock::given(method("GET"))
        .and(path(format!("/doi.org/{DOI}")))
        .respond_with(ResponseTemplate::new(302).insert_header("location", ACM_ROUTE))
        .mount(&server)
        .await;
    mount_acm(&server, pdf(paper_a())).await;

    let backend = build_backend(Config {
        backend_mode: BackendModeConfig::Cloud,
        cloud_api_base: server.uri(),
        user_id: Some(123),
        library_type: LibraryType::User,
        ..Config::default()
    })
    .unwrap();
    let service =
        PaperbridgeService::new(backend).with_crossref_base(&format!("{}/crossref", server.uri()));
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut session = FetchSession::default();

    let outcome = fetch_doi(
        &service,
        Some(&api),
        &mut connector,
        &format!("https://doi.org/{DOI}"),
        &fast_options(),
        &mut session,
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.provider, Some(Provider::Acm));
    assert_eq!(outcome.rule.as_deref(), Some("acm_doi_pdf"));
    assert_eq!(outcome.landing_url.as_deref(), Some(ACM_ROUTE));
    assert_eq!(outcome.bytes, Some(paper_a().len() as u64));
    assert!(outcome.ingested && !outcome.cached);
    assert_eq!(outcome.saved_path, None, "no --out: only the corpus copy");
    assert_eq!(outcome.extractor, Some(Extractor::Builtin));

    // The DOI is on the corpus entry, so DOI lookup finds it (the gap that
    // `paperseed corpus import` leaves).
    let entry = api
        .find_cached_identity(Some(DOI), None, None, None)
        .expect("corpus entry by DOI");
    assert_eq!(Some(&entry.paper.metadata.id), outcome.paper_id.as_ref());
    assert_eq!(entry.paper.metadata.title, "Synthetic ACM landing fixture");
    assert_eq!(entry.paper.metadata.authors, vec!["Lovelace, Ada"]);
    assert_eq!(entry.paper.metadata.year, Some(2021));
    assert_eq!(
        entry.paper.metadata.license,
        paperseed::models::License::UserOwnedPrivate
    );
    assert_eq!(std::fs::read(&entry.paper.file.path).unwrap(), paper_a());
    assert!(entry.read_full_text().unwrap().unwrap().len() > 10_000);

    // One tab, closed afterwards; one request per publisher URL.
    let browser = &connector.0;
    assert_eq!(browser.count("Target.createTarget"), 1);
    assert_eq!(browser.count("Target.closeTarget"), 1);
    assert!(browser.open_tabs().is_empty());
    assert_eq!(requests_to(&server, "/doi/pdf/").await, 1);

    // A rerun finds the paper in the corpus and never opens a tab.
    let again = fetch_doi(
        &service,
        Some(&api),
        &mut connector,
        DOI,
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(again.status, FetchStatus::Ok);
    assert!(again.cached);
    assert_eq!(again.paper_id, outcome.paper_id);
    assert_eq!(connector.0.count("Target.createTarget"), 1);
    assert_eq!(requests_to(&server, "/doi/pdf/").await, 1);

    let bad = fetch_doi(
        &service,
        Some(&api),
        &mut connector,
        "not-a-doi",
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(bad.status, FetchStatus::Failed);
    assert!(bad.reason.unwrap().contains("not a DOI"));
}

#[tokio::test]
async fn login_page_is_auth_required_and_the_tab_stays_open_for_the_operator() {
    let server = MockServer::start().await;
    // The proxied publisher bounces an unauthenticated session to the IdP.
    Mock::given(method("GET"))
        .and(path(format!("/dl.acm.org/doi/{DOI}")))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "https://login.microsoftonline.com/common/oauth2/authorize?client_id=fixture",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/login.microsoftonline.com/common/oauth2/authorize"))
        .respond_with(html(MICROSOFT_LOGIN))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut session = FetchSession::default();

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::AuthRequired, "{outcome:?}");
    assert!(
        outcome
            .landing_url
            .as_deref()
            .unwrap()
            .starts_with("https://login.microsoftonline.com/")
    );
    assert!(outcome.next.as_deref().unwrap().contains("Sign in"));
    assert!(!outcome.ingested);
    let browser = &connector.0;
    assert_eq!(browser.open_tabs().len(), 1, "the tab is left for sign-in");
    assert_eq!(browser.activated, vec!["TAB1"]);
    assert_eq!(browser.count("Target.closeTarget"), 0);
    // Paperbridge looked at the URL only: it read no markup, fetched
    // nothing, and sent no input, cookie, or storage command.
    assert!(
        browser
            .scripts
            .iter()
            .all(|marker| marker == cdp::STATE_MARKER),
        "{:?}",
        browser.scripts
    );
    assert!(browser.methods.iter().all(|method| {
        matches!(
            method.as_str(),
            "Target.createTarget"
                | "Target.attachToTarget"
                | "Page.navigate"
                | "Runtime.evaluate"
                | "Target.activateTarget"
                | "Target.detachFromTarget"
        )
    }));
    assert!(api.list_corpus_entries().unwrap().is_empty());

    // Same route host again in this run: not attempted, no second tab.
    let second = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(second.status, FetchStatus::AuthRequired);
    assert!(second.reason.unwrap().contains("not attempted"));
    assert_eq!(connector.0.count("Target.createTarget"), 1);
}

#[tokio::test]
async fn ezproxy_login_form_with_a_password_field_is_auth_required() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/example.idm.oclc.org/login"))
        .respond_with(html(EZPROXY_LOGIN))
        .mount(&server)
        .await;
    let route = format!("https://example.idm.oclc.org/login?url=https://doi.org/{DOI}");
    let mut connector = MockConnector(MockBrowser::new(&server));
    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(&route),
        &route,
        &FetchRouteHints::default(),
        &FetchOptions {
            ingest: false,
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(outcome.status, FetchStatus::AuthRequired, "{outcome:?}");
    assert_eq!(connector.0.open_tabs().len(), 1);
}

#[tokio::test]
async fn sciencedirect_pdfft_link_is_followed_by_navigation_to_the_asset_host() {
    let server = MockServer::start().await;
    let landing = "https://www-sciencedirect-com.proxy.test/science/article/pii/S0167404824000579";
    Mock::given(method("GET"))
        .and(path(
            "/www-sciencedirect-com.proxy.test/science/article/pii/S0167404824000579",
        ))
        .respond_with(html(SCIENCEDIRECT_LANDING))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/www-sciencedirect-com.proxy.test/science/article/pii/S0167404824000579/pdfft",
        ))
        .and(query_param("md5", "0123456789abcdef"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "https://pdf-assets.proxy.test/1-s2.0-S0167404824000579-main.pdf?signed=1",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/pdf-assets.proxy.test/1-s2.0-S0167404824000579-main.pdf",
        ))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(landing),
        landing,
        &proxy_test_hints(),
        &fast_options(),
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.provider, Some(Provider::ScienceDirect));
    assert_eq!(outcome.rule.as_deref(), Some("sciencedirect_pdfft"));
    assert_eq!(
        outcome.pdf_url.as_deref(),
        Some("https://pdf-assets.proxy.test/1-s2.0-S0167404824000579-main.pdf?signed=1")
    );
    assert_eq!(connector.0.count("Page.navigate"), 2);
    assert!(connector.0.open_tabs().is_empty());
}

#[tokio::test]
async fn paywall_page_and_missing_link_are_no_pdf_with_a_reason() {
    let server = MockServer::start().await;
    // The PDF URL answers with an HTML paywall, as it does without access.
    mount_acm(
        &server,
        ResponseTemplate::new(403).set_body_raw(
            b"<html>Get Access</html>".to_vec(),
            "text/html; charset=utf-8",
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/journal.example.test/article/1"))
        .respond_with(html("<html><head><title>No link</title></head></html>"))
        .mount(&server)
        .await;
    // A PDF too small to be a paper (an error stub).
    Mock::given(method("GET"))
        .and(path("/stub.example.test/article/2"))
        .respond_with(html(
            r#"<meta name="citation_pdf_url" content="https://stub.example.test/stub.pdf">"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stub.example.test/stub.pdf"))
        .respond_with(pdf(b"%PDF-1.4\n% access denied\n%%EOF\n".to_vec()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut session = FetchSession::default();

    let paywalled = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(paywalled.status, FetchStatus::NoPdf, "{paywalled:?}");
    let reason = paywalled.reason.unwrap();
    assert!(
        reason.contains("acm_doi_pdf: HTTP 403 text/html"),
        "{reason}"
    );
    assert!(reason.contains("does not start with %PDF-"), "{reason}");
    assert_eq!(
        connector.0.chunks_served, 0,
        "a non-PDF body never leaves the browser"
    );

    let unlinked = "https://journal.example.test/article/1";
    let missing = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(unlinked),
        unlinked,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(missing.status, FetchStatus::NoPdf);
    assert!(missing.reason.unwrap().contains("no PDF rule matched"));

    let stubbed = "https://stub.example.test/article/2";
    let stub = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(stubbed),
        stubbed,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(stub.status, FetchStatus::NoPdf);
    assert!(stub.reason.unwrap().contains("below the 8192-byte floor"));

    assert!(connector.0.open_tabs().is_empty(), "every tab was closed");
    assert!(api.list_corpus_entries().unwrap().is_empty());
}

#[tokio::test]
async fn large_pdf_crosses_cdp_in_chunks_and_no_ingest_saves_it_to_out() {
    let server = MockServer::start().await;
    let mut big = b"%PDF-1.7\n".to_vec();
    big.extend((0..1_300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
    big.extend_from_slice(b"\n%%EOF\n");
    Mock::given(method("GET"))
        .and(path("/www.usenix.test/conference/x"))
        .respond_with(html(
            &GENERIC_LANDING.replace("www.usenix.org", "www.usenix.test"),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/www.usenix.test/system/files/usenixsecurity23-x.pdf"))
        .respond_with(pdf(big.clone()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("pdfs");
    let mut connector = MockConnector(MockBrowser::new(&server));
    let route = "https://www.usenix.test/conference/x";

    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(route),
        route,
        &FetchRouteHints::default(),
        &FetchOptions {
            ingest: false,
            out_dir: Some(out.clone()),
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.provider, Some(Provider::Generic));
    assert_eq!(outcome.rule.as_deref(), Some("citation_pdf_url"));
    assert!(!outcome.ingested);
    let saved = out.join("10.1145_3460120.3484766.pdf");
    assert_eq!(outcome.saved_path.as_deref(), saved.to_str());
    assert_eq!(std::fs::read(saved).unwrap(), big, "bytes arrive intact");
    assert_eq!(connector.0.chunks_served, 3, "1.3 MB in 512 KiB chunks");
    assert!(
        connector.0.body.is_none(),
        "the page-side buffer is released"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn thin_builtin_text_is_replaced_by_pdftotext_and_the_report_says_so() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    mount_acm(&server, pdf(paper_a())).await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    // A stand-in pdftotext that recovers far more text than the built-in
    // extractor, across three pages.
    let script = dir.path().join("fake-pdftotext");
    std::fs::write(
        &script,
        "#!/bin/sh\n[ \"$1\" = -enc ] && [ \"$4\" = - ] || exit 2\nawk 'BEGIN { for (p = 0; p < 3; p++) { for (i = 0; i < 9000; i++) printf \"recovered body text line %d\\n\", i; printf \"\\f\" } }'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut connector = MockConnector(MockBrowser::new(&server));

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &FetchOptions {
            pdftotext_command: script.display().to_string(),
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.extractor, Some(Extractor::Pdftotext));
    let report = outcome.extraction.unwrap();
    assert_eq!(report.pages, Some(3));
    assert!(report.builtin_chars > 0);
    assert!(report.pdftotext_chars.unwrap() > 2 * report.builtin_chars);
    assert!(report.reason.contains("of pdftotext"), "{}", report.reason);
    let stored = api
        .get_cached_paper_fulltext(outcome.paper_id.as_deref().unwrap())
        .unwrap()
        .content;
    assert!(stored.starts_with("recovered body text line 0\n"));
    assert!(stored.ends_with("recovered body text line 8999"));
    // The PDF itself is kept.
    let entry = api
        .find_cached_identity(Some(DOI), None, None, None)
        .unwrap();
    assert_eq!(std::fs::read(&entry.paper.file.path).unwrap(), paper_a());
}

#[tokio::test]
async fn refetch_replaces_older_entries_for_the_same_paper_keeping_one() {
    let server = MockServer::start().await;
    mount_acm(&server, pdf(paper_a())).await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    // An older, different file already recorded under this DOI...
    let older = dir.path().join("older.pdf");
    std::fs::write(
        &older,
        real_pdf("arxiv_1808_06100_polynomial_optimization.pdf"),
    )
    .unwrap();
    let older_paper = api
        .ingest_with_metadata(
            &older,
            PaperbridgeMetadata {
                title: Some("Preprint copy".to_string()),
                doi: Some(DOI.to_uppercase()),
                arxiv_id: None,
                isbn: None,
                work_type: None,
                authors: Vec::new(),
                year: None,
                venue: None,
                abstract_note: None,
                license: Some("user-owned-private".to_string()),
                source_url: None,
            },
            None,
        )
        .unwrap();
    // ...the same bytes imported earlier without a DOI...
    let same = dir.path().join("same.pdf");
    std::fs::write(&same, paper_a()).unwrap();
    let doi_less = api.import_local_file(&same, None, None).unwrap();
    assert_eq!(doi_less.metadata.doi, None);
    // ...and an unrelated paper that must survive.
    let unrelated = dir.path().join("unrelated.txt");
    std::fs::write(&unrelated, "unrelated notes").unwrap();
    let unrelated = api.import_local_file(&unrelated, None, None).unwrap();
    assert_eq!(api.list_corpus_entries().unwrap().len(), 3);
    let mut connector = MockConnector(MockBrowser::new(&server));

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.replaced, vec![older_paper.metadata.id.clone()]);
    assert_eq!(outcome.paper_id, Some(doi_less.metadata.id.clone()));
    let entries = api.list_corpus_entries().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "one entry for the paper, plus the unrelated one"
    );
    let with_doi: Vec<_> = entries
        .iter()
        .filter(|entry| entry.paper.metadata.doi.as_deref() == Some(DOI))
        .collect();
    assert_eq!(with_doi.len(), 1);
    assert_eq!(with_doi[0].paper.metadata.id, doi_less.metadata.id);
    assert!(
        entries
            .iter()
            .any(|entry| entry.paper.metadata.id == unrelated.metadata.id)
    );
    assert!(
        !older_paper.file.path.exists(),
        "the superseded file is removed"
    );
}

#[tokio::test]
async fn batch_emits_one_outcome_per_doi_one_tab_at_a_time() {
    let server = MockServer::start().await;
    for doi in ["10.5555/one", "10.5555/two"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/crossref/works/{}",
                urlencoding::encode(doi)
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": {
                "DOI": doi,
                "title": [format!("Paper {doi}")],
                "URL": format!("https://doi.org/{doi}"),
            }})))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path_regex(r"^/crossref/works/10\.5555(%2F|/)missing$"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/doi.org/10.5555/one"))
        .respond_with(html(
            r#"<meta name="citation_pdf_url" content="https://doi.org/files/one.pdf">"#,
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/doi.org/files/one.pdf"))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/doi.org/10.5555/two"))
        .respond_with(html("<html>no link here</html>"))
        .mount(&server)
        .await;
    let backend = build_backend(Config {
        backend_mode: BackendModeConfig::Cloud,
        cloud_api_base: server.uri(),
        user_id: Some(123),
        library_type: LibraryType::User,
        ..Config::default()
    })
    .unwrap();
    let service =
        PaperbridgeService::new(backend).with_crossref_base(&format!("{}/crossref", server.uri()));
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let dois = paperbridge::fetch::parse_doi_list(
        "# three papers\n10.5555/one\n10.5555/two\n10.5555/missing\n10.5555/one\n",
    );
    let mut lines = Vec::new();
    let started = std::time::Instant::now();

    let outcomes = fetch_batch(
        &service,
        Some(&api),
        &mut connector,
        &dois,
        &fast_options(),
        Duration::from_millis(150),
        |outcome| lines.push(serde_json::to_string(outcome).unwrap()),
    )
    .await;

    let statuses: Vec<FetchStatus> = outcomes.iter().map(|outcome| outcome.status).collect();
    assert_eq!(
        statuses,
        vec![FetchStatus::Ok, FetchStatus::NoPdf, FetchStatus::Failed]
    );
    assert_eq!(lines.len(), 3);
    for (line, status) in lines.iter().zip(["ok", "no_pdf", "failed"]) {
        let value: Value = serde_json::from_str(line).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(value["status"], status);
        assert!(value["doi"].as_str().unwrap().starts_with("10.5555/"));
    }
    assert!(outcomes[2].reason.as_deref().unwrap().contains("not found"));
    // Two papers opened a tab, so the polite delay ran twice; never two
    // tabs at once.
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(connector.0.count("Target.createTarget"), 2);
    assert!(connector.0.open_tabs().is_empty());
}

const MENU: &str = include_str!("fixtures/fetch/resolver_menu.html");
const MENU_URL: &str = "https://example.search.serialssolutions.com/?url_ver=Z39.88-2004&rft_id=info:doi/10.1145/3460120.3484766";
const PROXIED_ACM: &str = "https://dl-acm-org.uccs.idm.oclc.org/doi/10.1145/3460120.3484766";

/// The gateway configured as `proxy.test`: proxied publisher hosts are
/// recognised under it and nowhere else.
fn proxy_test_hints() -> FetchRouteHints {
    FetchRouteHints {
        gateway_url: Some(Url::parse("https://proxy.test/login?url=").unwrap()),
        ..FetchRouteHints::default()
    }
}

#[tokio::test]
async fn host_that_only_contains_a_publisher_name_gets_no_provider_rule() {
    let server = MockServer::start().await;
    // `dl.acm.org` as the leading labels of somebody else's domain, and a
    // proxied form under a gateway that is not the configured one.
    let spoofs = [
        "https://evil.dl.acm.org.attacker.test/doi/10.1145/3460120.3484766",
        "https://dl-acm-org.attacker.test/doi/10.1145/3460120.3484766",
    ];
    for spoof in spoofs {
        let spoof = Url::parse(spoof).unwrap();
        Mock::given(method("GET"))
            .and(path(format!(
                "/{}{}",
                spoof.host_str().unwrap(),
                spoof.path()
            )))
            .respond_with(html("<html><head><title>Article</title></head></html>"))
            .mount(&server)
            .await;
    }
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    for spoof in spoofs {
        for hints in [FetchRouteHints::default(), proxy_test_hints()] {
            let mut connector = MockConnector(MockBrowser::new(&server));
            let outcome = fetch_resolved(
                Some(&api),
                &mut connector,
                &work(spoof),
                spoof,
                &hints,
                &fast_options(),
                &mut FetchSession::default(),
            )
            .await;
            assert_eq!(outcome.status, FetchStatus::NoPdf, "{outcome:?}");
            assert_eq!(outcome.provider, Some(Provider::Generic), "{spoof}");
        }
    }
    // The ACM rule's URL was never built on, or requested from, those hosts.
    assert_eq!(requests_to(&server, "/doi/pdf/").await, 0);
}

fn uccs_hints(doi: &str) -> FetchRouteHints {
    paperbridge::access::InstitutionAccess::new_profile(
        paperbridge::config::InstitutionAccessMode::Fallback,
        None,
        Some("https://example.search.serialssolutions.com/"),
        Some("https://uccs.idm.oclc.org/login?url="),
    )
    .unwrap()
    .fetch_route_hints(doi, None)
}

async fn mount_menu(server: &MockServer, body: &str) {
    Mock::given(method("GET"))
        .and(path("/example.search.serialssolutions.com/"))
        .respond_with(html(body))
        .mount(server)
        .await;
}

/// The gateway, for a signed-in session: `login?url=<target>` redirects on.
async fn mount_gateway(server: &MockServer, target: &str, destination: &str) {
    Mock::given(method("GET"))
        .and(path("/uccs.idm.oclc.org/login"))
        .and(query_param("url", target))
        .respond_with(ResponseTemplate::new(302).insert_header("location", destination))
        .mount(server)
        .await;
}

async fn mount_proxied_acm(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/dl-acm-org.uccs.idm.oclc.org/doi/{DOI}")))
        .respond_with(html(ACM_LANDING))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/dl-acm-org.uccs.idm.oclc.org/doi/pdf/{DOI}")))
        .and(query_param("download", "true"))
        .respond_with(pdf(paper_a()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn resolver_menu_full_text_link_leads_through_the_gateway_to_the_provider_rule() {
    let server = MockServer::start().await;
    mount_menu(&server, MENU).await;
    mount_gateway(
        &server,
        &format!("https://dl.acm.org/doi/{DOI}"),
        PROXIED_ACM,
    )
    .await;
    mount_proxied_acm(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &uccs_hints(DOI),
        &fast_options(),
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.route, Some(FetchRoute::ResolverMenu));
    assert_eq!(
        serde_json::to_value(&outcome).unwrap()["route"],
        "resolver_menu"
    );
    assert_eq!(outcome.route_url.as_deref(), Some(MENU_URL));
    assert_eq!(outcome.landing_url.as_deref(), Some(PROXIED_ACM));
    assert_eq!(outcome.provider, Some(Provider::Acm));
    assert_eq!(outcome.rule.as_deref(), Some("acm_doi_pdf"));
    assert!(
        api.find_cached_identity(Some(DOI), None, None, None)
            .is_some()
    );
    // "Full text" was followed first; the Article and Journal links and the
    // gateway DOI route were never needed.
    assert_eq!(requests_to(&server, "/uccs.idm.oclc.org/login").await, 1);
    assert_eq!(connector.0.count("Target.createTarget"), 1);
    assert!(connector.0.open_tabs().is_empty());
}

#[tokio::test]
async fn resolver_menu_without_links_falls_back_to_the_gateway_doi_route() {
    let server = MockServer::start().await;
    let doi = "10.1007/s10207-023-00803-x";
    mount_menu(
        &server,
        "<html><body><p>We could not find a direct link.</p><a href='/help'>Help</a></body></html>",
    )
    .await;
    let landing = "https://link-springer-com.uccs.idm.oclc.org/article/10.1007/s10207-023-00803-x";
    mount_gateway(&server, &format!("https://doi.org/{doi}"), landing).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/link-springer-com.uccs.idm.oclc.org/article/{doi}"
        )))
        .respond_with(html(include_str!("fixtures/fetch/springer_landing.html")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/link-springer-com.uccs.idm.oclc.org/content/pdf/{doi}.pdf"
        )))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut springer = work(MENU_URL);
    springer.doi = doi.to_string();

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &springer,
        MENU_URL,
        &uccs_hints(doi),
        &fast_options(),
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.route, Some(FetchRoute::GatewayDoi));
    assert_eq!(
        serde_json::to_value(&outcome).unwrap()["route"],
        "gateway_doi"
    );
    assert_eq!(outcome.landing_url.as_deref(), Some(landing));
    assert_eq!(outcome.provider, Some(Provider::Springer));
    assert_eq!(outcome.rule.as_deref(), Some("springer_content_pdf"));
    assert!(
        api.find_cached_identity(Some(doi), None, None, None)
            .is_some()
    );
}

#[tokio::test]
async fn resolver_menu_link_without_a_pdf_still_tries_the_gateway_doi_route() {
    let server = MockServer::start().await;
    mount_menu(&server, MENU).await;
    // Every menu link lands on a page with no PDF for this session.
    for target in [
        format!("https://dl.acm.org/doi/{DOI}"),
        format!("https://dl.acm.org/doi/abs/{DOI}"),
        "https://dl.acm.org/conference/ccs".to_string(),
    ] {
        mount_gateway(&server, &target, "https://portal.example.test/unavailable").await;
    }
    Mock::given(method("GET"))
        .and(path("/portal.example.test/unavailable"))
        .respond_with(html("<html>Not in your subscription</html>"))
        .mount(&server)
        .await;
    mount_gateway(&server, &format!("https://doi.org/{DOI}"), PROXIED_ACM).await;
    mount_proxied_acm(&server).await;
    let out = tempfile::tempdir().unwrap();
    let mut connector = MockConnector(MockBrowser::new(&server));

    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &uccs_hints(DOI),
        &FetchOptions {
            ingest: false,
            out_dir: Some(out.path().to_path_buf()),
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.route, Some(FetchRoute::GatewayDoi));
    assert_eq!(
        connector.0.count("Target.createTarget"),
        1,
        "one tab throughout"
    );
}

#[tokio::test]
async fn resolver_menu_outcomes_without_a_pdf_name_the_route_and_what_was_tried() {
    let server = MockServer::start().await;
    mount_menu(&server, MENU).await;
    // Not signed in: the gateway shows its login form instead of redirecting.
    Mock::given(method("GET"))
        .and(path("/uccs.idm.oclc.org/login"))
        .respond_with(html(EZPROXY_LOGIN))
        .mount(&server)
        .await;
    let mut connector = MockConnector(MockBrowser::new(&server));
    let options = FetchOptions {
        ingest: false,
        ..fast_options()
    };

    let signed_out = fetch_resolved(
        None,
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &uccs_hints(DOI),
        &options,
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(
        signed_out.status,
        FetchStatus::AuthRequired,
        "{signed_out:?}"
    );
    assert_eq!(signed_out.route, Some(FetchRoute::ResolverMenu));
    assert!(
        signed_out
            .landing_url
            .as_deref()
            .unwrap()
            .starts_with("https://uccs.idm.oclc.org/login?url=")
    );
    assert_eq!(connector.0.open_tabs().len(), 1, "left open for sign-in");

    // No resolver configured: the menu page is just an unknown landing page,
    // which is what the live run reported before this route existed.
    let unconfigured = fetch_resolved(
        None,
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &FetchRouteHints::default(),
        &options,
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(unconfigured.status, FetchStatus::NoPdf);
    assert_eq!(unconfigured.route, Some(FetchRoute::Direct));

    // A menu with no link and no gateway configured says so.
    let server = MockServer::start().await;
    mount_menu(&server, "<html><body>No results</body></html>").await;
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut hints = uccs_hints(DOI);
    hints.gateway_doi_url = None;
    let empty = fetch_resolved(
        None,
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &hints,
        &options,
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(empty.status, FetchStatus::NoPdf);
    assert_eq!(empty.route, Some(FetchRoute::ResolverMenu));
    assert!(
        empty
            .reason
            .as_deref()
            .unwrap()
            .contains("resolver menu: no full-text link found")
    );
    assert!(connector.0.open_tabs().is_empty());
}

const DATABASE_MENU_URL: &str = "https://login.uccs.idm.oclc.org/menu";

async fn mount_database_menu(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/login.uccs.idm.oclc.org/menu"))
        .respond_with(html(DATABASE_MENU))
        .mount(server)
        .await;
}

#[tokio::test]
async fn ezproxy_database_menu_is_gateway_menu_not_auth_required() {
    let server = MockServer::start().await;
    mount_menu(&server, "<html><body>No direct link.</body></html>").await;
    // The proxy does not recognise the target and shows its database menu.
    mount_gateway(
        &server,
        &format!("https://doi.org/{DOI}"),
        DATABASE_MENU_URL,
    )
    .await;
    mount_database_menu(&server).await;
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut session = FetchSession::default();
    let out = tempfile::tempdir().unwrap();
    let options = FetchOptions {
        ingest: false,
        out_dir: Some(out.path().to_path_buf()),
        ..fast_options()
    };

    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(MENU_URL),
        MENU_URL,
        &uccs_hints(DOI),
        &options,
        &mut session,
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::GatewayMenu, "{outcome:?}");
    assert_eq!(
        serde_json::to_value(&outcome).unwrap()["status"],
        "gateway_menu"
    );
    assert_eq!(outcome.route, Some(FetchRoute::GatewayDoi));
    assert_eq!(outcome.landing_url.as_deref(), Some(DATABASE_MENU_URL));
    let reason = outcome.reason.as_deref().unwrap();
    assert!(
        reason.starts_with("EZproxy did not recognise the target URL"),
        "{reason}"
    );
    assert!(!reason.contains("sign-in"), "{reason}");
    // Nothing for the operator to sign in to: the tab is closed.
    assert!(connector.0.open_tabs().is_empty());
    assert!(connector.0.activated.is_empty());

    // The same route host is tried again in this run: a database menu is
    // not a sign-in, so it must not block later papers.
    mount_gateway(&server, "https://doi.org/10.5555/next", PROXIED_ACM).await;
    mount_proxied_acm(&server).await;
    let mut next = work(MENU_URL);
    next.doi = "10.5555/next".to_string();
    let second = fetch_resolved(
        None,
        &mut connector,
        &next,
        MENU_URL,
        &uccs_hints("10.5555/next"),
        &options,
        &mut session,
    )
    .await;
    assert_ne!(second.status, FetchStatus::AuthRequired, "{second:?}");
    assert!(!second.reason.unwrap_or_default().contains("not attempted"));
    assert_eq!(connector.0.count("Target.createTarget"), 2);
}

#[tokio::test]
async fn gateway_routes_are_unencoded_and_the_publisher_url_is_tried_before_doi_org() {
    let server = MockServer::start().await;
    let doi = "10.1007/s10207-023-00803-x";
    mount_menu(&server, "<html><body>No direct link.</body></html>").await;
    let publisher = "https://link.springer.com/10.1007/s10207-023-00803-x";
    let landing = "https://link-springer-com.uccs.idm.oclc.org/article/10.1007/s10207-023-00803-x";
    mount_gateway(&server, publisher, landing).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/link-springer-com.uccs.idm.oclc.org/article/{doi}"
        )))
        .respond_with(html(include_str!("fixtures/fetch/springer_landing.html")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/link-springer-com.uccs.idm.oclc.org/content/pdf/{doi}.pdf"
        )))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    let hints = paperbridge::access::InstitutionAccess::new_profile(
        paperbridge::config::InstitutionAccessMode::Fallback,
        None,
        Some("https://example.search.serialssolutions.com/"),
        Some("https://uccs.idm.oclc.org/login?url="),
    )
    .unwrap()
    .fetch_route_hints(doi, Some(publisher));
    // Golden strings: the target follows `url=` unencoded.
    assert_eq!(
        hints.gateway_publisher_url.as_deref(),
        Some(
            "https://uccs.idm.oclc.org/login?url=https://link.springer.com/10.1007/s10207-023-00803-x"
        )
    );
    assert_eq!(
        hints.gateway_doi_url.as_deref(),
        Some("https://uccs.idm.oclc.org/login?url=https://doi.org/10.1007/s10207-023-00803-x")
    );
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut springer = work(MENU_URL);
    springer.doi = doi.to_string();
    let out = tempfile::tempdir().unwrap();

    let outcome = fetch_resolved(
        None,
        &mut connector,
        &springer,
        MENU_URL,
        &hints,
        &FetchOptions {
            ingest: false,
            out_dir: Some(out.path().to_path_buf()),
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.route, Some(FetchRoute::GatewayPublisher));
    assert_eq!(
        serde_json::to_value(&outcome).unwrap()["route"],
        "gateway_publisher"
    );
    // What reached the gateway on the wire.
    let gateway_queries: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/uccs.idm.oclc.org/login")
        .map(|request| request.url.query().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        gateway_queries,
        vec!["url=https://link.springer.com/10.1007/s10207-023-00803-x"]
    );
}

#[tokio::test]
async fn batch_continues_after_a_gateway_menu() {
    let server = MockServer::start().await;
    for (doi, resource) in [
        ("10.5555/unknown", "https://unknown.example.test/a/1"),
        (
            "10.1145/3460120.3484766",
            "https://dl.acm.org/doi/10.1145/3460120.3484766",
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!(
                "/crossref/works/{}",
                urlencoding::encode(doi)
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": {
                "DOI": doi,
                "title": [format!("Paper {doi}")],
                "URL": format!("https://doi.org/{doi}"),
                "resource": {"primary": {"URL": resource}},
            }})))
            .mount(&server)
            .await;
    }
    // First paper: the proxy knows neither the DOI link nor the publisher.
    mount_gateway(
        &server,
        "https://doi.org/10.5555/unknown",
        DATABASE_MENU_URL,
    )
    .await;
    mount_gateway(
        &server,
        "https://unknown.example.test/a/1",
        DATABASE_MENU_URL,
    )
    .await;
    mount_database_menu(&server).await;
    // Second paper: the DOI link is unknown too, the publisher URL works.
    mount_gateway(
        &server,
        &format!("https://doi.org/{DOI}"),
        DATABASE_MENU_URL,
    )
    .await;
    mount_gateway(
        &server,
        &format!("https://dl.acm.org/doi/{DOI}"),
        PROXIED_ACM,
    )
    .await;
    mount_proxied_acm(&server).await;
    let backend = build_backend(Config {
        backend_mode: BackendModeConfig::Cloud,
        cloud_api_base: server.uri(),
        user_id: Some(123),
        library_type: LibraryType::User,
        ..Config::default()
    })
    .unwrap();
    let service = PaperbridgeService::new(backend)
        .with_crossref_base(&format!("{}/crossref", server.uri()))
        .with_institution_profile(
            paperbridge::config::InstitutionAccessMode::Fallback,
            None,
            None,
            Some("https://uccs.idm.oclc.org/login?url=".to_string()),
        )
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let mut connector = MockConnector(MockBrowser::new(&server));
    let dois = vec!["10.5555/unknown".to_string(), DOI.to_string()];

    let outcomes = fetch_batch(
        &service,
        Some(&api),
        &mut connector,
        &dois,
        &fast_options(),
        Duration::from_millis(1),
        |_| {},
    )
    .await;

    assert_eq!(
        outcomes[0].status,
        FetchStatus::GatewayMenu,
        "{:?}",
        outcomes[0]
    );
    assert_eq!(
        outcomes[0].route_url.as_deref(),
        Some("https://uccs.idm.oclc.org/login?url=https://doi.org/10.5555/unknown")
    );
    assert!(
        outcomes[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("gateway publisher route")
    );
    assert_eq!(outcomes[1].status, FetchStatus::Ok, "{:?}", outcomes[1]);
    assert_eq!(outcomes[1].route, Some(FetchRoute::GatewayPublisher));
    assert_eq!(outcomes[1].rule.as_deref(), Some("acm_doi_pdf"));
    assert_eq!(
        connector.0.count("Target.createTarget"),
        2,
        "the second paper was attempted"
    );
    assert!(connector.0.open_tabs().is_empty());
}

#[tokio::test]
async fn unreachable_chrome_fails_with_the_launch_line_and_remote_hosts_are_refused() {
    // A loopback port with nothing listening.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut connector =
        paperbridge::fetch::ChromeConnector::new(Some(&format!("http://127.0.0.1:{port}")))
            .unwrap();
    let route = "https://journal.example.test/article/1";
    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(route),
        route,
        &FetchRouteHints::default(),
        &FetchOptions {
            ingest: false,
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(outcome.status, FetchStatus::Failed);
    let reason = outcome.reason.unwrap();
    assert!(reason.contains(cdp::CHROME_LAUNCH_LINE), "{reason}");
    assert!(reason.contains("--user-data-dir="), "{reason}");

    assert!(paperbridge::fetch::ChromeConnector::new(Some("http://10.0.0.5:9222")).is_err());
    assert!(paperbridge::fetch::ChromeConnector::new(None).is_ok());
}

/// Optional: the real WebSocket transport against a real headless Chrome
/// and the local mock server. Run with
/// `cargo test --test fetch_cdp -- --ignored real_headless_chrome`.
#[tokio::test]
#[ignore = "needs a local Chrome or Chromium binary"]
async fn real_headless_chrome_fetches_a_pdf_from_the_mock_server() {
    let chrome = std::env::var("PAPERBRIDGE_TEST_CHROME")
        .unwrap_or_else(|_| "google-chrome-stable".to_string());
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/article"))
        .respond_with(html(&format!(
            r#"<html><head><meta name="citation_pdf_url" content="{}/files/paper.pdf"></head><body>landing</body></html>"#,
            server.uri()
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/cross"))
        .respond_with(html(&format!(
            r#"<html><head><meta name="citation_pdf_url" content="{}/files/paper.pdf"></head><body>landing</body></html>"#,
            server.uri().replace("127.0.0.1", "localhost")
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/paper.pdf"))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let mut child = tokio::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--disable-gpu",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start Chrome");
    let port_file = profile.join("DevToolsActivePort");
    let mut port = None;
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(&port_file)
            && let Some(line) = text.lines().next()
            && let Ok(parsed) = line.trim().parse::<u16>()
        {
            port = Some(parsed);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let port = port.expect("Chrome wrote DevToolsActivePort");

    struct Real(WsCdpTransport);
    #[async_trait]
    impl CdpConnector for Real {
        async fn transport(&mut self) -> paperbridge::Result<&mut dyn CdpTransport> {
            Ok(&mut self.0)
        }
    }
    let mut connector = Real(
        WsCdpTransport::connect(&format!("http://127.0.0.1:{port}"))
            .await
            .unwrap(),
    );
    let options = |out: &std::path::Path| FetchOptions {
        ingest: false,
        out_dir: Some(out.to_path_buf()),
        ..FetchOptions::default()
    };
    let route = format!("{}/article", server.uri());
    let out = dir.path().join("out");
    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(&route),
        &route,
        &FetchRouteHints::default(),
        &options(&out),
        &mut FetchSession::default(),
    )
    .await;

    // Cross-origin link (`localhost` against `127.0.0.1`): the tab navigates
    // to the PDF and the viewer page fetches its own URL.
    let cross_route = format!("{}/cross", server.uri());
    let cross_out = dir.path().join("cross");
    let cross = fetch_resolved(
        None,
        &mut connector,
        &work(&cross_route),
        &cross_route,
        &FetchRouteHints::default(),
        &options(&cross_out),
        &mut FetchSession::default(),
    )
    .await;

    // The route is the PDF itself, as with an open-access link.
    let direct_route = format!("{}/files/paper.pdf", server.uri());
    let direct_out = dir.path().join("direct");
    let direct = fetch_resolved(
        None,
        &mut connector,
        &work(&direct_route),
        &direct_route,
        &FetchRouteHints::default(),
        &options(&direct_out),
        &mut FetchSession::default(),
    )
    .await;
    let _ = child.kill().await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert_eq!(outcome.rule.as_deref(), Some("citation_pdf_url"));
    assert_eq!(
        std::fs::read(out.join("10.1145_3460120.3484766.pdf")).unwrap(),
        paper_a()
    );
    assert_eq!(cross.status, FetchStatus::Ok, "{cross:?}");
    assert!(cross.pdf_url.as_deref().unwrap().contains("localhost"));
    assert_eq!(
        std::fs::read(cross_out.join("10.1145_3460120.3484766.pdf")).unwrap(),
        paper_a()
    );
    assert_eq!(direct.status, FetchStatus::Ok, "{direct:?}");
    assert_eq!(direct.rule.as_deref(), Some("direct_pdf"));
    assert_eq!(
        std::fs::read(direct_out.join("10.1145_3460120.3484766.pdf")).unwrap(),
        paper_a()
    );
}

/// The live crash: a publisher PDF whose ToUnicode CMap makes the built-in
/// extractor's parser panic. The fetch must finish and ingest the PDF.
#[cfg(unix)]
#[tokio::test]
async fn pdf_that_panics_the_builtin_extractor_is_still_ingested_via_pdftotext() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    let malformed = real_pdf("tounicode_ligature_range.pdf");
    mount_acm(&server, pdf(malformed.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let script = dir.path().join("fake-pdftotext");
    std::fs::write(
        &script,
        "#!/bin/sh\nawk 'BEGIN { for (i = 0; i < 400; i++) printf \"recovered body text line %d\\n\", i; printf \"\\f\" }'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut connector = MockConnector(MockBrowser::new(&server));
    let mut session = FetchSession::default();

    let outcome = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &FetchOptions {
            pdftotext_command: script.display().to_string(),
            ..fast_options()
        },
        &mut session,
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert!(outcome.ingested);
    assert_eq!(outcome.extractor, Some(Extractor::Pdftotext));
    let report = outcome.extraction.unwrap();
    assert!(
        report.reason.starts_with("builtin_panicked"),
        "{}",
        report.reason
    );
    assert!(
        report.reason.contains("bad length of hexstring"),
        "{}",
        report.reason
    );
    assert_eq!(report.builtin_chars, 0);
    let entry = api
        .find_cached_identity(Some(DOI), None, None, None)
        .unwrap();
    assert_eq!(std::fs::read(&entry.paper.file.path).unwrap(), malformed);
    assert!(
        entry
            .read_full_text()
            .unwrap()
            .unwrap()
            .starts_with("recovered body text line 0")
    );

    // Without pdftotext the PDF is still ingested, with no text and a reason.
    let dir = tempfile::tempdir().unwrap();
    let api = corpus(&dir);
    let untexted = fetch_resolved(
        Some(&api),
        &mut connector,
        &work(ACM_ROUTE),
        ACM_ROUTE,
        &FetchRouteHints::default(),
        &fast_options(),
        &mut session,
    )
    .await;
    assert_eq!(untexted.status, FetchStatus::Ok, "{untexted:?}");
    assert_eq!(untexted.extractor, Some(Extractor::None));
    assert!(
        untexted
            .extraction
            .unwrap()
            .reason
            .starts_with("builtin_panicked")
    );
    assert!(
        api.find_cached_identity(Some(DOI), None, None, None)
            .is_some()
    );
}

struct HeadlessConnector(MockBrowser);

#[async_trait]
impl CdpConnector for HeadlessConnector {
    async fn transport(&mut self) -> paperbridge::Result<&mut dyn CdpTransport> {
        Ok(&mut self.0)
    }

    fn headless(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn sign_in_page_in_a_headless_browser_says_to_run_once_without_headless() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/login.microsoftonline.com/common/oauth2/authorize"))
        .respond_with(html(MICROSOFT_LOGIN))
        .mount(&server)
        .await;
    let route = "https://login.microsoftonline.com/common/oauth2/authorize?client_id=fixture";
    let mut connector = HeadlessConnector(MockBrowser::new(&server));
    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(route),
        route,
        &FetchRouteHints::default(),
        &FetchOptions {
            ingest: false,
            ..fast_options()
        },
        &mut FetchSession::default(),
    )
    .await;
    assert_eq!(outcome.status, FetchStatus::AuthRequired, "{outcome:?}");
    let next = outcome.next.unwrap();
    assert!(
        next.contains("Run once without --headless to sign in"),
        "{next}"
    );
}

/// Optional: the connector starts Chrome when nothing answers, fetches, and
/// closes the browser it started.
#[tokio::test]
#[ignore = "needs a local Chrome or Chromium binary"]
async fn connector_launches_headless_chrome_fetches_and_closes_it() {
    use paperbridge::fetch::launch;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/article"))
        .respond_with(html(&format!(
            r#"<html><head><meta name="citation_pdf_url" content="{}/files/paper.pdf"></head></html>"#,
            server.uri()
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/files/paper.pdf"))
        .respond_with(pdf(paper_a()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("http://127.0.0.1:{port}");
    let mut connector = paperbridge::fetch::ChromeConnector::new(Some(&endpoint))
        .unwrap()
        .with_launch(launch::LaunchSpec {
            command: launch::resolve_chrome(
                std::env::var("PAPERBRIDGE_TEST_CHROME").ok().as_deref(),
                None,
            )
            .unwrap(),
            profile: profile.clone(),
            headless: true,
        });
    assert!(
        !connector.launched(),
        "nothing starts until a fetch needs it"
    );
    let route = format!("{}/article", server.uri());
    let out = dir.path().join("out");

    let outcome = fetch_resolved(
        None,
        &mut connector,
        &work(&route),
        &route,
        &FetchRouteHints::default(),
        &FetchOptions {
            ingest: false,
            out_dir: Some(out),
            ..FetchOptions::default()
        },
        &mut FetchSession::default(),
    )
    .await;

    assert_eq!(outcome.status, FetchStatus::Ok, "{outcome:?}");
    assert!(connector.launched() && connector.headless());
    assert_eq!(launch::restore_on_startup(&profile), Some(true));
    assert!(launch::endpoint_browser(&endpoint).await.is_some());
    assert!(
        connector.finish(false).await,
        "the started browser is closed"
    );
    assert!(launch::endpoint_browser(&endpoint).await.is_none());
    assert!(!launch::profile_in_use(&profile));
}

/// Launch headless Chrome on `profile`, visit `url`, and close Chrome cleanly.
async fn visit_once(profile: &std::path::Path, url: &str, cookie_host: &str) -> usize {
    use paperbridge::fetch::launch;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("http://127.0.0.1:{port}");
    let spec = launch::LaunchSpec {
        command: launch::resolve_chrome(
            std::env::var("PAPERBRIDGE_TEST_CHROME").ok().as_deref(),
            None,
        )
        .unwrap(),
        profile: profile.to_path_buf(),
        headless: true,
    };
    launch::launch_chrome(&endpoint, &spec).await.unwrap();
    let mut transport = WsCdpTransport::connect(&endpoint).await.unwrap();
    let (tab, _) = cdp::CdpTab::open(&mut transport, url, BrowserTiming::default())
        .await
        .unwrap();
    tab.close().await.unwrap();
    let cookies = launch::cookie_summary(&mut transport, cookie_host)
        .await
        .unwrap();
    drop(transport);
    assert!(
        launch::stop_browser(&endpoint, profile, false)
            .await
            .unwrap()
            .stopped
    );
    assert!(
        !launch::profile_in_use(profile),
        "Chrome released the profile"
    );
    cookies.session
}

/// Optional, and the actual check of the persistence mechanism: does a
/// session cookie survive a browser restart with
/// `session.restore_on_startup = 1`, and is it dropped without it? Run with
/// `cargo test --test fetch_cdp -- --ignored session_cookie --nocapture`.
#[tokio::test]
#[ignore = "needs a local Chrome or Chromium binary"]
async fn session_cookie_survives_a_restart_only_with_restore_on_startup() {
    use paperbridge::fetch::launch;
    let server = MockServer::start().await;
    // A session cookie: no Expires, no Max-Age, like EZproxy's.
    Mock::given(method("GET"))
        .and(path("/set"))
        .respond_with(
            html("<html>signed in</html>").insert_header("set-cookie", "pbsession=fixture; Path=/"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/check"))
        .respond_with(html("<html>check</html>"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut survived = Vec::new();
    for (name, restore) in [("restore", true), ("default", false)] {
        let profile = dir.path().join(name);
        if restore {
            assert_eq!(
                launch::ensure_restore_on_startup(&profile).unwrap(),
                launch::PreferencesEdit::Created
            );
        }
        let set = visit_once(&profile, &format!("{}/set", server.uri()), "127.0.0.1").await;
        assert_eq!(set, 1, "the browser held the session cookie before closing");
        // Chrome kept (or wrote) the setting through its first run.
        assert_eq!(launch::restore_on_startup(&profile), Some(restore));
        let marker = format!("/check?profile={name}");
        let held_after_restart =
            visit_once(&profile, &format!("{}{marker}", server.uri()), "127.0.0.1").await;
        let sent = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.query() == Some(&format!("profile={name}")))
            .any(|request| {
                request
                    .headers
                    .get("cookie")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|cookie| cookie.contains("pbsession=fixture"))
            });
        println!(
            "restore_on_startup={restore}: session cookie sent after restart = {sent}, cookies held after restart = {held_after_restart}"
        );
        survived.push(sent);
    }
    assert_eq!(survived, vec![true, false], "[with restore, without]");
}
