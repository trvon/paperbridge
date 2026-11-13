//! `papers fetch`: retrieve a paper's PDF through the operator's signed-in
//! Chrome, verify it, and ingest it into the Paperseed corpus under its DOI.
//!
//! Paperbridge holds no institutional username, password, or cookie. The
//! session stays in the browser profile the operator launched; when a page
//! asks for a sign-in, the tab is handed to the operator and the fetch stops.

pub mod cdp;
pub mod extract;
pub mod launch;
pub mod providers;

use crate::access::FetchRouteHints;
use crate::models::CrossrefWork;
use crate::paperseed_api::PaperseedApi;
use crate::service::PaperbridgeService;
use crate::{Result, ZoteroMcpError};
use async_trait::async_trait;
use cdp::{BrowserTiming, CdpTab, CdpTransport, PageState, WsCdpTransport};
use extract::{ExtractionReport, Extractor};
use paperseed::db::IndexedPaper;
use paperseed::sources::PaperbridgeMetadata;
use providers::{FetchPlan, PageKind, Provider};
use serde::Serialize;
use std::collections::HashSet;
use std::ops::Not;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

pub const FETCH_LICENSE: &str = "user-owned-private";
pub const DEFAULT_BATCH_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Directory that keeps a copy of the PDF. `None` keeps only the corpus copy.
    pub out_dir: Option<PathBuf>,
    pub ingest: bool,
    /// Fetch even when the corpus already holds full text for the DOI.
    pub force: bool,
    pub min_pdf_bytes: usize,
    pub timing: BrowserTiming,
    pub pdftotext_command: String,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            out_dir: None,
            ingest: true,
            force: false,
            min_pdf_bytes: extract::MIN_PDF_BYTES,
            timing: BrowserTiming::default(),
            pdftotext_command: "pdftotext".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FetchStatus {
    Ok,
    AuthRequired,
    NoPdf,
    /// The proxy showed its database menu: it did not recognise the target
    /// URL. Not a sign-in problem.
    GatewayMenu,
    #[default]
    Failed,
}

/// One paper's result; also one JSON line of a batch.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct FetchOutcome {
    pub doi: String,
    pub status: FetchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// How the publisher page was reached: `direct`, `resolver_menu`, or
    /// `gateway_doi`. Absent when no browser tab was opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<FetchRoute>,
    /// The provider rule that produced the PDF URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    pub ingested: bool,
    /// True when the corpus already held the paper and no browser tab opened.
    pub cached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_id: Option<String>,
    /// Corpus entries removed because they duplicated this DOI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<String>,
    /// Which extractor's text was stored as the full text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor: Option<Extractor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extraction: Option<ExtractionReport>,
    /// What the operator should do next, when there is something to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

impl FetchOutcome {
    fn new(doi: &str) -> Self {
        Self {
            doi: doi.to_string(),
            ..Self::default()
        }
    }

    fn failed(mut self, reason: impl Into<String>) -> Self {
        self.status = FetchStatus::Failed;
        self.reason = Some(reason.into());
        self
    }
}

/// How the browser reached the publisher page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FetchRoute {
    /// The resolved access route led straight to the publisher (or the PDF).
    #[default]
    Direct,
    /// The route was the link resolver's menu page; one of its full-text
    /// links was followed.
    ResolverMenu,
    /// The sign-in gateway wrapping the publisher's landing page.
    GatewayPublisher,
    /// The sign-in gateway wrapping `https://doi.org/<doi>`.
    GatewayDoi,
}

/// Result of the browser stage alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserFetch {
    Pdf {
        bytes: Vec<u8>,
        landing_url: String,
        pdf_url: String,
        provider: Provider,
        rule: String,
        route: FetchRoute,
    },
    AuthRequired {
        tab_url: String,
        reason: String,
        route: FetchRoute,
    },
    NoPdf {
        landing_url: String,
        provider: Provider,
        reason: String,
        route: FetchRoute,
    },
    /// Every route ended on the proxy's database menu.
    GatewayMenu {
        tab_url: String,
        reason: String,
        route: FetchRoute,
    },
}

/// Resolver-menu links followed before falling back to the gateway route.
const MAX_MENU_LINKS: usize = 3;

/// Open `route_url` in a new tab, find the PDF by provider rule, and return
/// its verified bytes. A sign-in page ends the attempt with the tab left
/// open for the operator; every other outcome closes the tab.
pub async fn fetch_pdf_via_browser(
    transport: &mut dyn CdpTransport,
    route_url: &str,
    doi: &str,
    hints: &FetchRouteHints,
    options: &FetchOptions,
) -> Result<BrowserFetch> {
    let (mut tab, state) = CdpTab::open(transport, route_url, options.timing).await?;
    let result = drive_tab(&mut tab, state, route_url, doi, hints, options).await;
    match &result {
        Ok(BrowserFetch::AuthRequired { .. }) => tab.hand_to_operator().await,
        _ => {
            let _ = tab.close().await;
        }
    }
    result
}

const GATEWAY_MENU_REASON: &str = "EZproxy did not recognise the target URL";

/// A URL alone (a fetch's redirect target): sign-in when it is an identity
/// provider or a proxy login URL.
fn login_url(raw: &str) -> bool {
    Url::parse(raw).is_ok_and(|url| {
        matches!(
            providers::classify_page(&url, ""),
            PageKind::SignIn | PageKind::LoginForm
        )
    })
}

/// Classify the page the tab is on. A proxy `/login` URL is settled by its
/// markup: only the presence of a password field is read.
async fn page_kind(tab: &mut CdpTab<'_>, state: &PageState) -> Result<PageKind> {
    let Ok(url) = Url::parse(&state.url) else {
        return Ok(PageKind::Content);
    };
    Ok(match providers::classify_page(&url, &state.title) {
        PageKind::LoginForm => {
            let html = tab.html().await?;
            if providers::login_form_is_sign_in(&url, &html) {
                PageKind::SignIn
            } else {
                PageKind::Content
            }
        }
        kind => kind,
    })
}

/// What one landing page yielded.
enum Landing {
    Pdf {
        bytes: Vec<u8>,
        pdf_url: String,
        rule: String,
    },
    Auth {
        tab_url: String,
        what: &'static str,
    },
    NoPdf {
        attempts: Vec<String>,
    },
}

fn finish(
    landing: Landing,
    page: &PageState,
    route: FetchRoute,
    notes: &[String],
    gateway: Option<&Url>,
) -> BrowserFetch {
    let provider = Url::parse(&page.url)
        .map(|url| providers::detect_provider(&url, gateway))
        .unwrap_or(Provider::Generic);
    match landing {
        Landing::Pdf {
            bytes,
            pdf_url,
            rule,
        } => BrowserFetch::Pdf {
            bytes,
            landing_url: page.url.clone(),
            pdf_url,
            provider,
            rule,
            route,
        },
        Landing::Auth { tab_url, what } => BrowserFetch::AuthRequired {
            tab_url,
            reason: format!("{what} is a sign-in page; Paperbridge does not enter credentials"),
            route,
        },
        Landing::NoPdf { attempts } => {
            let mut all: Vec<String> = notes.to_vec();
            all.extend(attempts);
            let reason = if all.is_empty() {
                format!(
                    "no PDF rule matched the {} landing page and it has no citation_pdf_url meta tag",
                    provider.as_str()
                )
            } else {
                format!(
                    "no candidate returned a PDF (the session may lack access): {}",
                    all.join("; ")
                )
            };
            BrowserFetch::NoPdf {
                landing_url: page.url.clone(),
                provider,
                reason,
                route,
            }
        }
    }
}

async fn drive_tab(
    tab: &mut CdpTab<'_>,
    landing: PageState,
    route_url: &str,
    doi: &str,
    hints: &FetchRouteHints,
    options: &FetchOptions,
) -> Result<BrowserFetch> {
    // Proxied publisher hosts are recognised only under this gateway.
    let gateway = hints.gateway_url.as_ref();
    let sign_in = |state: &PageState, route: FetchRoute| {
        finish(
            Landing::Auth {
                tab_url: state.url.clone(),
                what: "the access route",
            },
            state,
            route,
            &[],
            gateway,
        )
    };
    let gateway_routes = || {
        [
            (
                FetchRoute::GatewayPublisher,
                "gateway publisher route",
                hints.gateway_publisher_url.as_ref(),
            ),
            (
                FetchRoute::GatewayDoi,
                "gateway DOI route",
                hints.gateway_doi_url.as_ref(),
            ),
        ]
        .into_iter()
        .filter_map(|(route, label, url)| url.map(|url| (route, label.to_string(), url.clone())))
        .filter(|(_, _, url)| url != route_url)
    };

    let mut notes: Vec<String> = Vec::new();
    let mut routes: Vec<(FetchRoute, String, String)> = Vec::new();
    // The last proxy database menu seen, and whether any route reached a
    // page the provider rules could be applied to.
    let mut gateway_menu: Option<(PageState, FetchRoute)> = None;
    let mut reached_content = false;
    let on_resolver_menu = Url::parse(&landing.url).is_ok_and(|page| hints.is_resolver_menu(&page));
    match page_kind(tab, &landing).await? {
        PageKind::SignIn => return Ok(sign_in(&landing, FetchRoute::Direct)),
        PageKind::GatewayMenu => {
            notes.push(format!("direct: {GATEWAY_MENU_REASON}"));
            gateway_menu = Some((landing.clone(), FetchRoute::Direct));
            routes.extend(gateway_routes());
        }
        _ if on_resolver_menu => {
            // The link resolver showed its menu instead of redirecting.
            // Follow its full-text links, then fall back to the gateway.
            let html = tab.html().await?;
            let links = match Url::parse(&landing.url) {
                Ok(menu) => hints.menu_links(&menu, &html),
                Err(_) => Vec::new(),
            };
            if links.is_empty() {
                notes.push("resolver menu: no full-text link found".to_string());
            }
            routes.extend(links.into_iter().take(MAX_MENU_LINKS).map(|link| {
                (
                    FetchRoute::ResolverMenu,
                    format!("resolver link to {}", link.provider),
                    link.url,
                )
            }));
            routes.extend(gateway_routes());
        }
        _ => {
            let result = try_landing(tab, &landing, doi, gateway, options).await?;
            return Ok(finish(result, &landing, FetchRoute::Direct, &[], gateway));
        }
    }

    let mut last_page = landing.clone();
    let mut last_route = if on_resolver_menu {
        FetchRoute::ResolverMenu
    } else {
        FetchRoute::Direct
    };
    for (route, label, url) in routes {
        let state = match tab.navigate(&url).await {
            Ok(state) => state,
            Err(error) => {
                notes.push(format!("{label}: {error}"));
                continue;
            }
        };
        if Url::parse(&state.url).is_ok_and(|page| hints.is_resolver_menu(&page)) {
            notes.push(format!("{label}: stayed on the resolver"));
            continue;
        }
        match page_kind(tab, &state).await? {
            PageKind::SignIn => return Ok(sign_in(&state, route)),
            PageKind::GatewayMenu => {
                notes.push(format!("{label}: {GATEWAY_MENU_REASON} ({url})"));
                gateway_menu = Some((state, route));
                continue;
            }
            _ => {}
        }
        reached_content = true;
        last_route = route;
        match try_landing(tab, &state, doi, gateway, options).await? {
            Landing::NoPdf { attempts } => {
                if attempts.is_empty() {
                    notes.push(format!("{label}: no PDF rule matched {}", state.url));
                }
                notes.extend(
                    attempts
                        .into_iter()
                        .map(|attempt| format!("{label}: {attempt}")),
                );
                last_page = state;
            }
            found => return Ok(finish(found, &state, route, &[], gateway)),
        }
    }
    if let (false, Some((page, route))) = (reached_content, gateway_menu) {
        return Ok(BrowserFetch::GatewayMenu {
            tab_url: page.url,
            reason: format!("{GATEWAY_MENU_REASON}: {}", notes.join("; ")),
            route,
        });
    }
    Ok(finish(
        Landing::NoPdf {
            attempts: Vec::new(),
        },
        &last_page,
        last_route,
        &notes,
        gateway,
    ))
}

/// Apply the provider rules to one landing page.
async fn try_landing(
    tab: &mut CdpTab<'_>,
    landing: &PageState,
    doi: &str,
    gateway: Option<&Url>,
    options: &FetchOptions,
) -> Result<Landing> {
    let page = Url::parse(&landing.url).map_err(|error| {
        ZoteroMcpError::Http(format!("browser reported an invalid page URL: {error}"))
    })?;
    let mut attempts: Vec<String> = Vec::new();

    // An open-access route can be the PDF itself.
    if landing.content_type.eq_ignore_ascii_case("application/pdf")
        && let Ok(body) = tab.fetch(None, extract::MAX_PDF_BYTES).await
    {
        match extract::verify_pdf(&body.bytes, options.min_pdf_bytes) {
            Ok(()) => {
                return Ok(Landing::Pdf {
                    bytes: body.bytes,
                    pdf_url: landing.url.clone(),
                    rule: "direct_pdf".to_string(),
                });
            }
            Err(rejection) => attempts.push(format!("direct_pdf: {rejection}")),
        }
    }

    let html = tab.html().await?;
    let mut on_landing = true;
    for candidate in providers::pdf_candidates(&page, doi, &html, gateway) {
        let fetched = match candidate.plan {
            FetchPlan::SameOrigin => {
                if on_landing.not() {
                    tab.navigate(&landing.url).await?;
                    on_landing = true;
                }
                tab.fetch(Some(&candidate.url), extract::MAX_PDF_BYTES)
                    .await
            }
            FetchPlan::NavigateThenFetch => match tab.navigate(&candidate.url).await {
                Ok(state) => {
                    on_landing = false;
                    match page_kind(tab, &state).await? {
                        PageKind::SignIn => {
                            return Ok(Landing::Auth {
                                tab_url: state.url,
                                what: "the PDF link",
                            });
                        }
                        PageKind::GatewayMenu => {
                            attempts.push(format!("{}: {GATEWAY_MENU_REASON}", candidate.rule));
                            continue;
                        }
                        _ => {}
                    }
                    tab.fetch(None, extract::MAX_PDF_BYTES).await
                }
                // Chrome aborts a navigation that turns into a download; ask
                // the page for the same URL instead.
                Err(_) => {
                    tab.fetch(Some(&candidate.url), extract::MAX_PDF_BYTES)
                        .await
                }
            },
        };
        match fetched {
            Ok(body) => {
                if login_url(&body.final_url) {
                    return Ok(Landing::Auth {
                        tab_url: body.final_url,
                        what: "the PDF link's destination",
                    });
                }
                match extract::verify_pdf(&body.bytes, options.min_pdf_bytes) {
                    Ok(()) => {
                        return Ok(Landing::Pdf {
                            bytes: body.bytes,
                            pdf_url: if body.final_url.is_empty() {
                                candidate.url
                            } else {
                                body.final_url
                            },
                            rule: candidate.rule.to_string(),
                        });
                    }
                    Err(rejection) => attempts.push(format!(
                        "{}: HTTP {} {} ({} bytes): {rejection}",
                        candidate.rule, body.status, body.content_type, body.length
                    )),
                }
            }
            Err(error) => attempts.push(format!("{}: {error}", candidate.rule)),
        }
    }
    Ok(Landing::NoPdf { attempts })
}

/// Paperseed metadata for a Crossref record, so the corpus entry carries the
/// DOI and `papers open --doi` finds it.
pub fn metadata_from_crossref(work: &CrossrefWork) -> PaperbridgeMetadata {
    let year = work.year.as_deref().and_then(|raw| {
        raw.split(|c: char| c.is_ascii_digit().not())
            .find(|part| part.len() == 4)
            .and_then(|part| part.parse::<u16>().ok())
    });
    let work_type = match work.item_type.as_deref().map(str::to_ascii_lowercase) {
        Some(kind) if matches!(kind.as_str(), "book" | "monograph" | "edited-book") => "book",
        Some(kind) if matches!(kind.as_str(), "book-chapter" | "book-section") => "chapter",
        _ => "paper",
    };
    PaperbridgeMetadata {
        title: work
            .title
            .clone()
            .filter(|title| title.trim().is_empty().not()),
        doi: Some(work.doi.clone()),
        arxiv_id: None,
        isbn: None,
        work_type: Some(work_type.to_string()),
        authors: work.authors.clone(),
        year,
        venue: work.journal.clone(),
        abstract_note: work.abstract_note.clone(),
        license: Some(FETCH_LICENSE.to_string()),
        source_url: Some(
            work.url
                .clone()
                .unwrap_or_else(|| format!("https://doi.org/{}", work.doi)),
        ),
    }
}

fn title_key(title: &str) -> String {
    title
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Ids of corpus entries the freshly ingested paper supersedes: another file
/// recorded under the same DOI, or a DOI-less entry with the same title. The
/// entry holding `kept_hash` is the one that stays.
pub fn dedupe_targets(
    entries: &[IndexedPaper],
    kept_hash: &str,
    doi: &str,
    title: Option<&str>,
) -> Vec<String> {
    let doi = crate::internals::normalize_doi(doi);
    let title = title.map(title_key).filter(|key| key.is_empty().not());
    entries
        .iter()
        .filter(|entry| entry.paper.file.hash != kept_hash)
        .filter(|entry| {
            let metadata = &entry.paper.metadata;
            match metadata.doi.as_deref() {
                Some(existing) => doi.is_some() && crate::internals::normalize_doi(existing) == doi,
                None => title
                    .as_ref()
                    .is_some_and(|title| *title == title_key(&metadata.title)),
            }
        })
        .map(|entry| entry.paper.metadata.id.clone())
        .collect()
}

#[derive(Debug, Clone)]
pub struct IngestReport {
    pub paper_id: String,
    pub replaced: Vec<String>,
    pub extraction: ExtractionReport,
}

/// Ingest a verified PDF with Crossref metadata, replace thin built-in text
/// with `pdftotext` output when that recovers more, and drop duplicates.
pub async fn ingest_pdf(
    api: &PaperseedApi,
    work: &CrossrefWork,
    pdf: &Path,
    pdftotext_command: &str,
) -> Result<IngestReport> {
    let metadata = metadata_from_crossref(work);
    let license = Some(FETCH_LICENSE.to_string());
    let (paper, builtin_error) =
        match api.ingest_with_metadata_options(pdf, metadata.clone(), license.clone(), true) {
            Ok(paper) => (paper, None),
            // The built-in extractor rejected the file; keep the PDF and let
            // pdftotext supply the text.
            Err(error) => (
                api.ingest_with_metadata_options(pdf, metadata, license, false)?,
                Some(error.to_string()),
            ),
        };
    let paper_id = paper.metadata.id.clone();
    let builtin_text = if builtin_error.is_some() {
        None
    } else {
        paperseed::app::get_entry(api.paths(), &paper_id)
            .ok()
            .and_then(|entry| entry.read_full_text().ok().flatten())
    };
    let builtin_chars = builtin_text.as_deref().map_or(0, extract::text_chars);
    let pdftotext = extract::run_pdftotext(pdftotext_command, &paper.file.path).await;
    let measured = pdftotext
        .as_ref()
        .map(|output| (extract::text_chars(&output.text), output.pages));
    let (extractor, mut reason) = extract::select_extractor(builtin_chars, measured);
    if let Some(error) = builtin_error {
        let kind = if error.contains(paperseed::app::PDF_EXTRACTOR_PANICKED) {
            "builtin_panicked"
        } else {
            "builtin_failed"
        };
        let detail = error.lines().next().unwrap_or_default();
        reason = format!("{kind}: {detail}; {reason}");
    }
    if extractor == Extractor::Pdftotext
        && let Some(output) = pdftotext
    {
        api.replace_full_text(&paper_id, output.text)?;
    }

    let entries = api.list_corpus_entries()?;
    let mut replaced = Vec::new();
    for id in dedupe_targets(&entries, &paper.file.hash, &work.doi, work.title.as_deref()) {
        api.remove_corpus_entry(&id)?;
        replaced.push(id);
    }
    Ok(IngestReport {
        paper_id,
        replaced,
        extraction: ExtractionReport {
            extractor,
            reason,
            builtin_chars,
            pdftotext_chars: measured.map(|(chars, _)| chars),
            pages: measured.map(|(_, pages)| pages),
        },
    })
}

/// File name for a DOI: path separators and shell-hostile characters become `_`.
pub fn pdf_file_name(doi: &str) -> String {
    let stem: String = doi
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.pdf", stem.trim_matches('.'))
}

/// Supplies the browser connection on first use, so a paper already in the
/// corpus never needs Chrome.
#[async_trait]
pub trait CdpConnector: Send {
    async fn transport(&mut self) -> Result<&mut dyn CdpTransport>;

    /// True when the browser has no window the operator could sign in to.
    fn headless(&self) -> bool {
        false
    }
}

pub struct ChromeConnector {
    cdp_url: String,
    transport: Option<WsCdpTransport>,
    launch: Option<launch::LaunchSpec>,
    launched: bool,
}

impl ChromeConnector {
    pub fn new(cdp_url: Option<&str>) -> Result<Self> {
        let cdp_url = cdp_url.unwrap_or(cdp::DEFAULT_CDP_URL).trim().to_string();
        cdp::validate_cdp_url(&cdp_url)?;
        Ok(Self {
            cdp_url,
            transport: None,
            launch: None,
            launched: false,
        })
    }

    /// Start Chrome on first use when no endpoint answers.
    pub fn with_launch(mut self, spec: launch::LaunchSpec) -> Self {
        self.launch = Some(spec);
        self
    }

    /// True when this connector started the browser.
    pub fn launched(&self) -> bool {
        self.launched
    }

    /// Close the browser if this connector started it and `keep` is false.
    /// Returns true when a browser was closed.
    pub async fn finish(&mut self, keep: bool) -> bool {
        self.transport = None;
        if self.launched.not() || keep {
            return false;
        }
        self.launched = false;
        let Some(spec) = self.launch.as_ref() else {
            return false;
        };
        launch::stop_browser(&self.cdp_url, &spec.profile, false)
            .await
            .is_ok_and(|report| report.stopped)
    }
}

#[async_trait]
impl CdpConnector for ChromeConnector {
    async fn transport(&mut self) -> Result<&mut dyn CdpTransport> {
        let transport = match self.transport.take() {
            Some(transport) => transport,
            None => {
                if let Some(spec) = self.launch.as_ref() {
                    let report = launch::ensure_browser(&self.cdp_url, spec).await?;
                    self.launched |= report.launched;
                }
                WsCdpTransport::connect(&self.cdp_url).await?
            }
        };
        Ok(self.transport.insert(transport))
    }

    fn headless(&self) -> bool {
        self.launched && self.launch.as_ref().is_some_and(|spec| spec.headless)
    }
}

/// State shared across one invocation: hosts whose route asked for a sign-in
/// are not opened again, so a batch leaves one login tab instead of many.
#[derive(Debug, Default)]
pub struct FetchSession {
    blocked_hosts: HashSet<String>,
    /// True when the last paper opened a browser tab (the batch delay applies).
    pub touched_browser: bool,
}

fn host_of(raw: &str) -> Option<String> {
    Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
}

const SIGN_IN_NEXT: &str = "Sign in to your institution in the Chrome tab Paperbridge left open, then rerun the same command.";
const HEADLESS_SIGN_IN_NEXT: &str = "The browser is headless, so there is no window to sign in to. Run once without --headless to sign in, then rerun with --headless.";

/// Fetch a paper whose Crossref record and route are already resolved.
pub async fn fetch_resolved(
    api: Option<&PaperseedApi>,
    connector: &mut dyn CdpConnector,
    work: &CrossrefWork,
    route_url: &str,
    hints: &FetchRouteHints,
    options: &FetchOptions,
    session: &mut FetchSession,
) -> FetchOutcome {
    session.touched_browser = false;
    let mut outcome = FetchOutcome::new(&work.doi);
    outcome.title = work.title.clone();
    outcome.route_url = Some(route_url.to_string());
    if options.ingest && api.is_none() {
        return outcome.failed(
            "the Paperseed corpus is not available; set paperseed_enabled=true or pass --no-ingest with --out",
        );
    }
    let route_host = host_of(route_url);
    if let Some(host) = route_host.as_ref()
        && session.blocked_hosts.contains(host)
    {
        outcome.status = FetchStatus::AuthRequired;
        outcome.reason = Some(format!(
            "not attempted: {host} already asked for a sign-in during this run"
        ));
        outcome.next = Some(SIGN_IN_NEXT.to_string());
        return outcome;
    }

    // Connect (and launch, when asked to) before reading whether the
    // browser is headless: that is only known once it has started.
    if let Err(error) = connector.transport().await {
        return outcome.failed(error.to_string());
    }
    session.touched_browser = true;
    let headless = connector.headless();
    let transport = match connector.transport().await {
        Ok(transport) => transport,
        Err(error) => return outcome.failed(error.to_string()),
    };
    let fetched = match fetch_pdf_via_browser(transport, route_url, &work.doi, hints, options).await
    {
        Ok(fetched) => fetched,
        Err(error) => return outcome.failed(error.to_string()),
    };
    let bytes = match fetched {
        BrowserFetch::AuthRequired {
            tab_url,
            reason,
            route,
        } => {
            outcome.route = Some(route);
            if let Some(host) = route_host {
                session.blocked_hosts.insert(host);
            }
            outcome.status = FetchStatus::AuthRequired;
            outcome.landing_url = Some(tab_url);
            outcome.reason = Some(reason);
            outcome.next = Some(
                if headless {
                    HEADLESS_SIGN_IN_NEXT
                } else {
                    SIGN_IN_NEXT
                }
                .to_string(),
            );
            return outcome;
        }
        BrowserFetch::GatewayMenu {
            tab_url,
            reason,
            route,
        } => {
            outcome.route = Some(route);
            outcome.status = FetchStatus::GatewayMenu;
            outcome.landing_url = Some(tab_url);
            outcome.reason = Some(reason);
            outcome.next = Some(
                "The proxy has no entry for this URL. Check the route with `paperbridge papers access --doi <doi> --no-open`, or open the publisher page through your library by hand."
                    .to_string(),
            );
            return outcome;
        }
        BrowserFetch::NoPdf {
            landing_url,
            provider,
            reason,
            route,
        } => {
            outcome.route = Some(route);
            outcome.status = FetchStatus::NoPdf;
            outcome.landing_url = Some(landing_url);
            outcome.provider = Some(provider);
            outcome.reason = Some(reason);
            return outcome;
        }
        BrowserFetch::Pdf {
            bytes,
            landing_url,
            pdf_url,
            provider,
            rule,
            route,
        } => {
            outcome.route = Some(route);
            outcome.landing_url = Some(landing_url);
            outcome.pdf_url = Some(pdf_url);
            outcome.provider = Some(provider);
            outcome.rule = Some(rule);
            outcome.bytes = u64::try_from(bytes.len()).ok();
            bytes
        }
    };

    let scratch = match tempfile::tempdir() {
        Ok(scratch) => scratch,
        Err(error) => {
            return outcome.failed(format!("could not create a scratch directory: {error}"));
        }
    };
    let file_name = pdf_file_name(&work.doi);
    let (directory, keep) = match options.out_dir.as_deref() {
        Some(directory) => (directory.to_path_buf(), true),
        None if options.ingest => (scratch.path().to_path_buf(), false),
        None => (PathBuf::from("."), true),
    };
    let path = directory.join(&file_name);
    if let Err(error) =
        std::fs::create_dir_all(&directory).and_then(|()| std::fs::write(&path, &bytes))
    {
        return outcome.failed(format!("could not write {}: {error}", path.display()));
    }
    if keep {
        outcome.saved_path = Some(path.display().to_string());
    }

    if let (true, Some(api)) = (options.ingest, api) {
        match ingest_pdf(api, work, &path, &options.pdftotext_command).await {
            Ok(report) => {
                outcome.ingested = true;
                outcome.paper_id = Some(report.paper_id);
                outcome.replaced = report.replaced;
                outcome.extractor = Some(report.extraction.extractor);
                outcome.extraction = Some(report.extraction);
            }
            Err(error) => {
                return outcome.failed(format!("the PDF was fetched but ingest failed: {error}"));
            }
        }
    }
    outcome.status = FetchStatus::Ok;
    outcome
}

/// Fetch one DOI end to end: corpus check, Crossref metadata, access route,
/// browser fetch, ingest. Never returns `Err`; failures are outcomes.
pub async fn fetch_doi(
    service: &PaperbridgeService,
    api: Option<&PaperseedApi>,
    connector: &mut dyn CdpConnector,
    doi: &str,
    options: &FetchOptions,
    session: &mut FetchSession,
) -> FetchOutcome {
    session.touched_browser = false;
    let Some(doi) = crate::internals::normalize_doi(doi) else {
        return FetchOutcome::new(doi.trim())
            .failed("not a DOI; expected the form 10.<registrant>/<suffix>");
    };
    if options.ingest
        && options.force.not()
        && let Some(api) = api
        && let Some(entry) = api.find_cached_identity(Some(&doi), None, None, None)
        && entry.has_full_text()
    {
        let mut outcome = FetchOutcome::new(&doi);
        outcome.status = FetchStatus::Ok;
        outcome.cached = true;
        outcome.ingested = true;
        outcome.title = Some(entry.paper.metadata.title.clone());
        outcome.paper_id = Some(entry.paper.metadata.id.clone());
        outcome.reason =
            Some("already in the corpus with full text; pass --force to fetch again".to_string());
        return outcome;
    }
    let (work, access) = match service.resolve_doi_access(&doi).await {
        Ok(resolved) => resolved,
        Err(error) => return FetchOutcome::new(&doi).failed(error.to_string()),
    };
    if access.browser_open_allowed.not() {
        let mut outcome = FetchOutcome::new(&doi);
        outcome.title = work.title.clone();
        outcome.route_url = Some(access.selected_url);
        return outcome.failed(
            "the resolver returned a cross-origin or unverified route, so Paperbridge did not open it; inspect it with `paperbridge papers access --no-open`",
        );
    }
    let hints = service.fetch_route_hints(&doi, work.resource_url.as_deref());
    fetch_resolved(
        api,
        connector,
        &work,
        &access.selected_url,
        &hints,
        options,
        session,
    )
    .await
}

/// DOIs from a list file: one per line, blank lines and `#` comments
/// skipped, duplicates dropped, order kept.
pub fn parse_doi_list(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| line.is_empty().not())
        .map(|line| crate::internals::normalize_doi(line).unwrap_or_else(|| line.to_string()))
        .filter(|doi| seen.insert(doi.clone()))
        .collect()
}

/// Process a list one tab at a time, pausing `delay` after every paper that
/// opened a browser tab. `emit` receives each outcome as it completes.
pub async fn fetch_batch(
    service: &PaperbridgeService,
    api: Option<&PaperseedApi>,
    connector: &mut dyn CdpConnector,
    dois: &[String],
    options: &FetchOptions,
    delay: Duration,
    mut emit: impl FnMut(&FetchOutcome) + Send,
) -> Vec<FetchOutcome> {
    let mut session = FetchSession::default();
    let mut outcomes = Vec::with_capacity(dois.len());
    for (index, doi) in dois.iter().enumerate() {
        let outcome = fetch_doi(service, api, connector, doi, options, &mut session).await;
        emit(&outcome);
        outcomes.push(outcome);
        if session.touched_browser && index + 1 < dois.len() {
            tokio::time::sleep(delay).await;
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use paperseed::models::{License, LocalPaper, PaperMetadata, StoredFile};

    fn work() -> CrossrefWork {
        CrossrefWork {
            doi: "10.1145/3460120.3484766".to_string(),
            title: Some("A Synthetic Paper: On Fixtures".to_string()),
            authors: vec!["Ada Lovelace".to_string(), "Alan Turing".to_string()],
            year: Some("2021-11-12".to_string()),
            journal: Some("Proceedings of the Fixture Conference".to_string()),
            abstract_note: Some("An abstract.".to_string()),
            url: Some("https://doi.org/10.1145/3460120.3484766".to_string()),
            resource_url: None,
            publisher: Some("ACM".to_string()),
            item_type: Some("proceedings-article".to_string()),
            oa_pdf_url: None,
        }
    }

    fn entry(hash: &str, doi: Option<&str>, title: &str) -> IndexedPaper {
        IndexedPaper {
            paper: LocalPaper {
                metadata: PaperMetadata {
                    id: hash[..12.min(hash.len())].to_string(),
                    title: title.to_string(),
                    doi: doi.map(ToOwned::to_owned),
                    arxiv_id: None,
                    isbn: None,
                    work_type: None,
                    authors: Vec::new(),
                    year: None,
                    venue: None,
                    abstract_note: None,
                    license: License::UserOwnedPrivate,
                    source_url: None,
                },
                file: StoredFile {
                    hash: hash.to_string(),
                    path: PathBuf::from(format!("/corpus/{hash}.pdf")),
                    size_bytes: 1,
                    mime: "application/pdf".to_string(),
                },
            },
            full_text: None,
            full_text_path: None,
            yams_hash: None,
        }
    }

    #[test]
    fn crossref_record_maps_to_ingest_metadata_with_doi() {
        let metadata = metadata_from_crossref(&work());
        assert_eq!(metadata.doi.as_deref(), Some("10.1145/3460120.3484766"));
        assert_eq!(
            metadata.title.as_deref(),
            Some("A Synthetic Paper: On Fixtures")
        );
        assert_eq!(metadata.authors, vec!["Ada Lovelace", "Alan Turing"]);
        assert_eq!(metadata.year, Some(2021));
        assert_eq!(
            metadata.venue.as_deref(),
            Some("Proceedings of the Fixture Conference")
        );
        assert_eq!(metadata.license.as_deref(), Some("user-owned-private"));
        assert_eq!(metadata.work_type.as_deref(), Some("paper"));
        assert_eq!(
            metadata.source_url.as_deref(),
            Some("https://doi.org/10.1145/3460120.3484766")
        );

        let mut chapter = work();
        chapter.item_type = Some("book-chapter".to_string());
        chapter.url = None;
        chapter.year = None;
        chapter.title = Some("  ".to_string());
        let metadata = metadata_from_crossref(&chapter);
        assert_eq!(metadata.work_type.as_deref(), Some("chapter"));
        assert_eq!(metadata.year, None);
        assert_eq!(metadata.title, None);
        assert_eq!(
            metadata.source_url.as_deref(),
            Some("https://doi.org/10.1145/3460120.3484766")
        );
    }

    #[test]
    fn dedupe_replaces_same_doi_and_doi_less_title_matches_only() {
        let entries = vec![
            entry(
                "aaaaaaaaaaaaaaaa",
                Some("10.1145/3460120.3484766"),
                "new copy",
            ),
            entry(
                "bbbbbbbbbbbbbbbb",
                Some("https://doi.org/10.1145/3460120.3484766"),
                "older file, same DOI",
            ),
            entry("cccccccccccccccc", None, "a synthetic paper - on fixtures"),
            entry("dddddddddddddddd", None, "Unrelated import"),
            entry(
                "eeeeeeeeeeeeeeee",
                Some("10.1145/9999999.0000001"),
                "A Synthetic Paper: On Fixtures",
            ),
        ];
        let targets = dedupe_targets(
            &entries,
            "aaaaaaaaaaaaaaaa",
            "10.1145/3460120.3484766",
            Some("A Synthetic Paper: On Fixtures"),
        );
        assert_eq!(targets, vec!["bbbbbbbbbbbb", "cccccccccccc"]);
        // Without a title, only DOI matches are replaced.
        let targets = dedupe_targets(
            &entries,
            "aaaaaaaaaaaaaaaa",
            "10.1145/3460120.3484766",
            None,
        );
        assert_eq!(targets, vec!["bbbbbbbbbbbb"]);
    }

    #[test]
    fn doi_list_skips_comments_blanks_and_duplicates() {
        let list = "# batch for the survey\n10.1145/3460120.3484766\n\nhttps://doi.org/10.1007/S10207-023-00803-x  # springer\n10.1145/3460120.3484766\nnot-a-doi\n";
        assert_eq!(
            parse_doi_list(list),
            vec![
                "10.1145/3460120.3484766",
                "10.1007/s10207-023-00803-x",
                "not-a-doi"
            ]
        );
    }

    #[test]
    fn pdf_file_names_are_flat_and_safe() {
        assert_eq!(
            pdf_file_name("10.1145/3460120.3484766"),
            "10.1145_3460120.3484766.pdf"
        );
        assert_eq!(
            pdf_file_name("10.1002/(SICI)1097<3>;../x"),
            "10.1002__SICI_1097_3__.._x.pdf"
        );
    }

    #[test]
    fn outcome_serializes_spec_status_names() {
        for (status, wire) in [
            (FetchStatus::Ok, "ok"),
            (FetchStatus::AuthRequired, "auth_required"),
            (FetchStatus::NoPdf, "no_pdf"),
            (FetchStatus::GatewayMenu, "gateway_menu"),
            (FetchStatus::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), wire);
        }
    }
}
