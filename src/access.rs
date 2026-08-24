use crate::config::InstitutionAccessMode;
use crate::error::{Result, ZoteroMcpError};
use crate::request_router::global_request_router;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::time::Duration;
use url::Url;

const RESOLVER_RESPONSE_LIMIT: usize = 2_000_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstitutionGatewayKind {
    Ezproxy,
    OpenAthens,
    /// Retained for backward-compatible JSON. New profiles report OpenURL
    /// through `resolver` instead of treating it as an authentication gateway.
    OpenUrl,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InstitutionResolverKind {
    OpenUrl,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HoldingsStatus {
    NotChecked,
    Available,
    Unavailable,
    Unparsed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct AccessOption {
    pub provider: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_url: Option<String>,
    pub authentication_required: bool,
    /// True only when the outer URL belongs to the configured resolver or
    /// authentication gateway and is safe for automatic browser launch.
    pub auto_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct SourceAccessResolution {
    pub mode: InstitutionAccessMode,
    pub target_url: String,
    pub selected_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub institution_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<InstitutionGatewayKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver: Option<InstitutionResolverKind>,
    pub holdings_status: HoldingsStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_options: Vec<AccessOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver_error: Option<String>,
    pub authentication_required: bool,
    pub browser_open_allowed: bool,
}

pub(crate) struct HoldingsLookup {
    pub status: HoldingsStatus,
    pub options: Vec<AccessOption>,
}

#[derive(Clone)]
pub struct InstitutionAccess {
    mode: InstitutionAccessMode,
    resolver_url: Option<Url>,
    gateway_url: Option<Url>,
    gateway: Option<InstitutionGatewayKind>,
    client: reqwest::Client,
}

impl std::fmt::Debug for InstitutionAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstitutionAccess")
            .field("mode", &self.mode)
            .field("resolver_configured", &self.resolver_url.is_some())
            .field("gateway", &self.gateway)
            .finish_non_exhaustive()
    }
}

impl InstitutionAccess {
    pub fn new(mode: InstitutionAccessMode, legacy_url: Option<&str>) -> Result<Self> {
        Self::new_profile(mode, legacy_url, None, None)
    }

    pub fn new_profile(
        mode: InstitutionAccessMode,
        legacy_url: Option<&str>,
        resolver_url: Option<&str>,
        gateway_url: Option<&str>,
    ) -> Result<Self> {
        let legacy = parse_config_url("institution_access_url", legacy_url)?;
        let mut resolver = parse_config_url("institution_resolver_url", resolver_url)?;
        let mut gateway_base = parse_config_url("institution_gateway_url", gateway_url)?;

        if let Some(legacy) = legacy {
            match detect_gateway(&legacy) {
                InstitutionGatewayKind::OpenUrl if resolver.is_none() => resolver = Some(legacy),
                InstitutionGatewayKind::Ezproxy | InstitutionGatewayKind::OpenAthens
                    if gateway_base.is_none() =>
                {
                    gateway_base = Some(legacy);
                }
                _ => {}
            }
        }

        if resolver
            .as_ref()
            .is_some_and(|url| detect_gateway(url) != InstitutionGatewayKind::OpenUrl)
        {
            return Err(ZoteroMcpError::Config(
                "institution_resolver_url looks like an authentication gateway; use institution_gateway_url instead"
                    .to_string(),
            ));
        }

        let gateway = gateway_base.as_ref().map(detect_gateway);
        if gateway == Some(InstitutionGatewayKind::OpenUrl) {
            return Err(ZoteroMcpError::Config(
                "institution_gateway_url looks like an OpenURL resolver; use institution_resolver_url instead"
                    .to_string(),
            ));
        }

        if mode != InstitutionAccessMode::Off && resolver.is_none() && gateway_base.is_none() {
            return Err(ZoteroMcpError::MissingConfig(
                "an institutional resolver or gateway URL is required when institutional access is enabled"
                    .to_string(),
            ));
        }

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("paperbridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| ZoteroMcpError::Http(error.to_string()))?;

        Ok(Self {
            mode,
            resolver_url: resolver,
            gateway_url: gateway_base,
            gateway,
            client,
        })
    }

    pub fn disabled() -> Self {
        Self::new_profile(InstitutionAccessMode::Off, None, None, None)
            .unwrap_or_else(|_| unreachable!("disabled institutional access is always valid"))
    }

    pub fn has_resolver(&self) -> bool {
        self.mode != InstitutionAccessMode::Off && self.resolver_url.is_some()
    }

    pub fn resolve(&self, target: &str, doi: Option<&str>) -> Result<SourceAccessResolution> {
        self.resolve_with_policy(target, doi, true)
    }

    pub fn resolve_direct(
        &self,
        target: &str,
        doi: Option<&str>,
    ) -> Result<SourceAccessResolution> {
        self.resolve_with_policy(target, doi, false)
    }

    pub(crate) async fn resolve_holdings(
        &self,
        target: &str,
        doi: Option<&str>,
    ) -> Result<HoldingsLookup> {
        if self.has_resolver().not() {
            return Ok(HoldingsLookup {
                status: HoldingsStatus::NotChecked,
                options: Vec::new(),
            });
        }
        let target = parse_target_url(target)?;
        let Some(resolver_base) = self.resolver_url.as_ref() else {
            return Ok(HoldingsLookup {
                status: HoldingsStatus::NotChecked,
                options: Vec::new(),
            });
        };
        let resolver_url = build_openurl(resolver_base, &target, doi);
        let mut current = resolver_url.clone();

        for hop in 0..=4 {
            let response = global_request_router()
                .send("institution-resolver", self.client.get(current.clone()))
                .await?;
            let status = response.status();
            if status.is_redirection() {
                if hop == 4 {
                    return Err(ZoteroMcpError::Http(
                        "institutional resolver exceeded four redirects".to_string(),
                    ));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        ZoteroMcpError::Http(
                            "institutional resolver redirect omitted Location".to_string(),
                        )
                    })?;
                let next = current.join(location).map_err(|error| {
                    ZoteroMcpError::Http(format!(
                        "institutional resolver returned an invalid redirect: {error}"
                    ))
                })?;
                validate_redirect_target(&next)?;
                if same_origin(&next, resolver_base) {
                    current = next;
                    continue;
                }
                return Ok(HoldingsLookup {
                    status: HoldingsStatus::Available,
                    options: vec![self.access_option(next, false)],
                });
            }
            if status.is_success().not() {
                return Err(ZoteroMcpError::Api {
                    status: status.as_u16(),
                    message: "institutional resolver request failed".to_string(),
                });
            }

            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if content_type.starts_with("application/pdf") {
                return Ok(HoldingsLookup {
                    status: HoldingsStatus::Available,
                    options: vec![self.access_option(current, true)],
                });
            }
            if content_type.is_empty().not()
                && content_type.contains("html").not()
                && content_type.starts_with("text/").not()
            {
                return Ok(HoldingsLookup {
                    status: HoldingsStatus::Unparsed,
                    options: Vec::new(),
                });
            }
            if response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|length| length > RESOLVER_RESPONSE_LIMIT)
            {
                return Err(ZoteroMcpError::Http(
                    "institutional resolver response exceeded 2 MB".to_string(),
                ));
            }
            let bytes = response.bytes_limited(RESOLVER_RESPONSE_LIMIT).await?;
            let html = String::from_utf8_lossy(&bytes);
            let options = parse_openurl_options(&current, &html, self.gateway_url.as_ref())?;
            let status = if options.is_empty() {
                if explicitly_unavailable(&html) {
                    HoldingsStatus::Unavailable
                } else {
                    HoldingsStatus::Unparsed
                }
            } else {
                HoldingsStatus::Available
            };
            return Ok(HoldingsLookup { status, options });
        }

        Err(ZoteroMcpError::Http(
            "institutional resolver redirect handling failed".to_string(),
        ))
    }

    pub(crate) fn apply_holdings(
        &self,
        resolution: &mut SourceAccessResolution,
        mut lookup: HoldingsLookup,
    ) {
        lookup.options.sort_by_key(access_option_rank);
        resolution.holdings_status = lookup.status;
        if let Some(selected) = lookup.options.iter().find(|option| option.auto_open) {
            apply_selected_option(resolution, selected, true);
        } else if let Some(selected) = lookup.options.first() {
            apply_selected_option(resolution, selected, false);
        }
        resolution.access_options = lookup.options;
    }

    fn access_option(&self, url: Url, allow_resolver_origin: bool) -> AccessOption {
        let destination = deepest_destination(&url);
        let authentication_required = url_chain(&url)
            .iter()
            .any(|candidate| detect_gateway_only(candidate).is_some());
        let auto_open = (allow_resolver_origin
            && self
                .resolver_url
                .as_ref()
                .is_some_and(|resolver| same_origin(&url, resolver)))
            || self
                .gateway_url
                .as_ref()
                .is_some_and(|gateway| same_origin(&url, gateway));
        AccessOption {
            provider: destination
                .host_str()
                .map(provider_name_from_host)
                .unwrap_or_else(|| "institutional provider".to_string()),
            url: url.to_string(),
            destination_url: (destination != url).then(|| destination.to_string()),
            authentication_required,
            auto_open,
        }
    }

    fn resolve_with_policy(
        &self,
        target: &str,
        doi: Option<&str>,
        use_institution: bool,
    ) -> Result<SourceAccessResolution> {
        let target_url = parse_target_url(target)?;
        let institution_enabled = self.mode != InstitutionAccessMode::Off
            && (use_institution || self.mode == InstitutionAccessMode::Prefer);
        let use_resolver = institution_enabled
            && self.resolver_url.is_some()
            && (doi.is_some() || self.gateway_url.is_none());

        let (institution_url, gateway, resolver, authentication_required) = if use_resolver {
            let url = self
                .resolver_url
                .as_ref()
                .map(|base| build_openurl(base, &target_url, doi));
            (url, None, Some(InstitutionResolverKind::OpenUrl), false)
        } else if institution_enabled {
            match (self.gateway_url.as_ref(), self.gateway) {
                (Some(base), Some(kind)) => (
                    Some(build_gateway_url(base, kind, &target_url)),
                    Some(kind),
                    None,
                    true,
                ),
                _ => (None, None, None, false),
            }
        } else {
            (None, None, None, false)
        };

        let selected_url = institution_url
            .as_ref()
            .map_or_else(|| target_url.as_str(), Url::as_str)
            .to_string();
        Ok(SourceAccessResolution {
            mode: self.mode,
            target_url: target_url.to_string(),
            selected_url,
            institution_url: institution_url.map(Into::into),
            gateway,
            resolver,
            holdings_status: HoldingsStatus::NotChecked,
            access_options: Vec::new(),
            resolver_error: None,
            authentication_required,
            browser_open_allowed: true,
        })
    }
}

pub fn detect_gateway(url: &Url) -> InstitutionGatewayKind {
    detect_gateway_only(url).unwrap_or(InstitutionGatewayKind::OpenUrl)
}

fn detect_gateway_only(url: &Url) -> Option<InstitutionGatewayKind> {
    let host = url.host_str().unwrap_or_default();
    if host.eq_ignore_ascii_case("go.openathens.net") && url.path().starts_with("/redirector/") {
        Some(InstitutionGatewayKind::OpenAthens)
    } else if url.path().trim_end_matches('/').ends_with("/login")
        || host.ends_with(".idm.oclc.org")
        || url.query_pairs().any(|(key, _)| key == "url")
    {
        Some(InstitutionGatewayKind::Ezproxy)
    } else {
        None
    }
}

fn parse_config_url(key: &str, raw: Option<&str>) -> Result<Option<Url>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let url = Url::parse(raw).map_err(|error| {
        ZoteroMcpError::Config(format!("{key} must be an absolute HTTPS URL: {error}"))
    })?;
    if (url.scheme() != "https" && is_test_loopback_url(&url).not()) || url.host_str().is_none() {
        return Err(ZoteroMcpError::Config(format!(
            "{key} must be an absolute HTTPS URL"
        )));
    }
    if url.username().is_empty().not() || url.password().is_some() {
        return Err(ZoteroMcpError::Config(format!(
            "{key} must not contain embedded credentials"
        )));
    }
    Ok(Some(url))
}

fn is_test_loopback_url(url: &Url) -> bool {
    cfg!(test)
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
}

fn parse_target_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim()).map_err(|error| {
        ZoteroMcpError::InvalidInput(format!(
            "source URL must be an absolute HTTP(S) URL: {error}"
        ))
    })?;
    if matches!(url.scheme(), "http" | "https").not() || url.host_str().is_none() {
        return Err(ZoteroMcpError::InvalidInput(
            "source URL must be an absolute HTTP(S) URL".to_string(),
        ));
    }
    Ok(url)
}

fn build_gateway_url(base: &Url, kind: InstitutionGatewayKind, target: &Url) -> Url {
    let mut result = base.clone();
    match kind {
        InstitutionGatewayKind::Ezproxy | InstitutionGatewayKind::OpenAthens => {
            replace_query_pair(&mut result, "url", target.as_str());
        }
        InstitutionGatewayKind::OpenUrl => unreachable!("OpenURL is not an authentication gateway"),
    }
    result
}

fn build_openurl(base: &Url, target: &Url, doi: Option<&str>) -> Url {
    let mut result = base.clone();
    append_query_pair_if_missing(&mut result, "url_ver", "Z39.88-2004");
    append_query_pair_if_missing(&mut result, "rft_val_fmt", "info:ofi/fmt:kev:mtx:journal");
    let identifier = doi
        .map(str::trim)
        .filter(|value| value.is_empty().not())
        .map(|value| format!("info:doi/{value}"))
        .unwrap_or_else(|| target.to_string());
    replace_query_pair(&mut result, "rft_id", &identifier);
    append_query_pair_if_missing(&mut result, "rfr_id", "info:sid/paperbridge");
    result
}

fn replace_query_pair(url: &mut Url, key: &str, value: &str) {
    let existing = url
        .query_pairs()
        .filter(|(existing_key, _)| existing_key != key)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    let mut pairs = url.query_pairs_mut();
    for (existing_key, existing_value) in existing {
        pairs.append_pair(&existing_key, &existing_value);
    }
    pairs.append_pair(key, value);
}

fn append_query_pair_if_missing(url: &mut Url, key: &str, value: &str) {
    if url
        .query_pairs()
        .any(|(existing_key, _)| existing_key == key)
        .not()
    {
        url.query_pairs_mut().append_pair(key, value);
    }
}

fn validate_redirect_target(url: &Url) -> Result<()> {
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(ZoteroMcpError::Http(
            "institutional resolver redirect must use HTTPS".to_string(),
        ));
    }
    if url.username().is_empty().not() || url.password().is_some() {
        return Err(ZoteroMcpError::Http(
            "institutional resolver redirect must not contain credentials".to_string(),
        ));
    }
    let host = url.host_str().unwrap_or_default();
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(ZoteroMcpError::Http(
            "institutional resolver redirect must not target localhost".to_string(),
        ));
    }
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        let private = match address {
            std::net::IpAddr::V4(address) => {
                address.is_private()
                    || address.is_loopback()
                    || address.is_link_local()
                    || address.is_unspecified()
                    || address.is_broadcast()
            }
            std::net::IpAddr::V6(address) => {
                address.is_loopback()
                    || address.is_unspecified()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            }
        };
        if private {
            return Err(ZoteroMcpError::Http(
                "institutional resolver redirect must not target a private address".to_string(),
            ));
        }
    }
    Ok(())
}

fn explicitly_unavailable(html: &str) -> bool {
    let lowercase = html.to_ascii_lowercase();
    [
        "no full text available",
        "no full-text available",
        "no online access available",
        "no results found",
        "no holdings found",
    ]
    .iter()
    .any(|marker| lowercase.contains(marker))
}

fn apply_selected_option(
    resolution: &mut SourceAccessResolution,
    selected: &AccessOption,
    browser_open_allowed: bool,
) {
    resolution.selected_url.clone_from(&selected.url);
    resolution.institution_url = Some(selected.url.clone());
    resolution.authentication_required = selected.authentication_required;
    resolution.gateway = Url::parse(&selected.url)
        .ok()
        .and_then(|url| url_chain(&url).iter().find_map(detect_gateway_only));
    resolution.browser_open_allowed = browser_open_allowed;
}

fn parse_openurl_options(
    base: &Url,
    html: &str,
    gateway_url: Option<&Url>,
) -> Result<Vec<AccessOption>> {
    let providers = html_elements(html, "span")
        .into_iter()
        .filter_map(|element| {
            let classes = element.attributes.get("class")?;
            if classes
                .split_whitespace()
                .any(|class| class == "resource-name")
                .not()
            {
                return None;
            }
            let id = element.attributes.get("id")?;
            let name = normalize_text(&strip_html_tags(&element.inner));
            name.is_empty().not().then(|| (id.clone(), name))
        })
        .collect::<HashMap<_, _>>();

    let mut seen = HashSet::new();
    let mut options = Vec::new();
    for element in html_elements(html, "a") {
        let label = normalize_text(&strip_html_tags(&element.inner));
        let has_fulltext_class = element.attributes.get("class").is_some_and(|classes| {
            classes
                .split_whitespace()
                .any(|class| matches!(class, "full-text-link" | "fulltext-link"))
        });
        if is_fulltext_label(&label).not() && has_fulltext_class.not() {
            continue;
        }
        let Some(href) = element.attributes.get("href") else {
            continue;
        };
        if href.trim().is_empty() || href.trim_start().starts_with('#') {
            continue;
        }
        let Ok(url) = base.join(href) else {
            continue;
        };
        if (url.scheme() != "https" && is_test_loopback_url(&url).not())
            || seen.insert(url.to_string()).not()
        {
            continue;
        }
        let destination = deepest_destination(&url);
        let provider = element
            .attributes
            .get("aria-describedby")
            .and_then(|id| id.split_whitespace().find_map(|id| providers.get(id)))
            .cloned()
            .or_else(|| destination.host_str().map(provider_name_from_host))
            .unwrap_or_else(|| label.clone());
        let authentication_required = url_chain(&url)
            .iter()
            .any(|candidate| detect_gateway_only(candidate).is_some());
        options.push(AccessOption {
            provider,
            url: url.to_string(),
            destination_url: (destination != url).then(|| destination.to_string()),
            authentication_required,
            auto_open: same_origin(&url, base)
                || gateway_url.is_some_and(|gateway| same_origin(&url, gateway)),
        });
    }
    Ok(options)
}

struct HtmlElement {
    attributes: HashMap<String, String>,
    inner: String,
}

fn html_elements(html: &str, tag: &str) -> Vec<HtmlElement> {
    let lowercase = html.to_ascii_lowercase();
    let open_marker = format!("<{tag}");
    let close_marker = format!("</{tag}>");
    let mut cursor = 0;
    let mut elements = Vec::new();
    while let Some(relative_start) = lowercase[cursor..].find(&open_marker) {
        let start = cursor + relative_start;
        let boundary = lowercase.as_bytes().get(start + open_marker.len()).copied();
        if boundary.is_some_and(|byte| byte.is_ascii_whitespace().not() && byte != b'>') {
            cursor = start + open_marker.len();
            continue;
        }
        let Some(relative_open_end) = lowercase[start..].find('>') else {
            break;
        };
        let open_end = start + relative_open_end;
        let inner_start = open_end + 1;
        let Some(relative_close) = lowercase[inner_start..].find(&close_marker) else {
            break;
        };
        let close = inner_start + relative_close;
        let attributes = parse_html_attributes(
            html.get(start + open_marker.len()..open_end)
                .unwrap_or_default(),
        );
        elements.push(HtmlElement {
            attributes,
            inner: html.get(inner_start..close).unwrap_or_default().to_string(),
        });
        // Advance past the opening tag rather than the closing tag so nested
        // elements of the same kind remain discoverable.
        cursor = open_end + 1;
    }
    elements
}

fn parse_html_attributes(raw: &str) -> HashMap<String, String> {
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut attributes = HashMap::new();
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < bytes.len()
            && bytes[cursor].is_ascii_whitespace().not()
            && bytes[cursor] != b'='
        {
            cursor += 1;
        }
        if cursor == name_start {
            cursor += 1;
            continue;
        }
        let name = raw
            .get(name_start..cursor)
            .unwrap_or_default()
            .to_ascii_lowercase();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            attributes.insert(name, String::new());
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = bytes.get(cursor).copied();
        let (value_start, value_end) = if matches!(quote, Some(b'\'' | b'"')) {
            cursor += 1;
            let start = cursor;
            while cursor < bytes.len() && Some(bytes[cursor]) != quote {
                cursor += 1;
            }
            let end = cursor;
            cursor = cursor.saturating_add(1);
            (start, end)
        } else {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace().not() {
                cursor += 1;
            }
            (start, cursor)
        };
        attributes.insert(
            name,
            decode_html_entities(raw.get(value_start..value_end).unwrap_or_default()),
        );
    }
    attributes
}

fn strip_html_tags(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut inside_tag = false;
    for character in raw.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if inside_tag.not() => text.push(character),
            _ => {}
        }
    }
    decode_html_entities(&text)
}

fn decode_html_entities(raw: &str) -> String {
    quick_xml::escape::unescape(raw)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

fn normalize_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_fulltext_label(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    [
        "full text",
        "full-text",
        "view article",
        "get article",
        "download pdf",
        "view pdf",
        "access online",
        "online access",
        "available online",
    ]
    .iter()
    .any(|needle| label.contains(needle))
}

fn url_chain(url: &Url) -> Vec<Url> {
    let mut chain = vec![url.clone()];
    for _ in 0..4 {
        let Some(current) = chain.last() else {
            break;
        };
        let nested = current.query_pairs().find_map(|(key, value)| {
            matches!(key.as_ref(), "U" | "url" | "qurl" | "target")
                .then(|| Url::parse(value.as_ref()).ok())
                .flatten()
        });
        let Some(nested) = nested else {
            break;
        };
        if matches!(nested.scheme(), "http" | "https").not() {
            break;
        }
        chain.push(nested);
    }
    chain
}

fn deepest_destination(url: &Url) -> Url {
    url_chain(url).pop().unwrap_or_else(|| url.clone())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn provider_name_from_host(host: &str) -> String {
    host.trim_start_matches("www.").to_string()
}

fn access_option_rank(option: &AccessOption) -> (u8, u8, String) {
    let destination = option.destination_url.as_deref().unwrap_or(&option.url);
    let aggregator = [
        "ebsco",
        "gale.com",
        "proquest",
        "jstor",
        "firstsearch",
        "oclc.org",
    ]
    .iter()
    .any(|needle| destination.to_ascii_lowercase().contains(needle));
    (
        u8::from(option.auto_open.not()),
        u8::from(aggregator),
        option.provider.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ezproxy_url_wraps_target_without_hard_coded_institution() {
        let access = InstitutionAccess::new(
            InstitutionAccessMode::Fallback,
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let resolution = access
            .resolve("https://journals.example.org/paper?a=1&b=2", None)
            .unwrap();

        assert_eq!(resolution.gateway, Some(InstitutionGatewayKind::Ezproxy));
        let generated = Url::parse(resolution.institution_url.as_deref().unwrap()).unwrap();
        assert_eq!(
            generated
                .query_pairs()
                .find(|(key, _)| key == "url")
                .map(|(_, value)| value.into_owned()),
            Some("https://journals.example.org/paper?a=1&b=2".to_string())
        );
    }

    #[test]
    fn split_profile_prefers_openurl_for_doi() {
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some("https://resolver.example.edu/openurl"),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let resolution = access
            .resolve("https://doi.org/10.1000/example", Some("10.1000/example"))
            .unwrap();
        let generated = Url::parse(resolution.institution_url.as_deref().unwrap()).unwrap();
        assert_eq!(resolution.resolver, Some(InstitutionResolverKind::OpenUrl));
        assert!(resolution.gateway.is_none());
        assert!(
            generated
                .query_pairs()
                .any(|(key, value)| key == "rft_id" && value == "info:doi/10.1000/example")
        );
    }

    #[test]
    fn prefer_routes_direct_candidate_through_institution_but_fallback_does_not() {
        let fallback = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some("https://resolver.example.edu/openurl"),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let prefer = InstitutionAccess::new_profile(
            InstitutionAccessMode::Prefer,
            None,
            Some("https://resolver.example.edu/openurl"),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();

        let fallback_result = fallback
            .resolve_direct("https://oa.example.org/paper.pdf", Some("10.1000/example"))
            .unwrap();
        let prefer_result = prefer
            .resolve_direct("https://oa.example.org/paper.pdf", Some("10.1000/example"))
            .unwrap();
        assert_eq!(
            fallback_result.selected_url,
            "https://oa.example.org/paper.pdf"
        );
        assert_eq!(
            prefer_result.resolver,
            Some(InstitutionResolverKind::OpenUrl)
        );
    }

    #[test]
    fn explicit_url_uses_gateway_when_profile_has_both() {
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some("https://resolver.example.edu/openurl"),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let resolution = access.resolve("https://example.org/article", None).unwrap();
        assert_eq!(resolution.gateway, Some(InstitutionGatewayKind::Ezproxy));
        assert!(resolution.resolver.is_none());
    }

    #[test]
    fn openathens_redirector_is_detected() {
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Prefer,
            None,
            None,
            Some("https://go.openathens.net/redirector/example.edu?url="),
        )
        .unwrap();
        let resolution = access.resolve("https://example.org/article", None).unwrap();
        assert_eq!(resolution.gateway, Some(InstitutionGatewayKind::OpenAthens));
    }

    #[test]
    fn parser_extracts_and_ranks_fulltext_links() {
        let base = Url::parse("https://resolver.example.edu/").unwrap();
        let html = r#"
            <div class="resource-row">
              <a href="./log?U=https%3A%2F%2Fproxy.example.edu%2Flogin%3Furl%3Dhttps%3A%2F%2Fsearch.ebscohost.com%2Fpaper" aria-describedby="a">Full Text Online</a>
              <span class="resource-name" id="a">Academic Search</span>
            </div>
            <div class="resource-row">
              <a href="./log?U=https%3A%2F%2Fproxy.example.edu%2Flogin%3Furl%3Dhttps%3A%2F%2Fpublisher.example.org%2Farticle" aria-describedby="b">Full Text Online</a>
              <span class="resource-name" id="b">Publisher Journals</span>
            </div>
        "#;
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some(base.as_str()),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let mut resolution = access
            .resolve("https://doi.org/10.1000/example", Some("10.1000/example"))
            .unwrap();
        let options = parse_openurl_options(&base, html, None).unwrap();
        access.apply_holdings(
            &mut resolution,
            HoldingsLookup {
                status: HoldingsStatus::Available,
                options,
            },
        );

        assert_eq!(resolution.holdings_status, HoldingsStatus::Available);
        assert_eq!(resolution.access_options.len(), 2);
        assert_eq!(resolution.access_options[0].provider, "Publisher Journals");
        assert!(resolution.authentication_required);
        assert!(resolution.selected_url.contains("publisher.example.org"));
    }

    #[test]
    fn parser_rejects_insecure_links_and_does_not_auto_open_foreign_origins() {
        let base = Url::parse("https://resolver.example.edu/").unwrap();
        let gateway = Url::parse("https://proxy.example.edu/login?url=").unwrap();
        let html = r#"
            <a href="http://attacker.example/phish">Full Text Online</a>
            <a href="https://publisher.example.org/article">Full Text Online</a>
            <a href="https://proxy.example.edu/login?url=https%3A%2F%2Fpublisher.example.org%2Farticle">Full Text Online</a>
        "#;
        let options = parse_openurl_options(&base, html, Some(&gateway)).unwrap();
        assert_eq!(options.len(), 2);
        assert!(options.iter().any(|option| option.url.starts_with("https://publisher") && option.auto_open.not()));
        assert!(
            options
                .iter()
                .any(|option| option.url.starts_with("https://proxy") && option.auto_open)
        );
    }

    #[tokio::test]
    async fn resolver_redirect_is_preserved_without_fetching_foreign_origin() {
        let resolver = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(302)
                    .insert_header("Location", "https://publisher.example.org/article"),
            )
            .mount(&resolver)
            .await;
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some(&resolver.uri()),
            None,
        )
        .unwrap();
        let mut resolution = access
            .resolve("https://doi.org/10.1000/example", Some("10.1000/example"))
            .unwrap();
        let lookup = access
            .resolve_holdings("https://doi.org/10.1000/example", Some("10.1000/example"))
            .await
            .unwrap();
        assert_eq!(lookup.status, HoldingsStatus::Available);
        assert_eq!(lookup.options.len(), 1);
        assert_eq!(
            lookup.options[0].url,
            "https://publisher.example.org/article"
        );
        assert!(lookup.options[0].auto_open.not());
        access.apply_holdings(&mut resolution, lookup);
        assert!(resolution.browser_open_allowed.not());
    }

    #[test]
    fn resolver_redirect_validation_rejects_downgrade_and_private_targets() {
        for target in [
            "http://publisher.example.org/article",
            "https://127.0.0.1/article",
            "https://169.254.169.254/latest/meta-data",
            "https://localhost/article",
        ] {
            let url = Url::parse(target).unwrap();
            assert!(validate_redirect_target(&url).is_err(), "accepted {target}");
        }
    }

    #[tokio::test]
    async fn resolver_direct_pdf_is_available_without_buffering_the_document() {
        let resolver = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/pdf")
                    .set_body_bytes(vec![b'x'; 4096]),
            )
            .mount(&resolver)
            .await;
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some(&resolver.uri()),
            None,
        )
        .unwrap();
        let lookup = access
            .resolve_holdings("https://doi.org/10.1000/example", Some("10.1000/example"))
            .await
            .unwrap();
        assert_eq!(lookup.status, HoldingsStatus::Available);
        assert_eq!(lookup.options.len(), 1);
        assert!(lookup.options[0].auto_open);
    }

    #[tokio::test]
    async fn unfamiliar_resolver_html_is_unparsed_not_unavailable() {
        let resolver = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/html")
                    .set_body_string("<html><body>Choose a service</body></html>"),
            )
            .mount(&resolver)
            .await;
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some(&resolver.uri()),
            None,
        )
        .unwrap();
        let lookup = access
            .resolve_holdings("https://doi.org/10.1000/example", Some("10.1000/example"))
            .await
            .unwrap();
        assert_eq!(lookup.status, HoldingsStatus::Unparsed);
        assert!(lookup.options.is_empty());
    }

    #[test]
    fn parser_ignores_browse_and_unrelated_links() {
        let base = Url::parse("https://resolver.example.edu/").unwrap();
        let html = r#"
            <a href="/journal">Browse Journal</a>
            <a href="/help">Help</a>
        "#;
        assert!(parse_openurl_options(&base, html, None).unwrap().is_empty());
    }

    #[test]
    fn off_mode_leaves_target_direct_even_when_profile_is_configured() {
        let access = InstitutionAccess::new_profile(
            InstitutionAccessMode::Off,
            None,
            Some("https://resolver.example.edu/openurl"),
            Some("https://proxy.example.edu/login?url="),
        )
        .unwrap();
        let resolution = access
            .resolve("https://example.org/article", Some("10.1000/example"))
            .unwrap();
        assert_eq!(resolution.selected_url, "https://example.org/article");
        assert!(resolution.institution_url.is_none());
        assert!(resolution.authentication_required.not());
    }

    #[test]
    fn profile_rejects_swapped_resolver_and_gateway() {
        let error = InstitutionAccess::new_profile(
            InstitutionAccessMode::Fallback,
            None,
            Some("https://proxy.example.edu/login?url="),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("institution_gateway_url"));
    }

    #[test]
    fn unsafe_target_scheme_is_rejected() {
        let access = InstitutionAccess::disabled();
        let error = access.resolve("file:///tmp/paper.pdf", None).unwrap_err();
        assert!(error.to_string().contains("HTTP(S)"));
    }
}
