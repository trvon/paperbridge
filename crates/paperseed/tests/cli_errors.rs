use std::process::Command;

#[test]
fn json_runtime_errors_use_stable_stderr_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_paperseed"))
        .args([
            "--json",
            "--corpus-root",
            dir.path().to_str().unwrap(),
            "corpus",
            "show",
            "missing-id",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(envelope["error"], "paperseed operation failed");
    assert!(envelope["reason"].as_str().unwrap().contains("not found"));
    assert!(
        envelope["try"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
}
