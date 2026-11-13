//! Per-provider rules that turn a landing page into PDF candidates, plus
//! login-page detection. Pure functions: no network, no browser.

use crate::access::{html_elements, parse_html_attributes};
use serde::Serialize;
use std::ops::Not;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Acm,
    Springer,
    Ieee,
    ScienceDirect,
    Wiley,
    Generic,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Acm => "acm",
            Provider::Springer => "springer",
            Provider::Ieee => "ieee",
            Provider::ScienceDirect => "sciencedirect",
            Provider::Wiley => "wiley",
            Provider::Generic => "generic",
        }
    }
}

/// How the browser retrieves one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchPlan {
    /// `fetch(url, {credentials: 'include'})` from the landing page; the
    /// candidate shares the landing page's origin.
    SameOrigin,
    /// Navigate the tab to the candidate, then fetch `location.href`. Needed
    /// when the link redirects to another origin (ScienceDirect `pdfft`).
    NavigateThenFetch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfCandidate {
    pub rule: &'static str,
    pub url: String,
    pub plan: FetchPlan,
}

/// Hosts under which the configured gateway serves proxied publishers: the
/// gateway host itself and, for a `login.<proxy>` gateway, `<proxy>`.
fn proxy_bases(gateway: &Url) -> Vec<String> {
    let Some(host) = gateway.host_str().map(str::to_ascii_lowercase) else {
        return Vec::new();
    };
    let mut bases = Vec::new();
    if let Some(parent) = host.strip_prefix("login.") {
        bases.push(parent.to_string());
    }
    bases.push(host);
    bases
}

/// True when `host` is `publisher` or a subdomain of it, or the form the
/// configured `gateway` serves it under: EZproxy suffix form
/// (`dl.acm.org.ezproxy.example.edu`) or hyphenated form
/// (`dl-acm-org.example.idm.oclc.org`).
///
/// Whole labels only, and proxied forms only under the configured gateway: a
/// host that merely contains a publisher's name
/// (`dl.acm.org.attacker.example`) is not that publisher.
fn host_is(host: &str, publisher: &str, gateway: Option<&Url>) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    if host == publisher || host.ends_with(&format!(".{publisher}")) {
        return true;
    }
    let hyphenated = publisher.replace('.', "-");
    gateway
        .map(proxy_bases)
        .unwrap_or_default()
        .iter()
        .any(|base| {
            host.strip_suffix(base.as_str())
                .and_then(|prefix| prefix.strip_suffix('.'))
                .is_some_and(|prefix| {
                    prefix == publisher
                        || prefix == hyphenated
                        || prefix.ends_with(&format!(".{publisher}"))
                })
        })
}

/// The provider a page belongs to. `gateway` is the configured institutional
/// gateway; proxied publisher hosts are recognised only under it.
pub fn detect_provider(page_url: &Url, gateway: Option<&Url>) -> Provider {
    let Some(host) = page_url.host_str() else {
        return Provider::Generic;
    };
    const HOSTS: &[(&str, Provider)] = &[
        ("dl.acm.org", Provider::Acm),
        ("link.springer.com", Provider::Springer),
        ("ieeexplore.ieee.org", Provider::Ieee),
        ("www.sciencedirect.com", Provider::ScienceDirect),
        ("sciencedirect.com", Provider::ScienceDirect),
        ("onlinelibrary.wiley.com", Provider::Wiley),
    ];
    HOSTS
        .iter()
        .find(|(publisher, _)| host_is(host, publisher, gateway))
        .map(|(_, provider)| *provider)
        .unwrap_or(Provider::Generic)
}

/// Percent-encode a DOI for use as a URL path, keeping the `/` separators.
fn doi_path(doi: &str) -> String {
    doi.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn allowed_scheme(candidate: &Url, page: &Url) -> bool {
    candidate.scheme() == "https" || (candidate.scheme() == "http" && page.scheme() == "http")
}

fn join(page: &Url, path_and_query: &str) -> Option<Url> {
    page.join(path_and_query).ok()
}

/// All `<meta name=... content=...>` values for one meta name. `meta` is a
/// void element, so this scans opening tags only.
fn meta_contents(html: &str, name: &str) -> Vec<String> {
    html_elements_open(html, "meta")
        .into_iter()
        .filter(|attributes| {
            ["name", "property"].iter().any(|key| {
                attributes
                    .get(*key)
                    .is_some_and(|value| value.eq_ignore_ascii_case(name))
            })
        })
        .filter_map(|attributes| {
            attributes
                .get("content")
                .map(|content| content.trim().to_string())
        })
        .filter(|content| content.is_empty().not())
        .collect()
}

fn ieee_arnumber(page: &Url, html: &str) -> Option<String> {
    let digits = |raw: &str| {
        let number: String = raw.chars().take_while(char::is_ascii_digit).collect();
        number.is_empty().not().then_some(number)
    };
    if let Some(segments) = page.path_segments() {
        let segments: Vec<&str> = segments.collect();
        if let Some(position) = segments.iter().position(|segment| *segment == "document")
            && let Some(number) = segments.get(position + 1).and_then(|raw| digits(raw))
        {
            return Some(number);
        }
    }
    if let Some((_, value)) = page.query_pairs().find(|(key, _)| key == "arnumber")
        && let Some(number) = digits(&value)
    {
        return Some(number);
    }
    for marker in ["\"arnumber\":\"", "\"articleNumber\":\"", "arnumber="] {
        if let Some(position) = html.find(marker)
            && let Some(number) = digits(&html[position + marker.len()..])
        {
            return Some(number);
        }
    }
    None
}

fn sciencedirect_candidates(page: &Url, html: &str) -> Vec<Url> {
    let mut urls: Vec<Url> = html_elements(html, "a")
        .into_iter()
        .filter_map(|anchor| anchor.attributes.get("href").cloned())
        .filter(|href| href.contains("/pdfft"))
        .filter_map(|href| join(page, &href))
        .collect();
    if urls.is_empty()
        && let Some(segments) = page.path_segments()
    {
        let segments: Vec<&str> = segments.collect();
        if let Some(position) = segments.iter().position(|segment| *segment == "pii")
            && let Some(pii) = segments.get(position + 1)
            && pii.is_empty().not()
            && pii.chars().all(|c| c.is_ascii_alphanumeric())
            && let Some(url) = join(
                page,
                &format!("/science/article/pii/{pii}/pdfft?isDTMRedir=true&download=true"),
            )
        {
            urls.push(url);
        }
    }
    urls
}

/// Map a `citation_pdf_url` onto the landing page's origin when the landing
/// host is a proxied form of the meta host, so the request carries the
/// institutional session. Otherwise the URL is kept as published.
fn localize_meta_url(page: &Url, meta: &Url, gateway: Option<&Url>) -> Url {
    let (Some(page_host), Some(meta_host)) = (page.host_str(), meta.host_str()) else {
        return meta.clone();
    };
    if same_origin(page, meta) || host_is(page_host, &meta_host.to_ascii_lowercase(), gateway).not()
    {
        return meta.clone();
    }
    let mut localized = page.clone();
    localized.set_path(meta.path());
    localized.set_query(meta.query());
    localized.set_fragment(None);
    localized
}

/// Ordered PDF candidates for a landing page. Provider rules come first; the
/// `citation_pdf_url` meta tag is the generic fallback for every provider.
pub fn pdf_candidates(
    page_url: &Url,
    doi: &str,
    html: &str,
    gateway: Option<&Url>,
) -> Vec<PdfCandidate> {
    let provider = detect_provider(page_url, gateway);
    let doi = doi_path(doi.trim());
    let mut candidates: Vec<PdfCandidate> = Vec::new();
    let mut push = |rule: &'static str, url: Option<Url>, plan: Option<FetchPlan>| {
        let Some(url) = url else { return };
        if allowed_scheme(&url, page_url).not()
            || candidates.iter().any(|known| known.url == url.as_str())
        {
            return;
        }
        let plan = plan.unwrap_or(if same_origin(page_url, &url) {
            FetchPlan::SameOrigin
        } else {
            FetchPlan::NavigateThenFetch
        });
        candidates.push(PdfCandidate {
            rule,
            url: url.to_string(),
            plan,
        });
    };

    match provider {
        Provider::Acm => push(
            "acm_doi_pdf",
            join(page_url, &format!("/doi/pdf/{doi}?download=true")),
            None,
        ),
        Provider::Springer => push(
            "springer_content_pdf",
            join(page_url, &format!("/content/pdf/{doi}.pdf")),
            None,
        ),
        Provider::Ieee => {
            if let Some(arnumber) = ieee_arnumber(page_url, html) {
                push(
                    "ieee_stamp_pdf",
                    join(
                        page_url,
                        &format!("/stampPDF/getPDF.jsp?tp=&arnumber={arnumber}&ref="),
                    ),
                    None,
                );
            }
        }
        Provider::ScienceDirect => {
            for url in sciencedirect_candidates(page_url, html) {
                push(
                    "sciencedirect_pdfft",
                    Some(url),
                    Some(FetchPlan::NavigateThenFetch),
                );
            }
        }
        Provider::Wiley => push(
            "wiley_pdfdirect",
            join(page_url, &format!("/doi/pdfdirect/{doi}?download=true")),
            None,
        ),
        Provider::Generic => {}
    }

    for content in meta_contents(html, "citation_pdf_url") {
        let meta = Url::parse(&content)
            .ok()
            .or_else(|| join(page_url, &content));
        push(
            "citation_pdf_url",
            meta.map(|meta| localize_meta_url(page_url, &meta, gateway)),
            None,
        );
    }
    candidates
}

/// What a page is, as far as its URL and title can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// A publisher or resolver page: apply the route and provider rules.
    Content,
    /// An identity provider or publisher sign-in page.
    SignIn,
    /// A proxy `/login` URL. It is a sign-in only if the markup says so
    /// (see [`login_form_is_sign_in`]); the same host serves other pages.
    LoginForm,
    /// EZproxy's "Database Menu": the proxy did not recognise the target
    /// URL. Not a sign-in; signing in again changes nothing.
    GatewayMenu,
}

fn proxy_host(host: &str) -> bool {
    host.ends_with(".idm.oclc.org")
        || host.contains("ezproxy")
        || host
            .split('.')
            .any(|label| matches!(label, "proxy" | "login"))
}

/// Classify a page. A host named `login.<gateway>` is not evidence of a
/// sign-in on its own: EZproxy serves its menu and error pages from it.
pub fn classify_page(url: &Url, title: &str) -> PageKind {
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return PageKind::Content;
    };
    let path = url.path().to_ascii_lowercase();
    let path = path.trim_end_matches('/');
    if title.trim().eq_ignore_ascii_case("database menu") || (path == "/menu" && proxy_host(&host))
    {
        return PageKind::GatewayMenu;
    }
    const IDP_HOSTS: &[&str] = &[
        "login.microsoftonline.com",
        "login.microsoftonline.us",
        "login.microsoft.com",
        "login.live.com",
        "login.windows.net",
        "accounts.google.com",
        "login.openathens.net",
        "my.openathens.net",
        "okta.com",
        "oktapreview.com",
        "duosecurity.com",
        "onelogin.com",
        "pingone.com",
        "auth0.com",
    ];
    if IDP_HOSTS
        .iter()
        .any(|idp| host == *idp || host.ends_with(&format!(".{idp}")))
    {
        return PageKind::SignIn;
    }
    // Host labels that name an identity service. `login` and `auth` are
    // absent on purpose: proxies use them for every kind of page.
    const IDP_LABELS: &[&str] = &["idp", "sso", "cas", "adfs", "shib", "shibboleth", "wayf"];
    if IDP_LABELS.contains(&host.split('.').next().unwrap_or_default()) {
        return PageKind::SignIn;
    }
    const LOGIN_PATHS: &[&str] = &[
        "/idp/",
        "/adfs/ls",
        "/cas/login",
        "/shibboleth.sso",
        "/saml/",
        "/saml2/",
        "/oauth2/authorize",
        "/action/showlogin",
        "/action/ssostart",
        "/servlet/wayf",
    ];
    let full_path = url.path().to_ascii_lowercase();
    if LOGIN_PATHS.iter().any(|marker| full_path.contains(marker)) {
        return PageKind::SignIn;
    }
    let has_target = url
        .query_pairs()
        .any(|(key, _)| matches!(key.as_ref(), "url" | "qurl"));
    if (path == "/login" || path.starts_with("/login/")) && (has_target || proxy_host(&host)) {
        return PageKind::LoginForm;
    }
    PageKind::Content
}

/// Whether a proxy `/login` page is asking for a sign-in: it has a password
/// field, or it is still holding the target URL instead of redirecting to it
/// (a single-sign-on chooser). Only the presence of the field is read.
pub fn login_form_is_sign_in(url: &Url, html: &str) -> bool {
    let has_password = html_elements_open(html, "input").iter().any(|attributes| {
        attributes
            .get("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("password"))
    });
    has_password
        || url
            .query_pairs()
            .any(|(key, _)| matches!(key.as_ref(), "url" | "qurl"))
}

/// Attributes of every `<tag ...>` opening tag (void elements included).
fn html_elements_open(html: &str, tag: &str) -> Vec<std::collections::HashMap<String, String>> {
    let lowercase = html.to_ascii_lowercase();
    let marker = format!("<{tag}");
    let mut cursor = 0;
    let mut found = Vec::new();
    while let Some(relative) = lowercase[cursor..].find(&marker) {
        let start = cursor + relative + marker.len();
        let Some(relative_end) = lowercase[start..].find('>') else {
            break;
        };
        let end = start + relative_end;
        let boundary = lowercase.as_bytes().get(start).copied();
        if boundary.is_some_and(|byte| byte.is_ascii_whitespace() || byte == b'>' || byte == b'/') {
            found.push(parse_html_attributes(
                html.get(start..end).unwrap_or_default(),
            ));
        }
        cursor = end;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACM: &str = include_str!("../../tests/fixtures/fetch/acm_landing.html");
    const SPRINGER: &str = include_str!("../../tests/fixtures/fetch/springer_landing.html");
    const IEEE: &str = include_str!("../../tests/fixtures/fetch/ieee_landing.html");
    const SCIENCEDIRECT: &str =
        include_str!("../../tests/fixtures/fetch/sciencedirect_landing.html");
    const WILEY: &str = include_str!("../../tests/fixtures/fetch/wiley_landing.html");
    const GENERIC: &str = include_str!("../../tests/fixtures/fetch/generic_landing.html");

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// The configured gateway the proxied fixtures sit behind.
    fn gateway() -> Option<Url> {
        Some(url("https://uccs.idm.oclc.org/login?url="))
    }

    #[test]
    fn provider_detection_covers_direct_and_proxied_hosts() {
        let oclc = gateway();
        let ezproxy = Some(url("https://login.ezproxy.example.edu/login?url="));
        for (raw, gateway, expected) in [
            ("https://dl.acm.org/doi/10.1145/1", &None, Provider::Acm),
            ("https://DL.ACM.ORG./doi/10.1145/1", &None, Provider::Acm),
            (
                "https://dl-acm-org.uccs.idm.oclc.org/doi/10.1145/1",
                &oclc,
                Provider::Acm,
            ),
            (
                "https://dl.acm.org.ezproxy.example.edu/doi/10.1145/1",
                &ezproxy,
                Provider::Acm,
            ),
            (
                "https://dl-acm-org.ezproxy.example.edu/doi/10.1145/1",
                &ezproxy,
                Provider::Acm,
            ),
            (
                "https://link-springer-com.uccs.idm.oclc.org/article/10.1007/x",
                &oclc,
                Provider::Springer,
            ),
            (
                "https://ieeexplore-ieee-org.uccs.idm.oclc.org/document/1",
                &oclc,
                Provider::Ieee,
            ),
            (
                "https://www-sciencedirect-com.uccs.idm.oclc.org/science/article/pii/S1",
                &oclc,
                Provider::ScienceDirect,
            ),
            (
                "https://onlinelibrary.wiley.com/doi/10.1002/x",
                &oclc,
                Provider::Wiley,
            ),
            (
                "https://www.usenix.org/conference/x",
                &oclc,
                Provider::Generic,
            ),
        ] {
            assert_eq!(
                detect_provider(&url(raw), gateway.as_ref()),
                expected,
                "{raw}"
            );
        }
    }

    #[test]
    fn hosts_that_only_contain_a_publisher_name_are_not_that_publisher() {
        let oclc = gateway();
        let ezproxy = Some(url("https://login.ezproxy.example.edu/login?url="));
        for raw in [
            // A publisher name as leading or inner labels of another domain.
            "https://dl.acm.org.attacker.example/doi/1",
            "https://evil.dl.acm.org.attacker.example/doi/1",
            "https://notdl.acm.org.evil.example/doi/1",
            "https://dl-acm-org.attacker.example/doi/1",
            "https://evil.dl-acm-org.attacker.example/doi/1",
            // A proxied form, but not under the configured gateway.
            "https://dl-acm-org.other.idm.oclc.org/doi/1",
            "https://dl.acm.org.ezproxy.other.edu/doi/1",
            // The gateway's name inside another domain.
            "https://dl-acm-org.uccs.idm.oclc.org.attacker.example/doi/1",
            "https://dl.acm.org.ezproxy.example.edu.attacker.example/doi/1",
            // Not a whole label.
            "https://notdl.acm.org/doi/1",
            "https://xdl-acm-org.uccs.idm.oclc.org/doi/1",
            "https://dl-acm-orgx.uccs.idm.oclc.org/doi/1",
            "https://dl-acm-org.evil-uccs.idm.oclc.org/doi/1",
        ] {
            for gateway in [&oclc, &ezproxy] {
                assert_eq!(
                    detect_provider(&url(raw), gateway.as_ref()),
                    Provider::Generic,
                    "{raw}"
                );
            }
        }
        // No gateway configured: only the publisher's own hosts count.
        for raw in [
            "https://dl-acm-org.uccs.idm.oclc.org/doi/1",
            "https://dl.acm.org.ezproxy.example.edu/doi/1",
        ] {
            assert_eq!(detect_provider(&url(raw), None), Provider::Generic, "{raw}");
        }
    }

    #[test]
    fn a_spoofed_host_gets_no_provider_rule_and_no_rewritten_meta_url() {
        let spoof =
            url("https://evil.onlinelibrary.wiley.com.attacker.example/doi/10.1002/spe.3200");
        let candidates = pdf_candidates(&spoof, "10.1002/spe.3200", WILEY, gateway().as_ref());
        // Only the meta tag, kept as published: nothing is built on, or
        // moved onto, the spoofed origin.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule, "citation_pdf_url");
        assert_eq!(
            candidates[0].url,
            "https://onlinelibrary.wiley.com/doi/pdf/10.1002/spe.3200"
        );
        assert_eq!(candidates[0].plan, FetchPlan::NavigateThenFetch);
    }

    #[test]
    fn acm_rule_builds_same_origin_download_url() {
        let page = url("https://dl-acm-org.uccs.idm.oclc.org/doi/10.1145/3460120.3484766");
        let candidates = pdf_candidates(&page, "10.1145/3460120.3484766", ACM, gateway().as_ref());
        assert_eq!(candidates[0].rule, "acm_doi_pdf");
        assert_eq!(
            candidates[0].url,
            "https://dl-acm-org.uccs.idm.oclc.org/doi/pdf/10.1145/3460120.3484766?download=true"
        );
        assert_eq!(candidates[0].plan, FetchPlan::SameOrigin);
    }

    #[test]
    fn springer_rule_builds_content_pdf_url() {
        let page = url("https://link.springer.com/article/10.1007/s10207-023-00803-x");
        let candidates = pdf_candidates(
            &page,
            "10.1007/s10207-023-00803-x",
            SPRINGER,
            gateway().as_ref(),
        );
        assert_eq!(candidates[0].rule, "springer_content_pdf");
        assert_eq!(
            candidates[0].url,
            "https://link.springer.com/content/pdf/10.1007/s10207-023-00803-x.pdf"
        );
        // The meta fallback names the same URL and is not repeated.
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn ieee_rule_reads_arnumber_from_url_then_html() {
        let page = url("https://ieeexplore-ieee-org.uccs.idm.oclc.org/document/9833691");
        let candidates = pdf_candidates(
            &page,
            "10.1109/SP46214.2022.9833691",
            IEEE,
            gateway().as_ref(),
        );
        assert_eq!(candidates[0].rule, "ieee_stamp_pdf");
        assert_eq!(
            candidates[0].url,
            "https://ieeexplore-ieee-org.uccs.idm.oclc.org/stampPDF/getPDF.jsp?tp=&arnumber=9833691&ref="
        );

        let abstract_page = url("https://ieeexplore.ieee.org/abstract/landing");
        let candidates = pdf_candidates(
            &abstract_page,
            "10.1109/SP46214.2022.9833691",
            IEEE,
            gateway().as_ref(),
        );
        assert!(candidates[0].url.contains("arnumber=9833691&ref="));
    }

    #[test]
    fn sciencedirect_rule_follows_pdfft_link_by_navigation() {
        let page = url(
            "https://www-sciencedirect-com.uccs.idm.oclc.org/science/article/pii/S0167404824000579",
        );
        let candidates = pdf_candidates(
            &page,
            "10.1016/j.cose.2024.103747",
            SCIENCEDIRECT,
            gateway().as_ref(),
        );
        assert_eq!(candidates[0].rule, "sciencedirect_pdfft");
        assert_eq!(candidates[0].plan, FetchPlan::NavigateThenFetch);
        assert!(candidates[0].url.starts_with(
            "https://www-sciencedirect-com.uccs.idm.oclc.org/science/article/pii/S0167404824000579/pdfft?md5="
        ));

        // No link in the markup: the rule falls back to the PII in the URL.
        let candidates = pdf_candidates(
            &page,
            "10.1016/j.cose.2024.103747",
            "<html></html>",
            gateway().as_ref(),
        );
        assert_eq!(
            candidates[0].url,
            "https://www-sciencedirect-com.uccs.idm.oclc.org/science/article/pii/S0167404824000579/pdfft?isDTMRedir=true&download=true"
        );
    }

    #[test]
    fn wiley_rule_then_meta_fallback_on_the_proxied_origin() {
        let page = url("https://onlinelibrary-wiley-com.uccs.idm.oclc.org/doi/10.1002/spe.3200");
        let candidates = pdf_candidates(&page, "10.1002/spe.3200", WILEY, gateway().as_ref());
        assert_eq!(candidates[0].rule, "wiley_pdfdirect");
        assert_eq!(
            candidates[0].url,
            "https://onlinelibrary-wiley-com.uccs.idm.oclc.org/doi/pdfdirect/10.1002/spe.3200?download=true"
        );
        assert_eq!(candidates[1].rule, "citation_pdf_url");
        // Published as onlinelibrary.wiley.com; rewritten to the proxied origin.
        assert_eq!(
            candidates[1].url,
            "https://onlinelibrary-wiley-com.uccs.idm.oclc.org/doi/pdf/10.1002/spe.3200"
        );
        assert_eq!(candidates[1].plan, FetchPlan::SameOrigin);
    }

    #[test]
    fn generic_rule_uses_citation_pdf_url_and_rejects_downgrades() {
        let page = url("https://www.usenix.org/conference/usenixsecurity23/presentation/x");
        let candidates = pdf_candidates(&page, "10.5555/x", GENERIC, gateway().as_ref());
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule, "citation_pdf_url");
        assert_eq!(
            candidates[0].url,
            "https://www.usenix.org/system/files/usenixsecurity23-x.pdf"
        );

        let cross = r#"<meta name="citation_pdf_url" content="https://cdn.example.org/a.pdf">"#;
        let candidates = pdf_candidates(&page, "10.5555/x", cross, gateway().as_ref());
        assert_eq!(candidates[0].plan, FetchPlan::NavigateThenFetch);

        let downgrade = r#"<meta name="citation_pdf_url" content="http://cdn.example.org/a.pdf">
            <meta name="citation_pdf_url" content="javascript:alert(1)">"#;
        assert!(pdf_candidates(&page, "10.5555/x", downgrade, gateway().as_ref()).is_empty());
        assert!(pdf_candidates(&page, "10.5555/x", "<html></html>", gateway().as_ref()).is_empty());
    }

    const DATABASE_MENU: &str =
        include_str!("../../tests/fixtures/fetch/ezproxy_database_menu.html");
    const MICROSOFT_LOGIN: &str = include_str!("../../tests/fixtures/fetch/microsoft_login.html");
    const EZPROXY_LOGIN: &str = include_str!("../../tests/fixtures/fetch/ezproxy_login.html");

    #[test]
    fn identity_provider_pages_are_sign_ins() {
        for raw in [
            "https://login.microsoftonline.com/common/oauth2/authorize?client_id=x",
            "https://idp.example.edu/idp/profile/SAML2/Redirect/SSO",
            "https://sso.example.edu/cas/login?service=x",
            "https://example.okta.com/app/x",
            "https://login.openathens.net/auth",
            "https://dl.acm.org/action/showLogin?redirectUri=%2Fdoi%2F10.1145%2F1",
            "https://fs.example.edu/adfs/ls/?SAMLRequest=x",
            "https://www.example.edu/Shibboleth.sso/Login",
        ] {
            assert_eq!(classify_page(&url(raw), ""), PageKind::SignIn, "{raw}");
        }
    }

    #[test]
    fn ezproxy_database_menu_is_not_a_sign_in() {
        // The live case: `login.<gateway>/menu`, titled "Database Menu".
        let menu = url("https://login.uccs.idm.oclc.org/menu");
        assert_eq!(classify_page(&menu, "Database Menu"), PageKind::GatewayMenu);
        assert_eq!(classify_page(&menu, ""), PageKind::GatewayMenu);
        assert_eq!(
            classify_page(&url("https://login.ezproxy.example.edu/menu/"), ""),
            PageKind::GatewayMenu
        );
        // The title alone is enough when a proxy serves the menu elsewhere.
        assert_eq!(
            classify_page(
                &url("https://libproxy.example.edu/start"),
                " database menu "
            ),
            PageKind::GatewayMenu
        );
        assert!(DATABASE_MENU.contains("<title>Database Menu</title>"));
        assert!(login_form_is_sign_in(&menu, DATABASE_MENU).not());
    }

    #[test]
    fn a_login_host_alone_is_not_a_sign_in() {
        for raw in [
            "https://login.uccs.idm.oclc.org/connect?session=x",
            "https://login.example.edu/help",
            "https://auth.example.org/docs",
            "https://dl-acm-org.uccs.idm.oclc.org/doi/10.1145/3460120.3484766",
            "https://link.springer.com/article/10.1007/s10207-023-00803-x",
            "https://www.example.org/login",
            "about:blank",
        ] {
            assert_eq!(
                classify_page(&url(raw), "Article"),
                PageKind::Content,
                "{raw}"
            );
        }
    }

    #[test]
    fn proxy_login_urls_need_the_markup_to_count_as_a_sign_in() {
        let with_target =
            url("https://uccs.idm.oclc.org/login?url=https://dl.acm.org/doi/10.1145/1");
        let bare = url("https://login.ezproxy.example.edu/login");
        for page in [
            &with_target,
            &bare,
            &url("https://ezproxy.example.edu/login?qurl=x"),
        ] {
            assert_eq!(classify_page(page, "Login"), PageKind::LoginForm);
        }
        // A password field is a sign-in wherever the form sits.
        assert!(login_form_is_sign_in(&bare, EZPROXY_LOGIN));
        // Still holding the target URL: a single-sign-on chooser.
        assert!(login_form_is_sign_in(
            &with_target,
            "<html><a href='/sso'>University login</a></html>"
        ));
        // A bare /login page with no password field is not one.
        assert!(login_form_is_sign_in(&bare, "<html><input type='text' name='q'></html>").not());
        // The Microsoft page is recognised by host; its markup agrees.
        assert!(login_form_is_sign_in(&bare, MICROSOFT_LOGIN));
    }
}
