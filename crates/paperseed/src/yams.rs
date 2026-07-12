use crate::db::QueryHit;
use crate::models::LocalPaper;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YamsConfig {
    pub enabled: bool,
    pub binary: PathBuf,
}

impl YamsConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            binary: PathBuf::from("yams"),
        }
    }

    pub fn auto_detect() -> Self {
        Self {
            // The CLI can serve searches through the daemon or initialize its
            // local search services. Treat an installed binary as usable and
            // report individual command failures at the call site instead of
            // permanently disabling YAMS because a point-in-time daemon probe
            // was stale.
            enabled: yams_available("yams"),
            binary: PathBuf::from("yams"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YamsHealth {
    pub binary_available: bool,
    pub daemon_running: bool,
}

impl YamsHealth {
    pub fn ready(&self) -> bool {
        self.binary_available && self.daemon_running
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamsIndexRequest<'a> {
    pub paper: &'a LocalPaper,
    pub full_text: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamsDownloadRequest<'a> {
    pub url: &'a str,
    pub title: Option<&'a str>,
    pub doi: Option<&'a str>,
    pub source_url: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YamsDownloadResult {
    Stored {
        hash: Option<String>,
        stored_path: Option<PathBuf>,
        job_id: Option<String>,
        state: Option<String>,
    },
    Queued {
        job_id: String,
        state: Option<String>,
    },
}

impl YamsDownloadResult {
    pub fn hash(&self) -> Option<&str> {
        match self {
            Self::Stored { hash, .. } => hash.as_deref(),
            Self::Queued { .. } => None,
        }
    }
}

pub trait YamsRunner {
    fn run(&self, args: &[String]) -> std::io::Result<YamsOutput>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamsOutput {
    pub status_success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// A global YAMS result that can be surfaced independently of the Paperseed
/// corpus database.
#[derive(Debug, Clone, PartialEq)]
pub struct YamsResearchHit {
    pub hash: String,
    pub path: PathBuf,
    pub score: f64,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamsStoredDocument {
    pub hash: String,
    pub path: PathBuf,
    pub indexed: i64,
}

#[derive(Debug, Clone)]
pub struct CommandYamsRunner {
    binary: PathBuf,
    timeout: Duration,
}

impl CommandYamsRunner {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: Duration::from_secs(2),
        }
    }

    pub fn with_timeout(binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            timeout,
        }
    }
}

impl YamsRunner for CommandYamsRunner {
    fn run(&self, args: &[String]) -> std::io::Result<YamsOutput> {
        run_command_with_timeout(&self.binary, args, self.timeout)
    }
}

pub fn yams_available(binary: &str) -> bool {
    run_command_with_timeout(
        &PathBuf::from(binary),
        &["--version".to_string()],
        Duration::from_millis(500),
    )
    .map(|output| output.status_success)
    .unwrap_or(false)
}

pub fn yams_daemon_running(binary: &str) -> bool {
    run_command_with_timeout(
        &PathBuf::from(binary),
        &["daemon".to_string(), "status".to_string(), "-d".to_string()],
        Duration::from_secs(2),
    )
    .map(|output| yams_status_indicates_running(&output))
    .unwrap_or(false)
}

pub fn yams_health(binary: &str) -> YamsHealth {
    let binary_available = yams_available(binary);
    let daemon_running = binary_available && yams_daemon_running(binary);
    YamsHealth {
        binary_available,
        daemon_running,
    }
}

pub fn yams_ready(binary: &str) -> bool {
    yams_health(binary).ready()
}

fn yams_status_indicates_running(output: &YamsOutput) -> bool {
    if !output.status_success {
        return false;
    }
    let text = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    !text.contains("not running")
        && !text.contains("stopped")
        && !text.contains("unavailable")
        && !text.contains("failed")
}

fn run_command_with_timeout(
    binary: &PathBuf,
    args: &[String],
    timeout: Duration,
) -> std::io::Result<YamsOutput> {
    let mut child = Command::new(binary)
        .args(args)
        // YAMS currently treats an immediate stdin EOF as a signal to fall
        // back from its daemon and may fail local search initialization. Keep
        // an idle pipe open for the command lifetime without exposing the
        // parent MCP/CLI stdin stream to the child.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let _stdin = child.stdin.take();
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            return Ok(YamsOutput {
                status_success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Ok(YamsOutput {
                status_success: false,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: "yams command timed out".to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn index_paper_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    request: YamsIndexRequest<'_>,
) -> Option<String> {
    if !config.enabled {
        return None;
    }

    let mut text = format!(
        "{}\n{}\n{}",
        request.paper.metadata.title,
        request.paper.metadata.authors.join(", "),
        request.paper.metadata.doi.as_deref().unwrap_or_default()
    );
    if let Some(full_text) = request.full_text {
        text.push('\n');
        text.push_str(full_text);
    }

    // `--name=...` (one token) instead of `--name <title>` (two tokens) so a
    // title starting with `-` can't be parsed as a YAMS flag. Same shape used
    // for every other arg-with-value below for consistency.
    let mut args = vec![
        // `--json` is a YAMS global option for `add`; placing it after the
        // subcommand currently leaves human output even though parsing
        // succeeds.
        "--json".to_string(),
        "add".to_string(),
        request.paper.file.path.display().to_string(),
        format!("--name={}", request.paper.metadata.title),
        "--tags=paperseed,paperbridge,paper".to_string(),
        "--collection=paperbridge".to_string(),
        format!("--metadata=paperseed_id={}", request.paper.metadata.id),
        format!(
            "--metadata=doi={}",
            request.paper.metadata.doi.as_deref().unwrap_or_default()
        ),
        // Char count, not byte length — YAMS metadata is informational and
        // `text.len()` over-reports for non-ASCII titles/authors/full text.
        format!("--metadata=paperseed_text_chars={}", text.chars().count()),
    ];
    if !request.paper.metadata.authors.is_empty() {
        args.push(format!(
            "--metadata=authors={}",
            request.paper.metadata.authors.join("; ")
        ));
    }
    if let Some(year) = request.paper.metadata.year {
        args.push(format!("--metadata=year={year}"));
    }
    if let Some(venue) = request.paper.metadata.venue.as_deref() {
        args.push(format!("--metadata=venue={venue}"));
    }
    if let Some(source_url) = request.paper.metadata.source_url.as_deref() {
        args.push(format!("--metadata=source_url={source_url}"));
    }
    args.extend([
        "--no-session".to_string(),
        "--sync".to_string(),
        "--sync-timeout=30".to_string(),
        "--verify".to_string(),
    ]);

    let output = match runner.run(&args) {
        Ok(out) => out,
        Err(err) => {
            eprintln!(
                "paperseed: yams add failed to spawn for paper {}: {err}",
                request.paper.metadata.id
            );
            return None;
        }
    };
    if !output.status_success {
        eprintln!(
            "paperseed: yams add returned non-success for paper {}: {}",
            request.paper.metadata.id,
            output.stderr.trim()
        );
        return None;
    }
    parse_add_hash(&output.stdout)
}

pub fn download_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    request: YamsDownloadRequest<'_>,
) -> Option<YamsDownloadResult> {
    if !config.enabled {
        return None;
    }

    let mut args = vec![
        "download".to_string(),
        request.url.to_string(),
        "--tag".to_string(),
        "paperseed".to_string(),
        "--tag".to_string(),
        "paperbridge".to_string(),
        "--tag".to_string(),
        "paper".to_string(),
    ];
    if let Some(title) = request.title {
        args.extend(["--meta".to_string(), format!("paperseed_title={title}")]);
    }
    if let Some(doi) = request.doi {
        args.extend(["--meta".to_string(), format!("doi={doi}")]);
    }
    if let Some(source_url) = request.source_url {
        args.extend(["--meta".to_string(), format!("source_url={source_url}")]);
    }
    args.extend(["--json".to_string(), "--quiet".to_string()]);

    let output = runner.run(&args).ok()?;
    if !output.status_success {
        return None;
    }
    parse_download_result(&output.stdout)
}

pub fn download_status_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    job_id: &str,
) -> Option<YamsDownloadResult> {
    if !config.enabled || job_id.trim().is_empty() {
        return None;
    }
    let args = vec![
        "download".to_string(),
        "--status".to_string(),
        job_id.to_string(),
        "--json".to_string(),
        "--quiet".to_string(),
    ];
    let output = runner.run(&args).ok()?;
    if !output.status_success {
        return None;
    }
    parse_download_result(&output.stdout)
}

pub fn cat_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    hash: &str,
) -> Option<String> {
    if !config.enabled || hash.trim().is_empty() {
        return None;
    }
    let args = vec!["cat".to_string(), hash.to_string()];
    let output = runner.run(&args).ok()?;
    if !output.status_success || output.stdout.trim().is_empty() {
        return None;
    }
    Some(output.stdout)
}

pub fn query_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    q: &str,
) -> Option<Vec<QueryHit>> {
    if !config.enabled || q.trim().is_empty() {
        return None;
    }
    let args = vec![
        "search".to_string(),
        q.to_string(),
        "--json".to_string(),
        "--limit".to_string(),
        "20".to_string(),
        "--no-session".to_string(),
    ];
    let output = runner.run(&args).ok()?;
    if !output.status_success {
        return None;
    }
    parse_yams_hits(&output.stdout).ok()
}

pub fn query_research_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    q: &str,
    limit: usize,
) -> Result<Vec<YamsResearchHit>, String> {
    if !config.enabled || q.trim().is_empty() || limit == 0 {
        return Err("YAMS research query is disabled or empty".into());
    }
    let args = vec![
        "search".to_string(),
        q.to_string(),
        "--json".to_string(),
        "--show-hash".to_string(),
        "--limit".to_string(),
        limit.to_string(),
        "--no-session".to_string(),
    ];
    let output = runner
        .run(&args)
        .map_err(|error| format!("failed to start YAMS search: {error}"))?;
    if !output.status_success {
        return Err(if output.stderr.trim().is_empty() {
            "YAMS search returned a non-success status".into()
        } else {
            output.stderr.trim().to_string()
        });
    }
    parse_research_hits(&output.stdout).map_err(|error| format!("invalid YAMS JSON: {error}"))
}

pub fn parse_research_hits(raw: &str) -> serde_json::Result<Vec<YamsResearchHit>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let items = value
        .as_array()
        .cloned()
        .or_else(|| value.get("results").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    Ok(items
        .into_iter()
        .filter_map(|item| {
            let hash = item.get("hash")?.as_str()?.trim();
            let path = item.get("path")?.as_str()?.trim();
            if hash.is_empty() || path.is_empty() {
                return None;
            }
            Some(YamsResearchHit {
                hash: hash.to_string(),
                path: PathBuf::from(path),
                score: item.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                snippet: item
                    .get("snippet")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

pub fn list_research_group_with_runner(
    config: &YamsConfig,
    runner: &impl YamsRunner,
    group: &std::path::Path,
    limit: usize,
) -> Result<Vec<YamsStoredDocument>, String> {
    if !config.enabled || limit == 0 {
        return Err("YAMS research listing is disabled".into());
    }
    let pattern = format!("{}/*", group.display());
    let args = vec![
        "list".to_string(),
        pattern,
        "--format".to_string(),
        "json".to_string(),
        "--show-deleted".to_string(),
        "--no-snippets".to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ];
    let output = runner
        .run(&args)
        .map_err(|error| format!("failed to start YAMS list: {error}"))?;
    if !output.status_success {
        return Err(if output.stderr.trim().is_empty() {
            "YAMS list returned a non-success status".into()
        } else {
            output.stderr.trim().to_string()
        });
    }
    parse_stored_documents(&output.stdout).map_err(|error| format!("invalid YAMS JSON: {error}"))
}

pub fn parse_stored_documents(raw: &str) -> serde_json::Result<Vec<YamsStoredDocument>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    Ok(value
        .get("documents")
        .and_then(|documents| documents.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(YamsStoredDocument {
                hash: item.get("hash")?.as_str()?.to_string(),
                path: PathBuf::from(item.get("path")?.as_str()?),
                indexed: item.get("indexed").and_then(|v| v.as_i64()).unwrap_or(0),
            })
        })
        .collect())
}

pub fn parse_yams_hits(raw: &str) -> serde_json::Result<Vec<QueryHit>> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let items = value
        .as_array()
        .cloned()
        .or_else(|| {
            value
                .get("results")
                .and_then(|value| value.as_array())
                .cloned()
        })
        .unwrap_or_default();
    Ok(items
        .into_iter()
        .enumerate()
        .map(|(index, item)| QueryHit {
            id: yams_string_field(&item, &["paperseed_id", "id"])
                .unwrap_or_else(|| format!("yams-{index}")),
            title: item
                .get("title")
                .or_else(|| item.get("name"))
                .or_else(|| {
                    item.get("metadata")
                        .and_then(|metadata| metadata.get("title"))
                })
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| "Untitled YAMS result".to_string()),
            score: item
                .get("score")
                .and_then(|value| value.as_u64())
                .and_then(|score| usize::try_from(score).ok())
                .unwrap_or(1),
            path: item
                .get("path")
                .and_then(|value| value.as_str())
                .map(PathBuf::from)
                .unwrap_or_default(),
        })
        .collect())
}

fn yams_string_field(item: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        item.get(key)
            .or_else(|| item.get("metadata").and_then(|metadata| metadata.get(key)))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
    })
}

fn parse_add_hash(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let Some(hash) = value.get("hash").and_then(|value| value.as_str()) {
        return Some(hash.to_string());
    }
    let items = value
        .as_array()
        .or_else(|| value.get("results").and_then(|results| results.as_array()))?;
    items.iter().find_map(|item| {
        item.get("hash")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned)
    })
}

fn parse_download_result(raw: &str) -> Option<YamsDownloadResult> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let success = value
        .get("success")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let job_id = value
        .get("job_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let state = value
        .get("state")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let hash = value
        .get("hash")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let stored_path = value
        .get("stored_path")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let state_lower = state.as_deref().unwrap_or_default().to_ascii_lowercase();
    let pending = matches!(
        state_lower.as_str(),
        "queued" | "running" | "pending" | "accepted"
    );

    if let Some(job_id) = job_id.clone()
        && pending
        && hash.is_none()
        && stored_path.is_none()
    {
        return Some(YamsDownloadResult::Queued { job_id, state });
    }
    if success || job_id.is_some() || hash.is_some() || stored_path.is_some() {
        return Some(YamsDownloadResult::Stored {
            hash,
            stored_path,
            job_id,
            state,
        });
    }
    None
}
