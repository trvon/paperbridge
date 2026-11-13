//! Launching and inspecting the dedicated Chrome that `papers fetch` drives.
//!
//! The institutional session lives in a Chrome profile directory that the
//! operator owns. Paperbridge can start that Chrome, ask it to restore
//! session cookies across restarts ("Continue where you left off"), and
//! report how many gateway cookies it holds. It never reads, stores, or
//! prints a cookie value or a credential.

use super::cdp::{self, CdpTransport, WsCdpTransport};
use crate::{Result, ZoteroMcpError};
use serde::Serialize;
use serde_json::{Value, json};
use std::ops::Not;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

/// Chrome's "On startup: Continue where you left off". With it, Chrome
/// restores session cookies when it starts; without it, it drops them.
const RESTORE_LAST_SESSION: u64 = 1;
const CHROME_COMMANDS: &[&str] = &[
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
];
const START_TIMEOUT: Duration = Duration::from_secs(40);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// `$XDG_DATA_HOME/paperbridge/chrome-profile`, the directory the documented
/// launch line uses.
pub fn default_profile_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("paperbridge")
        .join("chrome-profile")
}

fn preferences_path(profile: &Path) -> PathBuf {
    profile.join("Default").join("Preferences")
}

/// True when a Chrome process holds the profile. Chrome marks a profile in
/// use with a `SingletonLock` symlink to `<host>-<pid>`; a lock whose process
/// is gone (a crash) does not count.
pub fn profile_in_use(profile: &Path) -> bool {
    let lock = profile.join("SingletonLock");
    let Ok(target) = std::fs::read_link(&lock) else {
        // Not a symlink: in use only if something else sits there.
        return std::fs::symlink_metadata(&lock).is_ok();
    };
    let pid = target
        .to_string_lossy()
        .rsplit('-')
        .next()
        .and_then(|raw| raw.parse::<u32>().ok());
    match pid {
        Some(pid) if cfg!(target_os = "linux") => Path::new(&format!("/proc/{pid}")).exists(),
        _ => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferencesEdit {
    Created,
    Updated,
    AlreadySet,
}

/// Whether the profile is set to restore the last session on startup.
/// `None` when the profile has no readable Preferences file yet.
pub fn restore_on_startup(profile: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(preferences_path(profile)).ok()?;
    let preferences: Value = serde_json::from_str(&raw).ok()?;
    Some(
        preferences
            .pointer("/session/restore_on_startup")
            .and_then(Value::as_u64)
            == Some(RESTORE_LAST_SESSION),
    )
}

/// Set `session.restore_on_startup = 1` in `<profile>/Default/Preferences`,
/// keeping every other key. Refuses while Chrome runs on the profile: Chrome
/// rewrites the file on exit and would discard the edit.
pub fn ensure_restore_on_startup(profile: &Path) -> Result<PreferencesEdit> {
    if profile_in_use(profile) {
        return Err(ZoteroMcpError::Config(format!(
            "Chrome is running on {}; close that Chrome before changing its startup setting (or set \"On startup: Continue where you left off\" in its settings)",
            profile.display()
        )));
    }
    let path = preferences_path(profile);
    let io_error = |error: std::io::Error| {
        ZoteroMcpError::Config(format!("could not update {}: {error}", path.display()))
    };
    let (mut preferences, existed) = match std::fs::read_to_string(&path) {
        Ok(raw) => (
            serde_json::from_str::<Value>(&raw).map_err(|error| {
                ZoteroMcpError::Config(format!(
                    "{} is not valid JSON ({error}); not touching it",
                    path.display()
                ))
            })?,
            true,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (json!({}), false),
        Err(error) => return Err(io_error(error)),
    };
    let Some(root) = preferences.as_object_mut() else {
        return Err(ZoteroMcpError::Config(format!(
            "{} is not a JSON object; not touching it",
            path.display()
        )));
    };
    let session = root.entry("session").or_insert_with(|| json!({}));
    let Some(session) = session.as_object_mut() else {
        return Err(ZoteroMcpError::Config(format!(
            "`session` in {} is not a JSON object; not touching it",
            path.display()
        )));
    };
    if session.get("restore_on_startup").and_then(Value::as_u64) == Some(RESTORE_LAST_SESSION) {
        return Ok(PreferencesEdit::AlreadySet);
    }
    session.insert(
        "restore_on_startup".to_string(),
        json!(RESTORE_LAST_SESSION),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_error)?;
    }
    let staged = path.with_extension("paperbridge.tmp");
    std::fs::write(&staged, preferences.to_string()).map_err(io_error)?;
    std::fs::rename(&staged, &path).map_err(io_error)?;
    Ok(if existed {
        PreferencesEdit::Updated
    } else {
        PreferencesEdit::Created
    })
}

/// What to launch when no DevTools endpoint answers.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Chrome program and leading arguments.
    pub command: Vec<String>,
    pub profile: PathBuf,
    pub headless: bool,
}

/// Pick the Chrome command: an explicit one, else `browser_command` when it
/// names Chrome or Chromium (it may name another browser for `papers
/// access`), else the usual binary names.
pub fn chrome_command(explicit: Option<&str>, browser_command: Option<&str>) -> Vec<Vec<String>> {
    let split = |raw: &str| -> Vec<String> {
        raw.split_whitespace()
            .filter(|part| part.contains("%s").not())
            .map(str::to_string)
            .collect()
    };
    if let Some(explicit) = explicit.map(split).filter(|argv| argv.is_empty().not()) {
        return vec![explicit];
    }
    let mut candidates = Vec::new();
    if let Some(configured) = browser_command.map(split).filter(|argv| {
        argv.first().is_some_and(|program| {
            let name = program.rsplit('/').next().unwrap_or(program).to_lowercase();
            name.contains("chrome") || name.contains("chromium")
        })
    }) {
        candidates.push(configured);
    }
    candidates.extend(CHROME_COMMANDS.iter().map(|name| vec![(*name).to_string()]));
    candidates
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
    })
}

/// The first Chrome command that exists on this machine.
pub fn resolve_chrome(
    explicit: Option<&str>,
    browser_command: Option<&str>,
) -> Result<Vec<String>> {
    chrome_command(explicit, browser_command)
        .into_iter()
        .find(|argv| argv.first().is_some_and(|program| on_path(program)))
        .ok_or_else(|| {
            ZoteroMcpError::Config(format!(
                "no Chrome found (tried {}). Install Chrome or pass --chrome <command>",
                explicit
                    .unwrap_or("google-chrome-stable, google-chrome, chromium, chromium-browser")
            ))
        })
}

/// The arguments Paperbridge starts Chrome with. Loopback debugging port, no
/// first-run screens (they block the port on a new profile), dedicated
/// profile; `--headless=new` shares the headed profile format.
pub fn launch_args(port: u16, profile: &Path, headless: bool) -> Vec<String> {
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        format!("--user-data-dir={}", profile.display()),
    ];
    if headless {
        args.push("--headless=new".to_string());
    }
    args
}

fn endpoint_port(cdp_url: &str) -> Result<u16> {
    cdp::validate_cdp_url(cdp_url)?
        .port_or_known_default()
        .ok_or_else(|| ZoteroMcpError::InvalidInput("--cdp-url needs a port".to_string()))
}

/// Chrome's version string when a DevTools endpoint answers at `cdp_url`.
///
/// An `http://` endpoint is asked for `/json/version`. A `ws://` endpoint is
/// the browser's own WebSocket, so it is asked `Browser.getVersion` there.
pub async fn endpoint_browser(cdp_url: &str) -> Option<String> {
    let url = cdp::validate_cdp_url(cdp_url).ok()?;
    if url.scheme() == "ws" {
        let probe = async {
            let mut transport = WsCdpTransport::connect(cdp_url).await.ok()?;
            let version = transport
                .call(None, "Browser.getVersion", json!({}))
                .await
                .ok()?;
            Some(
                version
                    .get("product")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            )
        };
        return tokio::time::timeout(PROBE_TIMEOUT, probe)
            .await
            .ok()
            .flatten();
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let version: Value = client
        .get(url.join("/json/version").ok()?)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some(
        version
            .get("Browser")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct StartReport {
    pub endpoint: String,
    /// False when a browser was already answering and nothing was started.
    pub launched: bool,
    pub headless: bool,
    pub profile_dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferences: Option<PreferencesEdit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Start Chrome on the profile unless an endpoint already answers. Before
/// the launch the profile is set to restore its session, so the proxy's
/// session cookie survives the next restart.
pub async fn ensure_browser(cdp_url: &str, spec: &LaunchSpec) -> Result<StartReport> {
    let port = endpoint_port(cdp_url)?;
    let mut report = StartReport {
        endpoint: cdp_url.to_string(),
        launched: false,
        headless: spec.headless,
        profile_dir: spec.profile.display().to_string(),
        command: None,
        preferences: None,
        note: None,
    };
    if endpoint_browser(cdp_url).await.is_some() {
        report.headless = false;
        report.note =
            Some("a browser already answers at this endpoint; nothing was started".into());
        return Ok(report);
    }
    if profile_in_use(&spec.profile) {
        return Err(ZoteroMcpError::Config(format!(
            "Chrome is already running on {} but not answering at {cdp_url}. Close it, or start it with --remote-debugging-port={port}",
            spec.profile.display()
        )));
    }
    refuse_websocket_launch(cdp_url, port)?;
    report.preferences = Some(ensure_restore_on_startup(&spec.profile)?);
    report.command = Some(launch_chrome(cdp_url, spec).await?);
    report.launched = true;
    Ok(report)
}

/// A `ws://` endpoint names one running browser: its path carries an id that
/// Chrome generates at each start. A browser started now would answer at a
/// different address, so there is nothing to wait for.
fn refuse_websocket_launch(cdp_url: &str, port: u16) -> Result<()> {
    if cdp::validate_cdp_url(cdp_url)?.scheme() != "ws" {
        return Ok(());
    }
    Err(ZoteroMcpError::InvalidInput(format!(
        "nothing answers at {cdp_url}, and a ws:// endpoint names one running browser, so Paperbridge cannot start a browser at it. Pass the http:// form to start one: --cdp-url http://127.0.0.1:{port}"
    )))
}

/// Spawn Chrome on the profile and wait for its DevTools port. Returns the
/// program that was started. Does not touch the profile's preferences.
pub async fn launch_chrome(cdp_url: &str, spec: &LaunchSpec) -> Result<String> {
    let port = endpoint_port(cdp_url)?;
    refuse_websocket_launch(cdp_url, port)?;
    let Some((program, leading)) = spec.command.split_first() else {
        return Err(ZoteroMcpError::Config(
            "no Chrome command configured".into(),
        ));
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(leading)
        .args(launch_args(port, &spec.profile, spec.headless))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group: the browser must outlive the command that
    // started it, including a shell or agent that later signals its group.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        ZoteroMcpError::Config(format!(
            "could not start {program}: {error}. Install Chrome or pass --chrome <command>"
        ))
    })?;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if endpoint_browser(cdp_url).await.is_some() {
            return Ok(program.clone());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(ZoteroMcpError::Config(format!(
                "{program} exited ({status}) before opening the DevTools port{}",
                if spec.headless {
                    ""
                } else {
                    "; a headed Chrome needs a display"
                }
            )));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.start_kill();
            return Err(ZoteroMcpError::Config(format!(
                "{program} did not open the DevTools port at {cdp_url} within {} seconds",
                START_TIMEOUT.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn lock_pid(profile: &Path) -> Option<u32> {
    std::fs::read_link(profile.join("SingletonLock"))
        .ok()?
        .to_string_lossy()
        .rsplit('-')
        .next()?
        .parse()
        .ok()
}

/// Confirm that the browser answering on `port` is the Chrome running on
/// `profile`, and return its process id when known. `Err` says why not.
///
/// Paperbridge closes only that browser. Anything else answering on the
/// port, such as a Chrome the operator started on another profile, is left
/// alone.
pub fn endpoint_owner(profile: &Path, port: u16) -> std::result::Result<Option<u32>, String> {
    if profile_in_use(profile).not() {
        return Err(format!(
            "no Chrome is running on the profile {}",
            profile.display()
        ));
    }
    let pid = lock_pid(profile);
    if cfg!(target_os = "linux") {
        let Some(pid) = pid else {
            return Err(format!(
                "the lock in {} does not name a process",
                profile.display()
            ));
        };
        let raw = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map_err(|error| format!("could not inspect process {pid}: {error}"))?;
        // Chrome rewrites its command line with spaces between arguments,
        // so match whole arguments in the flat string.
        let line = String::from_utf8_lossy(&raw).replace('\0', " ");
        let has_argument = |argument: &str| {
            line.match_indices(argument).any(|(index, _)| {
                let before = index == 0 || line.as_bytes()[index - 1] == b' ';
                let after = line[index + argument.len()..].chars().next();
                before && matches!(after, None | Some(' '))
            })
        };
        let canonical = std::fs::canonicalize(profile).unwrap_or_else(|_| profile.to_path_buf());
        let on_profile = [profile, canonical.as_path()]
            .iter()
            .any(|directory| has_argument(&format!("--user-data-dir={}", directory.display())));
        let on_port = has_argument(&format!("--remote-debugging-port={port}"));
        if on_profile.not() || on_port.not() {
            return Err(format!(
                "process {pid}, which holds the profile {}, was not started with --remote-debugging-port={port} on that profile",
                profile.display()
            ));
        }
        return Ok(Some(pid));
    }
    // Elsewhere, Chrome's own record of its DevTools port in the profile.
    let recorded = std::fs::read_to_string(profile.join("DevToolsActivePort"))
        .ok()
        .and_then(|text| {
            text.lines()
                .next()
                .and_then(|line| line.trim().parse::<u16>().ok())
        });
    if recorded != Some(port) {
        return Err(format!(
            "the Chrome on the profile {} does not record DevTools port {port}",
            profile.display()
        ));
    }
    Ok(pid)
}

/// How a `browser stop` ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopStatus {
    /// Nothing answered at the endpoint; there was nothing to close.
    #[default]
    NotRunning,
    /// A dry run: the browser was identified and left running.
    WouldStop,
    /// Chrome exited and released the profile.
    Stopped,
    /// Chrome was asked to close but is still running, or still holds the
    /// profile. Its cookies may not have been written.
    NotStopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct StopReport {
    pub endpoint: String,
    pub profile_dir: String,
    /// What answers at the endpoint, when something does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub status: StopStatus,
    /// True only when Chrome exited and the profile lock is gone.
    pub stopped: bool,
    pub dry_run: bool,
    pub detail: String,
}

impl StopReport {
    /// The error a caller should end with when the browser was asked to
    /// close and did not.
    pub fn failure(&self) -> Option<ZoteroMcpError> {
        (self.status == StopStatus::NotStopped).then(|| ZoteroMcpError::Config(self.detail.clone()))
    }
}

/// Ask the Chrome running on `profile` to exit cleanly (`Browser.close`),
/// which flushes its cookie store. A browser at `cdp_url` that is not the
/// one on `profile` is refused, never closed. With `dry_run`, only reports
/// what would be closed.
pub async fn stop_browser(cdp_url: &str, profile: &Path, dry_run: bool) -> Result<StopReport> {
    stop_browser_within(cdp_url, profile, dry_run, STOP_TIMEOUT).await
}

async fn stop_browser_within(
    cdp_url: &str,
    profile: &Path,
    dry_run: bool,
    wait: Duration,
) -> Result<StopReport> {
    let port = endpoint_port(cdp_url)?;
    let mut report = StopReport {
        endpoint: cdp_url.to_string(),
        profile_dir: profile.display().to_string(),
        browser: endpoint_browser(cdp_url).await,
        pid: None,
        status: StopStatus::NotRunning,
        stopped: false,
        dry_run,
        detail: String::new(),
    };
    let Some(browser) = report.browser.clone() else {
        report.detail = "nothing answers at this endpoint".to_string();
        return Ok(report);
    };
    report.pid = endpoint_owner(profile, port).map_err(|why| {
        ZoteroMcpError::Config(format!(
            "not closing the browser at {cdp_url} ({browser}): {why}. `browser stop` only closes the Chrome running on the Paperbridge profile; pass --profile <dir> if yours is elsewhere, or close that window yourself"
        ))
    })?;
    let target = format!(
        "{browser}{} on profile {}",
        report
            .pid
            .map(|pid| format!(", process {pid},"))
            .unwrap_or_default(),
        profile.display()
    );
    if dry_run {
        report.status = StopStatus::WouldStop;
        report.detail = format!("would close {target}");
        return Ok(report);
    }
    let mut transport = WsCdpTransport::connect(cdp_url).await?;
    close_browser(&mut transport).await;
    drop(transport);
    let deadline = tokio::time::Instant::now() + wait;
    // Stopped means both: the endpoint is gone and the profile is released.
    let (answering, locked) = loop {
        let state = (
            endpoint_browser(cdp_url).await.is_some(),
            profile_in_use(profile),
        );
        if state == (false, false) || tokio::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(Duration::from_millis(200).min(wait)).await;
    };
    if answering || locked {
        let still = match (answering, locked) {
            (true, true) => "it still answers and still holds the profile",
            (true, false) => "it still answers at the endpoint",
            _ => "a Chrome process still holds the profile",
        };
        report.status = StopStatus::NotStopped;
        report.detail = format!(
            "asked {target} to close, but after {} seconds {still}. Its cookies may not have been written; close that window yourself, then check with `paperbridge browser status`",
            wait.as_secs()
        );
        return Ok(report);
    }
    report.status = StopStatus::Stopped;
    report.stopped = true;
    report.detail = format!("closed {target}");
    Ok(report)
}

/// Chrome drops the connection as it exits, so a failed reply is expected.
pub async fn close_browser(transport: &mut dyn CdpTransport) {
    let _ = transport.call(None, "Browser.close", json!({})).await;
}

/// Counts and expiry times of the cookies a browser holds for one host.
/// There is no field for a cookie's name or value: they are never kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CookieSummary {
    pub host: String,
    pub total: usize,
    /// Session cookies: kept across a restart only with `restore_on_startup`.
    pub session: usize,
    pub persistent: usize,
    /// Unix seconds; over the persistent cookies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_expiry: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_expiry: Option<i64>,
}

/// Whether a browser would send a cookie with this `domain` to `host`.
/// Chrome reports a domain cookie with a leading dot: it applies to that
/// domain and everything below it. Without the dot the cookie is host-only.
/// A cookie scoped to a subdomain of `host` is never sent to `host`.
fn cookie_matches(domain: &str, host: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    match domain.strip_prefix('.') {
        Some(parent) => {
            parent.is_empty().not() && (host == parent || host.ends_with(&format!(".{parent}")))
        }
        None => domain.is_empty().not() && domain == host,
    }
}

/// Reduce a CDP cookie list to counts and expiries for `host`. Only the
/// `domain`, `session`, and `expires` attributes are looked at.
pub fn summarize_cookies(reply: &Value, host: &str) -> CookieSummary {
    let host = host.to_ascii_lowercase();
    let mut summary = CookieSummary {
        host: host.clone(),
        ..CookieSummary::default()
    };
    let cookies = reply.get("cookies").and_then(Value::as_array);
    for cookie in cookies.into_iter().flatten() {
        let domain = cookie.get("domain").and_then(Value::as_str).unwrap_or("");
        if cookie_matches(domain, &host).not() {
            continue;
        }
        summary.total += 1;
        let expires = cookie
            .get("expires")
            .and_then(Value::as_f64)
            .unwrap_or(-1.0);
        let session = cookie
            .get("session")
            .and_then(Value::as_bool)
            .unwrap_or(expires <= 0.0);
        if session {
            summary.session += 1;
            continue;
        }
        summary.persistent += 1;
        let expires = expires as i64;
        summary.earliest_expiry = Some(summary.earliest_expiry.map_or(expires, |e| e.min(expires)));
        summary.latest_expiry = Some(summary.latest_expiry.map_or(expires, |e| e.max(expires)));
    }
    summary
}

/// Ask the browser for its cookies and keep only the summary for `host`.
/// Chrome's reply carries cookie values; they are dropped here, unread.
pub async fn cookie_summary(transport: &mut dyn CdpTransport, host: &str) -> Result<CookieSummary> {
    let reply = transport
        .call(None, "Storage.getCookies", json!({}))
        .await?;
    Ok(summarize_cookies(&reply, host))
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserStatus {
    pub endpoint: String,
    pub endpoint_answers: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    pub profile_dir: String,
    pub profile_exists: bool,
    /// A Chrome process holds the profile (it may be one without DevTools).
    pub profile_in_use: bool,
    /// "Continue where you left off": session cookies survive a restart.
    pub restore_on_startup: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_cookies: Option<CookieSummary>,
    pub next: String,
}

/// Where things stand: endpoint, profile, restore setting, and how many
/// cookies the browser holds for the institutional gateway.
pub async fn browser_status(
    cdp_url: &str,
    profile: &Path,
    gateway_url: Option<&str>,
) -> Result<BrowserStatus> {
    cdp::validate_cdp_url(cdp_url)?;
    let browser = endpoint_browser(cdp_url).await;
    let restore = restore_on_startup(profile).unwrap_or(false);
    let gateway_host = gateway_url
        .and_then(|raw| Url::parse(raw).ok())
        .and_then(|url| url.host_str().map(str::to_string));
    let mut gateway_cookies = None;
    if browser.is_some()
        && let Some(host) = gateway_host.as_deref()
        && let Ok(mut transport) = WsCdpTransport::connect(cdp_url).await
    {
        gateway_cookies = cookie_summary(&mut transport, host).await.ok();
    }
    let signed_in = gateway_cookies
        .as_ref()
        .is_some_and(|cookies| cookies.total > 0);
    let next = if browser.is_none() {
        "No browser answers. Start one with `paperbridge browser start`, sign in to your institution in it, then fetch."
    } else if restore.not() {
        "The profile will drop session cookies when this Chrome closes. Close it and run `paperbridge browser start` to set \"Continue where you left off\"."
    } else if gateway_host.is_none() {
        "Set institution_gateway_url to see whether the browser holds a gateway session."
    } else if signed_in {
        "The browser holds gateway cookies. `paperbridge papers fetch --doi <doi>` can run."
    } else {
        "The browser holds no gateway cookies. Sign in to your institution in that window."
    };
    Ok(BrowserStatus {
        endpoint: cdp_url.to_string(),
        endpoint_answers: browser.is_some(),
        browser,
        profile_dir: profile.display().to_string(),
        profile_exists: profile.is_dir(),
        profile_in_use: profile_in_use(profile),
        restore_on_startup: restore,
        gateway_cookies,
        next: next.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFERENCES: &str = include_str!("../../tests/fixtures/fetch/chrome_preferences.json");
    const COOKIES: &str = include_str!("../../tests/fixtures/fetch/cdp_cookies.json");

    fn profile_with(preferences: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(preferences) = preferences {
            std::fs::create_dir_all(dir.path().join("Default")).unwrap();
            std::fs::write(preferences_path(dir.path()), preferences).unwrap();
        }
        dir
    }

    fn read(profile: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(preferences_path(profile)).unwrap()).unwrap()
    }

    #[test]
    fn preferences_edit_sets_the_key_and_preserves_every_other_key() {
        let profile = profile_with(Some(PREFERENCES));
        let before: Value = serde_json::from_str(PREFERENCES).unwrap();
        assert_eq!(before["session"]["restore_on_startup"], 5);
        assert_eq!(restore_on_startup(profile.path()), Some(false));

        assert_eq!(
            ensure_restore_on_startup(profile.path()).unwrap(),
            PreferencesEdit::Updated
        );

        let mut after = read(profile.path());
        assert_eq!(after["session"]["restore_on_startup"], 1);
        assert_eq!(restore_on_startup(profile.path()), Some(true));
        // Everything else is byte-for-byte the same data.
        after["session"]["restore_on_startup"] = json!(5);
        assert_eq!(after, before);
        assert_eq!(
            ensure_restore_on_startup(profile.path()).unwrap(),
            PreferencesEdit::AlreadySet
        );
    }

    #[test]
    fn preferences_are_created_minimal_for_a_new_profile() {
        let profile = profile_with(None);
        assert_eq!(restore_on_startup(profile.path()), None);
        assert_eq!(
            ensure_restore_on_startup(profile.path()).unwrap(),
            PreferencesEdit::Created
        );
        assert_eq!(
            read(profile.path()),
            json!({"session": {"restore_on_startup": 1}})
        );
    }

    #[test]
    fn preferences_edit_refuses_bad_files_without_touching_them() {
        let profile = profile_with(Some("{ not json"));
        assert!(ensure_restore_on_startup(profile.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(preferences_path(profile.path())).unwrap(),
            "{ not json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preferences_edit_refuses_while_the_singleton_lock_is_held() {
        let profile = profile_with(Some(PREFERENCES));
        // Chrome's lock: a symlink to <host>-<pid>. This test process is alive.
        std::os::unix::fs::symlink(
            format!("fixturehost-{}", std::process::id()),
            profile.path().join("SingletonLock"),
        )
        .unwrap();
        assert!(profile_in_use(profile.path()));
        let error = ensure_restore_on_startup(profile.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("Chrome is running"), "{error}");
        assert_eq!(read(profile.path())["session"]["restore_on_startup"], 5);

        // A lock left by a dead process (a crash) does not block the edit.
        std::fs::remove_file(profile.path().join("SingletonLock")).unwrap();
        std::os::unix::fs::symlink("fixturehost-4194999", profile.path().join("SingletonLock"))
            .unwrap();
        if cfg!(target_os = "linux") {
            assert!(profile_in_use(profile.path()).not());
            assert!(ensure_restore_on_startup(profile.path()).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_browser_on_another_profile_is_not_ours_to_close() {
        // Nothing runs on the profile: whatever answers on the port is not it.
        let profile = profile_with(None);
        let error = endpoint_owner(profile.path(), 9555).unwrap_err();
        assert!(
            error.contains("no Chrome is running on the profile"),
            "{error}"
        );

        // A live process holds the profile, but it is not a Chrome started
        // with this port on this profile (it is this test process).
        std::os::unix::fs::symlink(
            format!("fixturehost-{}", std::process::id()),
            profile.path().join("SingletonLock"),
        )
        .unwrap();
        std::fs::write(
            profile.path().join("DevToolsActivePort"),
            "9444\n/devtools/browser/x\n",
        )
        .unwrap();
        let error = endpoint_owner(profile.path(), 9555).unwrap_err();
        assert!(
            error.contains("--remote-debugging-port=9555") || error.contains("DevTools port 9555"),
            "{error}"
        );
    }

    #[test]
    fn launch_arguments_are_exactly_the_documented_ones() {
        let profile = Path::new("/home/op/.local/share/paperbridge/chrome-profile");
        assert_eq!(
            launch_args(9222, profile, false),
            vec![
                "--remote-debugging-port=9222",
                "--no-first-run",
                "--no-default-browser-check",
                "--user-data-dir=/home/op/.local/share/paperbridge/chrome-profile",
            ]
        );
        let headless = launch_args(9333, profile, true);
        assert_eq!(headless.len(), 5);
        assert_eq!(headless[0], "--remote-debugging-port=9333");
        assert_eq!(headless[4], "--headless=new");
        // Never an address flag that would expose the port beyond loopback.
        assert!(
            headless
                .iter()
                .all(|arg| arg.contains("remote-debugging-address").not())
        );
    }

    #[test]
    fn chrome_command_prefers_explicit_then_a_chrome_browser_command() {
        assert_eq!(
            chrome_command(
                Some("/opt/chrome/chrome --ozone-platform=x11"),
                Some("firefox")
            ),
            vec![vec!["/opt/chrome/chrome", "--ozone-platform=x11"]]
        );
        let configured = chrome_command(None, Some("google-chrome-beta %s"));
        assert_eq!(configured[0], vec!["google-chrome-beta"]);
        assert_eq!(configured[1], vec!["google-chrome-stable"]);
        // `browser_command` may name another browser for `papers access`.
        let other = chrome_command(None, Some("firefox --new-tab"));
        assert_eq!(other[0], vec!["google-chrome-stable"]);
        assert_eq!(other.len(), 4);
    }

    #[test]
    fn cookie_summary_reports_counts_and_expiries_and_no_values() {
        let reply: Value = serde_json::from_str(COOKIES).unwrap();
        let summary = summarize_cookies(&reply, "uccs.idm.oclc.org");
        assert_eq!(
            summary,
            CookieSummary {
                host: "uccs.idm.oclc.org".to_string(),
                total: 2,
                session: 1,
                persistent: 1,
                earliest_expiry: Some(1_800_000_000),
                latest_expiry: Some(1_800_000_000),
            }
        );
        // The host-only cookie on `login.<gateway>` counts for that host.
        assert_eq!(
            summarize_cookies(&reply, "login.uccs.idm.oclc.org").total,
            3
        );
        assert_eq!(summarize_cookies(&reply, "example.org").total, 0);
        assert_eq!(summarize_cookies(&json!({}), "uccs.idm.oclc.org").total, 0);
    }

    /// Every string in the fixture that a summary must never carry: each
    /// cookie's name and value.
    fn fixture_names_and_values() -> Vec<String> {
        let reply: Value = serde_json::from_str(COOKIES).unwrap();
        reply["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|cookie| [cookie["name"].clone(), cookie["value"].clone()])
            .map(|field| field.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn cookie_summary_carries_no_cookie_name_or_value() {
        let reply: Value = serde_json::from_str(COOKIES).unwrap();
        let withheld = fixture_names_and_values();
        assert_eq!(withheld.len(), 8);
        for host in ["uccs.idm.oclc.org", "login.microsoftonline.com"] {
            let summary = summarize_cookies(&reply, host);
            assert!(summary.total > 0);
            let printed = serde_json::to_string(&summary).unwrap();
            // Counted, not printed: a failure names no fixture string.
            let leaked = withheld
                .iter()
                .filter(|text| printed.contains(text.as_str()))
                .count();
            assert_eq!(leaked, 0);
            let keys: Vec<String> = serde_json::to_value(&summary)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            assert_eq!(
                keys,
                [
                    "earliest_expiry",
                    "host",
                    "latest_expiry",
                    "persistent",
                    "session",
                    "total"
                ]
            );
        }
    }

    #[test]
    fn cookie_domains_follow_browser_matching() {
        let host = "proxy.example.edu";
        for (domain, expected) in [
            // Host-only cookie: that exact host.
            ("proxy.example.edu", true),
            ("PROXY.example.edu", true),
            // Domain cookies: the host itself, or a parent of it.
            (".proxy.example.edu", true),
            (".example.edu", true),
            // A host-only cookie on a parent is not sent to the host.
            ("example.edu", false),
            // A cookie scoped to a subdomain is never sent to its parent.
            ("evil.proxy.example.edu", false),
            (".evil.proxy.example.edu", false),
            // Not a whole label, and names that only contain the host.
            (".oxy.example.edu", false),
            ("notproxy.example.edu", false),
            ("proxy.example.edu.attacker.example", false),
            (".proxy.example.edu.attacker.example", false),
            ("", false),
            (".", false),
        ] {
            assert_eq!(cookie_matches(domain, host), expected, "{domain:?}");
        }
    }

    #[test]
    fn subdomain_cookies_do_not_make_the_gateway_look_signed_in() {
        let reply = json!({"cookies": [
            {"domain": "evil.proxy.example.edu", "session": true, "expires": -1},
            {"domain": ".evil.proxy.example.edu", "session": false, "expires": 1_900_000_000},
        ]});
        assert_eq!(summarize_cookies(&reply, "proxy.example.edu").total, 0);
        assert_eq!(summarize_cookies(&reply, "evil.proxy.example.edu").total, 2);
    }

    /// A DevTools browser WebSocket on loopback. Answers `Browser.getVersion`;
    /// on `Browser.close` it runs `on_close` and, when that returns true,
    /// stops answering, as an exiting Chrome does.
    async fn devtools_socket(on_close: impl Fn() -> bool + Send + Sync + 'static) -> String {
        use futures::{SinkExt, StreamExt};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let on_close = Arc::new(on_close);
        let gone = Arc::new(AtomicBool::new(false));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if gone.load(Ordering::SeqCst) {
                    continue;
                }
                let (on_close, gone) = (on_close.clone(), gone.clone());
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let request: Value = serde_json::from_str(text.as_str()).unwrap();
                        if request["method"] == "Browser.close" && on_close() {
                            gone.store(true, Ordering::SeqCst);
                            return;
                        }
                        let reply = json!({
                            "id": request["id"],
                            "result": {"product": "Chrome/Fixture"},
                        });
                        if socket.send(Message::text(reply.to_string())).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        format!("ws://127.0.0.1:{port}/devtools/browser/fixture")
    }

    fn port_of(endpoint: &str) -> u16 {
        Url::parse(endpoint).unwrap().port().unwrap()
    }

    /// A long-lived process that holds `profile` the way Chrome does: the
    /// profile and port are on its command line, and the lock names it.
    #[cfg(unix)]
    fn hold_profile(profile: &Path, port: u16) -> std::process::Child {
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("while :; do sleep 1; done")
            .arg("fixture-chrome")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(format!("--remote-debugging-port={port}"))
            .spawn()
            .unwrap();
        std::os::unix::fs::symlink(
            format!("fixturehost-{}", child.id()),
            profile.join("SingletonLock"),
        )
        .unwrap();
        std::fs::write(
            profile.join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/fixture\n"),
        )
        .unwrap();
        child
    }

    #[tokio::test]
    async fn websocket_endpoint_is_probed_like_an_http_one() {
        let endpoint = devtools_socket(|| false).await;
        assert_eq!(
            endpoint_browser(&endpoint).await.as_deref(),
            Some("Chrome/Fixture")
        );

        let profile = profile_with(Some(PREFERENCES));
        let status = browser_status(&endpoint, profile.path(), None)
            .await
            .unwrap();
        assert!(status.endpoint_answers);
        assert_eq!(status.browser.as_deref(), Some("Chrome/Fixture"));

        // `start` sees the running browser and starts nothing.
        let spec = LaunchSpec {
            command: vec!["/nonexistent/chrome".to_string()],
            profile: profile.path().to_path_buf(),
            headless: true,
        };
        let report = ensure_browser(&endpoint, &spec).await.unwrap();
        assert!(report.launched.not());
        assert!(report.note.is_some());

        // `stop` reaches the ownership check instead of "nothing answers".
        let error = stop_browser(&endpoint, profile.path(), false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("not closing the browser"), "{error}");
    }

    #[tokio::test]
    async fn websocket_endpoint_that_is_down_is_reported_and_never_launched() {
        // A loopback port with nothing listening on it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let endpoint = format!("ws://127.0.0.1:{port}/devtools/browser/gone");
        assert_eq!(endpoint_browser(&endpoint).await, None);

        let profile = profile_with(None);
        let status = browser_status(&endpoint, profile.path(), None)
            .await
            .unwrap();
        assert!(status.endpoint_answers.not());

        let stop = stop_browser(&endpoint, profile.path(), false)
            .await
            .unwrap();
        assert_eq!(stop.status, StopStatus::NotRunning);
        assert!(stop.stopped.not() && stop.failure().is_none());

        // The command does not exist: reaching the spawn would say so.
        let spec = LaunchSpec {
            command: vec!["/nonexistent/chrome".to_string()],
            profile: profile.path().to_path_buf(),
            headless: true,
        };
        for error in [
            ensure_browser(&endpoint, &spec).await.unwrap_err(),
            launch_chrome(&endpoint, &spec).await.unwrap_err(),
        ] {
            let error = error.to_string();
            assert!(
                error.contains(&format!("--cdp-url http://127.0.0.1:{port}")),
                "{error}"
            );
        }
        // Nothing was written to the profile on the way to the refusal.
        assert_eq!(restore_on_startup(profile.path()), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_that_does_not_exit_is_reported_as_not_stopped() {
        let profile = profile_with(None);
        // Ignores Browser.close and keeps answering.
        let endpoint = devtools_socket(|| false).await;
        let mut holder = hold_profile(profile.path(), port_of(&endpoint));

        let dry = stop_browser_within(&endpoint, profile.path(), true, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(dry.status, StopStatus::WouldStop);
        assert!(dry.stopped.not() && dry.failure().is_none());

        let report =
            stop_browser_within(&endpoint, profile.path(), false, Duration::from_millis(300))
                .await
                .unwrap();
        let _ = holder.kill();
        let _ = holder.wait();

        assert_eq!(report.status, StopStatus::NotStopped);
        assert!(report.stopped.not());
        assert!(
            report.detail.contains("still answers and still holds"),
            "{}",
            report.detail
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["status"],
            "not_stopped"
        );
        // The caller ends with an error, so the command exits non-zero.
        let failure = report.failure().unwrap().to_string();
        assert!(failure.contains("cookies may not have been written"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_released_endpoint_with_a_held_profile_is_still_not_stopped() {
        let profile = profile_with(None);
        // The DevTools port goes away, but the process keeps the profile.
        let endpoint = devtools_socket(|| true).await;
        let mut holder = hold_profile(profile.path(), port_of(&endpoint));

        let report =
            stop_browser_within(&endpoint, profile.path(), false, Duration::from_millis(300))
                .await
                .unwrap();
        let _ = holder.kill();
        let _ = holder.wait();

        assert_eq!(report.status, StopStatus::NotStopped);
        assert!(report.stopped.not() && report.failure().is_some());
        assert!(
            report.detail.contains("still holds the profile"),
            "{}",
            report.detail
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_that_exits_and_releases_the_profile_is_stopped() {
        let profile = profile_with(None);
        let lock = profile.path().join("SingletonLock");
        // Exiting: the port closes and the profile lock is removed.
        let endpoint = devtools_socket(move || std::fs::remove_file(&lock).is_ok()).await;
        let mut holder = hold_profile(profile.path(), port_of(&endpoint));

        let report = stop_browser_within(&endpoint, profile.path(), false, Duration::from_secs(10))
            .await
            .unwrap();
        let _ = holder.kill();
        let _ = holder.wait();

        assert_eq!(report.status, StopStatus::Stopped);
        assert!(report.stopped && report.failure().is_none());
        assert!(report.detail.starts_with("closed "), "{}", report.detail);
        assert_eq!(serde_json::to_value(&report).unwrap()["status"], "stopped");
    }
}
