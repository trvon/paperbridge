use paperseed::app::{
    CorpusPaths, ImportRequest, import_with_yams, import_with_yams_runner,
    query_entries_with_yams_runner, query_with_yams,
};
use paperseed::db::QueryHit;
use paperseed::models::{License, LocalPaper, PaperMetadata, StoredFile};
use paperseed::yams::{
    YamsConfig, YamsDownloadRequest, YamsIndexRequest, YamsOutput, YamsRunner,
    download_with_runner, index_paper_with_runner, parse_yams_hits, query_with_runner,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn default_corpus_root_uses_xdg_data_home_when_set() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", dir.path());
    }
    let root = paperseed::app::default_corpus_root();
    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
    }
    assert_eq!(root, dir.path().join("paperbridge").join("paperseed"));
}

#[test]
fn default_corpus_root_falls_back_to_home_local_share() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
        std::env::set_var("HOME", dir.path());
    }
    let root = paperseed::app::default_corpus_root();
    unsafe {
        std::env::remove_var("HOME");
    }
    assert_eq!(
        root,
        dir.path()
            .join(".local")
            .join("share")
            .join("paperbridge")
            .join("paperseed")
    );
}

#[test]
fn parses_yams_json_array_and_object_results() {
    let hits = parse_yams_hits(
        r#"{"results":[{"id":"1","title":"Paper","score":3,"path":"/tmp/paper.pdf"}]}"#,
    )
    .unwrap();
    assert_eq!(
        hits,
        vec![QueryHit {
            id: "1".to_string(),
            title: "Paper".to_string(),
            score: 3,
            path: PathBuf::from("/tmp/paper.pdf"),
        }]
    );
}

#[test]
fn yams_query_falls_back_on_invalid_json() {
    let runner = FakeRunner::new(YamsOutput {
        status_success: true,
        stdout: "not json".to_string(),
        stderr: String::new(),
    });
    let config = YamsConfig {
        enabled: true,
        binary: PathBuf::from("yams"),
    };
    assert!(query_with_runner(&config, &runner, "paper").is_none());
}

#[test]
fn yams_query_returns_none_when_disabled() {
    let runner = FakeRunner::new(YamsOutput {
        status_success: true,
        stdout: "[]".to_string(),
        stderr: String::new(),
    });
    assert!(query_with_runner(&YamsConfig::disabled(), &runner, "paper").is_none());
    assert_eq!(runner.calls.borrow().len(), 0);
}

#[test]
fn yams_query_maps_paperseed_metadata_for_retrieval() {
    let runner = FakeRunner::new(YamsOutput {
        status_success: true,
        stdout: r#"{"results":[{"id":"yams-doc","name":"Stored doc","path":"/tmp/doc.pdf","metadata":{"paperseed_id":"paper-123","title":"Stored Paper"}}]}"#.to_string(),
        stderr: String::new(),
    });
    let config = YamsConfig {
        enabled: true,
        binary: PathBuf::from("yams"),
    };

    let hits = query_with_runner(&config, &runner, "stored").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "paper-123");
    assert_eq!(hits[0].title, "Stored doc");

    let calls = runner.calls.borrow();
    assert_eq!(calls[0][0], "search");
    assert!(calls[0].contains(&"--json".to_string()));
}

#[test]
fn yams_index_sends_title_path_and_text() {
    let runner = FakeRunner::new(YamsOutput {
        status_success: true,
        stdout: r#"[{"success":true,"hash":"yams-hash-1"}]"#.to_string(),
        stderr: String::new(),
    });
    let config = YamsConfig {
        enabled: true,
        binary: PathBuf::from("yams"),
    };
    let paper = fake_paper();

    assert_eq!(
        index_paper_with_runner(
            &config,
            &runner,
            YamsIndexRequest {
                paper: &paper,
                full_text: Some("body text")
            }
        ),
        Some("yams-hash-1".to_string())
    );

    let calls = runner.calls.borrow();
    let args = &calls[0];
    assert!(args.contains(&"add".to_string()));
    assert!(args.contains(&paper.file.path.display().to_string()));
    assert!(args.contains(&"--name".to_string()));
    assert!(args.contains(&"YAMS Paper".to_string()));
    assert!(args.contains(&"--tags".to_string()));
    assert!(args.contains(&"paperseed,paperbridge,paper".to_string()));
    assert!(args.iter().any(|arg| arg.contains("paperseed_text_chars")));
}

#[test]
fn import_and_query_with_yams_preserve_fallback_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("paper.txt");
    std::fs::write(&source, "alpha beta beta").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    let paper = import_with_yams(
        &paths,
        ImportRequest {
            path: source,
            title: Some("Alpha".to_string()),
            license: Some("private".to_string()),
            yams_hash: None,
        },
        &YamsConfig::disabled(),
    )
    .unwrap();

    let hits = query_with_yams(&paths, "beta", &YamsConfig::disabled()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, paper.metadata.id);
}

#[test]
fn append_to_yams_search_and_retrieve_paper_content() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("spectral.txt");
    let body = "spectral llama retrieval content lives in the local corpus";
    std::fs::write(&source, body).unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let config = YamsConfig {
        enabled: true,
        binary: PathBuf::from("yams"),
    };
    let runner = SequenceRunner::new(vec![
        YamsOutput {
            status_success: true,
            stdout: r#"[{"success":true,"hash":"spectral-hash"}]"#.to_string(),
            stderr: String::new(),
        },
        YamsOutput {
            status_success: true,
            stdout: String::new(),
            stderr: String::new(),
        },
    ]);

    let paper = import_with_yams_runner(
        &paths,
        ImportRequest {
            path: source,
            title: Some("Spectral Llama".to_string()),
            license: Some("cc-by".to_string()),
            yams_hash: None,
        },
        &config,
        &runner,
    )
    .unwrap();
    runner.replace_output(
        0,
        YamsOutput {
            status_success: true,
            stdout: format!(
                r#"{{"results":[{{"name":"Spectral Llama","path":"{}","metadata":{{"paperseed_id":"{}"}}}}]}}"#,
                paper.file.path.display(),
                paper.metadata.id
            ),
            stderr: String::new(),
        },
    );

    let entries = query_entries_with_yams_runner(&paths, "spectral llama", &config, &runner)
        .expect("query entries via yams");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].paper.metadata.id, paper.metadata.id);
    assert_eq!(entries[0].full_text.as_deref(), Some(body));

    let calls = runner.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0][0], "add");
    assert_eq!(calls[1][0], "search");
    assert_eq!(calls[1][1], "spectral llama");
}

#[test]
fn yams_download_queue_indexes_ten_search_results() {
    let runner = ExportingRunner::new(YamsOutput {
        status_success: true,
        stdout: r#"{"success":true,"job_id":"download-1","state":"queued"}"#.to_string(),
        stderr: String::new(),
    });
    let config = YamsConfig {
        enabled: true,
        binary: PathBuf::from("yams"),
    };

    for index in 0..10 {
        let result = download_with_runner(
            &config,
            &runner,
            YamsDownloadRequest {
                url: &format!("https://example.org/open/paper-{index}.txt"),
                title: Some(&format!("Queued Paper {index}")),
                doi: Some(&format!("10.5555/queued.{index}")),
                source_url: Some("https://example.org/search"),
            },
        );
        assert!(result.is_some());
    }

    let calls = runner.calls.borrow();
    assert_eq!(calls.len(), 10);
    for (index, args) in calls.iter().enumerate() {
        assert_eq!(args[0], "download");
        assert_eq!(
            args[1],
            format!("https://example.org/open/paper-{index}.txt")
        );
        assert!(args.contains(&"--tag".to_string()));
        assert!(args.contains(&"paperseed".to_string()));
        assert!(args.contains(&"paperbridge".to_string()));
        assert!(args.contains(&"--meta".to_string()));
        assert!(args.contains(&format!("doi=10.5555/queued.{index}")));
        assert!(args.contains(&"--json".to_string()));
    }
}

#[test]
fn local_corpus_validates_ten_searches_and_content_without_yams() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    for index in 0..10 {
        let source = dir.path().join(format!("local-{index}.txt"));
        std::fs::write(
            &source,
            format!("local validation paper {index} unique-token-{index} body content"),
        )
        .unwrap();
        import_with_yams(
            &paths,
            ImportRequest {
                path: source,
                title: Some(format!("Local Validation Paper {index}")),
                license: Some("cc-by".to_string()),
                yams_hash: None,
            },
            &YamsConfig::disabled(),
        )
        .unwrap();
    }

    for index in 0..10 {
        let entries = paperseed::app::query_entries_with_yams(
            &paths,
            &format!("unique-token-{index}"),
            &YamsConfig::disabled(),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        let expected = format!("local validation paper {index} unique-token-{index} body content");
        assert_eq!(entries[0].full_text.as_deref(), Some(expected.as_str()));
    }
}

fn fake_paper() -> LocalPaper {
    LocalPaper {
        metadata: PaperMetadata {
            id: "paper1".to_string(),
            title: "YAMS Paper".to_string(),
            doi: Some("10.1/yams".to_string()),
            authors: vec!["Ada Lovelace".to_string()],
            year: Some(2024),
            venue: Some("Journal".to_string()),
            license: License::UserOwnedPrivate,
            source_url: None,
        },
        file: StoredFile {
            hash: "abcdef".to_string(),
            path: PathBuf::from("/tmp/yams-paper.txt"),
            size_bytes: 10,
            mime: "text/plain".to_string(),
        },
    }
}

struct FakeRunner {
    output: YamsOutput,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeRunner {
    fn new(output: YamsOutput) -> Self {
        Self {
            output,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl YamsRunner for FakeRunner {
    fn run(&self, args: &[String]) -> std::io::Result<YamsOutput> {
        self.calls.borrow_mut().push(args.to_vec());
        Ok(self.output.clone())
    }
}

struct SequenceRunner {
    outputs: RefCell<VecDeque<YamsOutput>>,
    calls: RefCell<Vec<Vec<String>>>,
}

struct ExportingRunner {
    output: YamsOutput,
    calls: RefCell<Vec<Vec<String>>>,
}

impl ExportingRunner {
    fn new(output: YamsOutput) -> Self {
        Self {
            output,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl YamsRunner for ExportingRunner {
    fn run(&self, args: &[String]) -> std::io::Result<YamsOutput> {
        self.calls.borrow_mut().push(args.to_vec());
        if args.first().map(String::as_str) == Some("download")
            && let Some(export_index) = args.iter().position(|arg| arg == "--export")
            && let Some(path) = args.get(export_index + 1)
        {
            std::fs::write(path, "downloaded queued paper content")?;
        }
        Ok(self.output.clone())
    }
}

impl SequenceRunner {
    fn new(outputs: Vec<YamsOutput>) -> Self {
        Self {
            outputs: RefCell::new(outputs.into()),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn replace_output(&self, index: usize, output: YamsOutput) {
        self.outputs.borrow_mut()[index] = output;
    }
}

impl YamsRunner for SequenceRunner {
    fn run(&self, args: &[String]) -> std::io::Result<YamsOutput> {
        self.calls.borrow_mut().push(args.to_vec());
        self.outputs
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| std::io::Error::other("no fake yams output queued"))
    }
}
