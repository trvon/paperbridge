//! CLI surface for browser choice (`papers access`) and `papers fetch`.
//! No network: `--url` routes resolve locally when institutional access is
//! off, and the fetch cases stop before any request is made.
#![cfg(unix)]

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const ARTICLE: &str = "https://example.org/article?id=1&x=$(id)";

struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let sandbox = Self {
            dir: tempfile::tempdir().expect("temp dir"),
        };
        std::fs::write(
            sandbox.config(),
            format!(
                "backend_mode = 'cloud'\nuser_id = 123\napi_key = 'fixture'\npaperseed_enabled = false\npaperseed_yams_enabled = false\npaperseed_corpus_root = '{}'\ngrobid_auto_spawn = false\nupdate_check_enabled = false\n",
                sandbox.dir.path().join("corpus").display()
            ),
        )
        .unwrap();
        sandbox
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    /// A stand-in browser that records its arguments, one per line.
    fn browser(&self, name: &str) -> (PathBuf, PathBuf) {
        let script = self.dir.path().join(name);
        let log = self.dir.path().join(format!("{name}.args"));
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    fn run(&self, args: &[&str], browser_env: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_paperbridge"));
        command
            .args(args)
            .env("PAPERBRIDGE_CONFIG", self.config())
            .env_remove("ZOTERO_MCP_CONFIG")
            .env_remove("PAPERBRIDGE_BROWSER_COMMAND")
            .env_remove("BROWSER");
        if let Some(value) = browser_env {
            command.env("BROWSER", value);
        }
        command.output().expect("run paperbridge")
    }
}

fn recorded_args(log: &Path) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(log) {
            return text.lines().map(str::to_string).collect();
        }
        assert!(Instant::now() < deadline, "the browser command never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn access_browser_flag_launches_that_command_with_the_route_as_one_argument() {
    let sandbox = Sandbox::new();
    let (chosen, chosen_log) = sandbox.browser("chosen-browser");
    let (other, other_log) = sandbox.browser("env-browser");

    let output = sandbox.run(
        &[
            "--json",
            "papers",
            "access",
            "--url",
            ARTICLE,
            "--browser",
            &format!("{} --new-window", chosen.display()),
        ],
        Some(&other.display().to_string()),
    );

    assert!(output.status.success(), "{output:?}");
    let access: Value = serde_json::from_slice(&output.stdout).expect("JSON on stdout");
    assert_eq!(access["selected_url"], ARTICLE);
    assert_eq!(recorded_args(&chosen_log), vec!["--new-window", ARTICLE]);
    assert!(!other_log.exists(), "--browser wins over $BROWSER");
}

#[test]
fn access_honours_browser_env_before_the_desktop_default() {
    let sandbox = Sandbox::new();
    let (browser, log) = sandbox.browser("env-browser");

    let output = sandbox.run(
        &["papers", "access", "--url", ARTICLE],
        Some(&format!("{} --app=%s", browser.display())),
    );

    assert!(output.status.success(), "{output:?}");
    assert_eq!(recorded_args(&log), vec![format!("--app={ARTICLE}")]);
}

#[test]
fn access_config_browser_command_wins_over_browser_env() {
    let sandbox = Sandbox::new();
    let (configured, configured_log) = sandbox.browser("configured-browser");
    let (other, other_log) = sandbox.browser("env-browser");
    let set = sandbox.run(
        &[
            "config",
            "set",
            "browser_command",
            &configured.display().to_string(),
        ],
        None,
    );
    assert!(set.status.success(), "{set:?}");
    let get = sandbox.run(&["config", "get", "browser_command"], None);
    assert!(
        String::from_utf8_lossy(&get.stdout).contains(&configured.display().to_string()),
        "{get:?}"
    );

    let output = sandbox.run(
        &["papers", "access", "--url", ARTICLE],
        Some(&other.display().to_string()),
    );

    assert!(output.status.success(), "{output:?}");
    assert_eq!(recorded_args(&configured_log), vec![ARTICLE]);
    assert!(!other_log.exists());
}

#[test]
fn access_reports_a_browser_command_that_cannot_start() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(
        &[
            "papers",
            "access",
            "--url",
            ARTICLE,
            "--browser",
            "paperbridge-no-such-browser",
        ],
        None,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("paperbridge-no-such-browser (from --browser)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(ARTICLE),
        "the URL is offered for manual opening"
    );
}

#[test]
fn fetch_refuses_a_remote_devtools_endpoint() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(
        &[
            "--json",
            "papers",
            "fetch",
            "--doi",
            "10.1145/3460120.3484766",
            "--cdp-url",
            "http://192.168.1.20:9222",
        ],
        None,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("stderr is valid JSON");
    assert_eq!(error["error"], "invalid_input");
    assert!(error["reason"].as_str().unwrap().contains("loopback"));
}

#[test]
fn fetch_rejects_an_empty_doi_file_before_touching_the_browser() {
    let sandbox = Sandbox::new();
    let list = sandbox.dir.path().join("dois.txt");
    std::fs::write(&list, "# nothing yet\n\n").unwrap();
    let output = sandbox.run(
        &["papers", "fetch", "--doi-file", &list.display().to_string()],
        None,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("lists no DOIs"));
}

#[test]
fn fetch_help_carries_the_chrome_launch_line_and_privacy_statement() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["papers", "fetch", "--help"], None);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains(
            "google-chrome-stable --remote-debugging-port=9222 --no-first-run --no-default-browser-check --user-data-dir=\"$HOME/.local/share/paperbridge/chrome-profile\""
        ),
        "{help}"
    );
    assert!(help.contains("stores no username, password, or cookie"));
    for flag in [
        "--doi",
        "--doi-file",
        "--cdp-url",
        "--no-ingest",
        "--out",
        "--delay-secs",
    ] {
        assert!(help.contains(flag), "{flag}");
    }
}

fn free_endpoint() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("http://127.0.0.1:{port}")
}

#[test]
fn browser_status_without_a_browser_reports_state_and_the_next_step() {
    let sandbox = Sandbox::new();
    let profile = sandbox.dir.path().join("profile");
    let endpoint = free_endpoint();
    let output = sandbox.run(
        &[
            "--json",
            "browser",
            "status",
            "--cdp-url",
            &endpoint,
            "--profile",
            &profile.display().to_string(),
        ],
        None,
    );
    assert!(output.status.success(), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["endpoint_answers"], false);
    assert_eq!(status["profile_exists"], false);
    assert_eq!(status["profile_in_use"], false);
    assert_eq!(status["restore_on_startup"], false);
    assert_eq!(status["profile_dir"], profile.display().to_string());
    assert!(status.get("gateway_cookies").is_none());
    assert!(
        status["next"]
            .as_str()
            .unwrap()
            .contains("paperbridge browser start")
    );
    // Status never creates or edits the profile.
    assert!(!profile.exists());

    let stop = sandbox.run(
        &[
            "--json",
            "browser",
            "stop",
            "--cdp-url",
            &endpoint,
            "--profile",
            &profile.display().to_string(),
        ],
        None,
    );
    assert!(stop.status.success());
    let stopped: Value = serde_json::from_slice(&stop.stdout).unwrap();
    assert_eq!(stopped["stopped"], false);
    assert_eq!(stopped["detail"], "nothing answers at this endpoint");

    let remote = sandbox.run(
        &["browser", "status", "--cdp-url", "http://192.168.1.20:9222"],
        None,
    );
    assert!(!remote.status.success());
    assert!(String::from_utf8_lossy(&remote.stderr).contains("loopback"));
}

#[test]
fn browser_start_reports_a_missing_chrome_without_touching_the_profile() {
    let sandbox = Sandbox::new();
    let profile = sandbox.dir.path().join("profile");
    let output = sandbox.run(
        &[
            "browser",
            "start",
            "--cdp-url",
            &free_endpoint(),
            "--profile",
            &profile.display().to_string(),
            "--chrome",
            "paperbridge-no-such-chrome",
        ],
        None,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no Chrome found"), "{stderr}");
    assert!(stderr.contains("paperbridge-no-such-chrome"), "{stderr}");
    assert!(!profile.exists());
}

/// Optional: `browser start --headless`, `status`, `stop` against a real Chrome.
/// Run with `cargo test --test cli_fetch -- --ignored`.
#[test]
#[ignore = "needs a local Chrome or Chromium binary"]
fn browser_start_status_stop_round_trip_with_real_headless_chrome() {
    let sandbox = Sandbox::new();
    let profile = sandbox.dir.path().join("profile").display().to_string();
    let endpoint = free_endpoint();
    std::fs::write(
        sandbox.config(),
        format!(
            "{}institution_access_mode = 'fallback'\ninstitution_gateway_url = 'https://proxy.example.edu/login?url='\n",
            std::fs::read_to_string(sandbox.config()).unwrap()
        ),
    )
    .unwrap();
    let common = [
        "--cdp-url",
        endpoint.as_str(),
        "--profile",
        profile.as_str(),
    ];

    let start = sandbox.run(
        &[&["--json", "browser", "start", "--headless"], &common[..]].concat(),
        None,
    );
    assert!(start.status.success(), "{start:?}");
    let started: Value = serde_json::from_slice(&start.stdout).unwrap();
    assert_eq!(started["launched"], true);
    assert_eq!(started["preferences"], "created");

    let status = sandbox.run(
        &[&["--json", "browser", "status"], &common[..]].concat(),
        None,
    );
    // The same endpoint with another profile: that browser is not the one
    // on the named profile, so it is refused and keeps running.
    let other = sandbox
        .dir
        .path()
        .join("other-profile")
        .display()
        .to_string();
    let refused = sandbox.run(
        &[
            "browser",
            "stop",
            "--cdp-url",
            endpoint.as_str(),
            "--profile",
            other.as_str(),
        ],
        None,
    );
    let dry = sandbox.run(
        &[&["--json", "browser", "stop", "--dry-run"], &common[..]].concat(),
        None,
    );
    let still = sandbox.run(
        &[&["--json", "browser", "status"], &common[..]].concat(),
        None,
    );
    let stop = sandbox.run(
        &[&["--json", "browser", "stop"], &common[..]].concat(),
        None,
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not closing the browser"));
    let dry: Value = serde_json::from_slice(&dry.stdout).unwrap();
    assert_eq!(dry["stopped"], false);
    assert!(
        dry["detail"].as_str().unwrap().starts_with("would close "),
        "{dry}"
    );
    assert!(dry["pid"].as_u64().is_some());
    let still: Value = serde_json::from_slice(&still.stdout).unwrap();
    assert_eq!(
        still["endpoint_answers"], true,
        "refusal and dry run closed nothing"
    );
    assert!(stop.status.success(), "{stop:?}");
    assert!(status.status.success(), "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["endpoint_answers"], true);
    assert_eq!(status["profile_in_use"], true);
    assert_eq!(status["restore_on_startup"], true);
    assert_eq!(status["gateway_cookies"]["host"], "proxy.example.edu");
    assert_eq!(status["gateway_cookies"]["total"], 0);
    let stopped: Value = serde_json::from_slice(&stop.stdout).unwrap();
    assert_eq!(stopped["stopped"], true);

    let after = sandbox.run(
        &[&["--json", "browser", "status"], &common[..]].concat(),
        None,
    );
    let after: Value = serde_json::from_slice(&after.stdout).unwrap();
    assert_eq!(after["endpoint_answers"], false);
    assert_eq!(after["profile_in_use"], false);
    assert_eq!(
        after["restore_on_startup"], true,
        "the setting outlives the browser"
    );
}

/// A browser that Paperbridge did not start, on a profile it does not know:
/// `browser stop` must refuse and must not even open a DevTools connection.
/// The "browser" is a local mock of the DevTools HTTP endpoint.
#[test]
fn browser_stop_refuses_a_browser_on_another_profile() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/json/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "Browser": "Chrome/Operator",
                "webSocketDebuggerUrl": format!(
                    "ws://{}/devtools/browser/operator",
                    server.address()
                ),
            })))
            .mount(&server)
            .await;
        server
    });
    let sandbox = Sandbox::new();
    let profile = sandbox.dir.path().join("profile").display().to_string();

    for extra in [&[][..], &["--dry-run"][..]] {
        let output = sandbox.run(
            &[
                &[
                    "--json",
                    "browser",
                    "stop",
                    "--cdp-url",
                    server.uri().as_str(),
                    "--profile",
                    profile.as_str(),
                ],
                extra,
            ]
            .concat(),
            None,
        );
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        let reason = error["reason"].as_str().unwrap();
        assert!(reason.contains("not closing the browser"), "{reason}");
        assert!(
            reason.contains("Chrome/Operator"),
            "it says what answers there: {reason}"
        );
        assert!(
            reason.contains("no Chrome is running on the profile"),
            "{reason}"
        );
    }
    // Only the version probe reached the endpoint: no DevTools connection,
    // so no Browser.close could have been sent.
    let requests = runtime.block_on(server.received_requests()).unwrap();
    assert!(!requests.is_empty());
    assert!(
        requests
            .iter()
            .all(|request| request.url.path() == "/json/version"),
        "{requests:?}"
    );
}

/// The live crash, at the process level: a PDF that panics the PDF library
/// must give an ordinary error (exit 1), not a panic abort (exit 101), and
/// can still be stored without text.
#[test]
fn pdf_that_panics_the_extractor_does_not_abort_the_process() {
    let sandbox = Sandbox::new();
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/crates/paperseed/tests/fixtures/tounicode_ligature_range.pdf"
    );
    let output = sandbox.run(&["--json", "paperseed", "corpus", "import", fixture], None);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("thread 'main' panicked"), "{stderr}");
    let error: Value = serde_json::from_slice(&output.stderr).expect("a structured error");
    assert!(
        error["reason"]
            .as_str()
            .unwrap()
            .contains("built-in PDF extractor panicked")
    );

    let stored = sandbox.run(
        &[
            "--json",
            "paperseed",
            "corpus",
            "import",
            fixture,
            "--no-fulltext",
        ],
        None,
    );
    assert_eq!(stored.status.code(), Some(0), "{stored:?}");
}
