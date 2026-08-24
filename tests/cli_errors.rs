use serde_json::Value;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("temp config dir");
    Command::new(env!("CARGO_BIN_EXE_paperbridge"))
        .args(args)
        .env("PAPERBRIDGE_CONFIG", dir.path().join("config.toml"))
        .env_remove("ZOTERO_MCP_CONFIG")
        .output()
        .expect("run paperbridge")
}

#[test]
fn json_runtime_errors_are_structured_on_stderr() {
    let output = run(&["config", "get", "definitely-not-a-key", "--json"]);

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "error must not contaminate stdout"
    );

    let error: Value = serde_json::from_slice(&output.stderr).expect("stderr is valid JSON");
    assert_eq!(error["error"], "invalid_input");
    assert!(
        error["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("Unknown config key"))
    );
    assert_eq!(error["try"][0], "paperbridge config get");
}

#[test]
fn human_runtime_errors_remain_plain_text() {
    let output = run(&["config", "get", "definitely-not-a-key"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf8");
    assert!(stderr.starts_with("Error: invalid input:"));
    assert!(serde_json::from_str::<Value>(&stderr).is_err());
}

#[test]
fn json_runtime_errors_redact_configured_secret_values() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let secret = "do-not-print-this-secret";
    let output = Command::new(env!("CARGO_BIN_EXE_paperbridge"))
        .args(["config", "validate", "--json"])
        .env("PAPERBRIDGE_CONFIG", dir.path().join("config.toml"))
        .env("PAPERBRIDGE_BACKEND_MODE", "local")
        .env("PAPERBRIDGE_API_KEY", secret)
        .env("PAPERBRIDGE_TIMEOUT_SECS", secret)
        .output()
        .expect("run paperbridge");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr is utf8");
    assert!(!stderr.contains(secret));
    let error: Value = serde_json::from_str(&stderr).expect("stderr is valid JSON");
    assert_eq!(error["error"], "configuration_error");
    assert!(
        error["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("<redacted>"))
    );
}
