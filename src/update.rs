use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tracing::debug;

use crate::error::{Result, ZoteroMcpError};

const RELEASES_URL: &str = "https://api.github.com/repos/trvon/paperbridge/releases/latest";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const HTTP_TIMEOUT_SECS: u64 = 5;
const NPM_PACKAGE: &str = "paperbridge";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    checked_at: u64,
    latest_tag: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
}

/// Check GitHub for a newer release. Cached for 24h on disk; fail-silent on
/// any network or filesystem error so a stale cache or no internet never
/// blocks a real command.
pub async fn check_for_update() -> Option<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION").to_string();

    let latest = match read_cache() {
        Some(entry) if !is_cache_stale(&entry) => entry.latest_tag,
        _ => match fetch_latest_tag().await {
            Ok(tag) => {
                let _ = write_cache(&tag);
                tag
            }
            Err(e) => {
                debug!(error = %e, "update check failed; skipping");
                return None;
            }
        },
    };

    if is_newer(&latest, &current) {
        Some(UpdateInfo { current, latest })
    } else {
        None
    }
}

/// Print a one-line stderr nag when an update is available. No-op when
/// `info` is `None` so callers can pipe through unconditionally.
pub fn print_nag(info: Option<&UpdateInfo>) {
    if let Some(info) = info {
        eprintln!(
            "note: paperbridge {} is available (you have {}); run `paperbridge update`",
            info.latest, info.current
        );
    }
}

/// Run `npm install -g paperbridge@latest`. Surfaces npm's stdout/stderr
/// directly so the user sees the same output they'd see running it manually.
pub async fn run_update() -> Result<()> {
    println!("Running: npm install -g {NPM_PACKAGE}@latest");
    let status = Command::new("npm")
        .args(["install", "-g", &format!("{NPM_PACKAGE}@latest")])
        .status()
        .await
        .map_err(|e| {
            ZoteroMcpError::Config(format!(
                "failed to exec `npm` (is npm installed and on PATH?): {e}"
            ))
        })?;
    if !status.success() {
        return Err(ZoteroMcpError::Config(format!(
            "`npm install -g {NPM_PACKAGE}@latest` exited with {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "<no code>".to_string())
        )));
    }
    let _ = clear_cache();
    Ok(())
}

fn cache_path() -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("paperbridge")
            .join("update_check.json"),
    )
}

fn read_cache() -> Option<CacheEntry> {
    let path = cache_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(tag: &str) -> Result<()> {
    let path =
        cache_path().ok_or_else(|| ZoteroMcpError::Config("no cache dir available".into()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ZoteroMcpError::Config(e.to_string()))?;
    }
    let entry = CacheEntry {
        checked_at: now_secs(),
        latest_tag: tag.to_string(),
    };
    let text = serde_json::to_string(&entry).map_err(|e| ZoteroMcpError::Serde(e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| ZoteroMcpError::Config(e.to_string()))?;
    Ok(())
}

fn clear_cache() -> Result<()> {
    if let Some(path) = cache_path()
        && path.exists()
    {
        std::fs::remove_file(&path).map_err(|e| ZoteroMcpError::Config(e.to_string()))?;
    }
    Ok(())
}

fn is_cache_stale(entry: &CacheEntry) -> bool {
    now_secs().saturating_sub(entry.checked_at) > CACHE_TTL_SECS
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn fetch_latest_tag() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| ZoteroMcpError::Http(e.to_string()))?;
    crate::security::ensure_secure_transport(RELEASES_URL)?;
    let resp = client
        .get(RELEASES_URL)
        .header(
            "User-Agent",
            concat!("paperbridge/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| ZoteroMcpError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(ZoteroMcpError::Http(format!(
            "GitHub releases API returned {}",
            resp.status()
        )));
    }
    let release: GithubRelease = resp
        .json()
        .await
        .map_err(|e| ZoteroMcpError::Http(e.to_string()))?;
    Ok(release.tag_name)
}

/// Compare two version strings (with optional `v` prefix) and return true if
/// `latest` is strictly newer than `current`. Falls back to string equality
/// if either side fails to parse as semver-ish dotted integers — never panics.
fn is_newer(latest: &str, current: &str) -> bool {
    let a = parse_semver(latest);
    let b = parse_semver(current);
    match (a, b) {
        (Some(a), Some(b)) => a > b,
        _ => latest.trim_start_matches('v') != current.trim_start_matches('v'),
    }
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_handles_v_prefix() {
        assert_eq!(parse_semver("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_semver_strips_pre_release_and_build() {
        assert_eq!(parse_semver("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build.5"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_semver_rejects_garbage() {
        assert_eq!(parse_semver("not-a-version"), None);
        assert_eq!(parse_semver("1.2"), None);
    }

    #[test]
    fn is_newer_detects_real_bump() {
        assert!(is_newer("v0.5.0", "0.4.0"));
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn is_newer_rejects_same_or_older() {
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("v0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
    }

    #[test]
    fn cache_stale_after_ttl() {
        let fresh = CacheEntry {
            checked_at: now_secs(),
            latest_tag: "v0.5.0".into(),
        };
        assert!(!is_cache_stale(&fresh));
        let old = CacheEntry {
            checked_at: now_secs().saturating_sub(CACHE_TTL_SECS + 1),
            latest_tag: "v0.5.0".into(),
        };
        assert!(is_cache_stale(&old));
    }
}
