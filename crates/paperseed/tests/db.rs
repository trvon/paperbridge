use paperseed::app::{
    CorpusPaths, ImportRequest, create_seed_manifest, get_full_text, import_with_yams,
    list_entries, query_with_yams, remove_entry, seed_check, status_summary,
};
use paperseed::db::{CorpusDb, IndexedPaper};
use paperseed::models::{CorpusAction, License, LocalPaper, PaperMetadata, StoredFile};
use paperseed::policy::evaluate;
use paperseed::yams::YamsConfig;
use std::path::PathBuf;

fn import_test_file(
    paths: &CorpusPaths,
    source: std::path::PathBuf,
    title: &str,
    license: &str,
) -> paperseed::Result<paperseed::models::LocalPaper> {
    import_with_yams(
        paths,
        ImportRequest {
            path: source,
            title: Some(title.to_string()),
            license: Some(license.to_string()),
            yams_hash: None,
            extract_full_text: true,
        },
        &YamsConfig::disabled(),
    )
}

fn indexed_paper(id: &str, hash: &str) -> IndexedPaper {
    IndexedPaper {
        paper: LocalPaper {
            metadata: PaperMetadata {
                id: id.to_string(),
                title: format!("Paper {id}"),
                doi: None,
                arxiv_id: None,
                authors: Vec::new(),
                year: None,
                venue: None,
                abstract_note: None,
                license: License::UserOwnedPrivate,
                source_url: None,
            },
            file: StoredFile {
                hash: hash.to_string(),
                path: PathBuf::from(format!("/tmp/{id}.txt")),
                size_bytes: 0,
                mime: "text/plain".to_string(),
            },
        },
        full_text: None,
        full_text_path: None,
        yams_hash: None,
    }
}

#[test]
fn import_persists_file_and_query_index() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "alpha beta beta").unwrap();

    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Alpha Systems", "cc-by").unwrap();

    assert!(paper.file.path.exists());
    assert!(paths.db_path.exists());

    let hits = query_with_yams(&paths, "beta", &YamsConfig::disabled()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, paper.metadata.id);
    assert!(hits[0].score > 0.0);
}

#[test]
fn seed_check_allows_cc_by_imports() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "redistributable").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    let paper = import_test_file(&paths, source, "Redistributable", "cc-by").unwrap();

    assert!(seed_check(&paths, &paper.metadata.id).is_ok());
}

#[test]
fn seed_check_blocks_private_imports() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "private").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    let paper = import_test_file(&paths, source, "Private", "private").unwrap();

    assert!(seed_check(&paths, &paper.metadata.id).is_err());
}

#[test]
fn private_storage_policy_remains_explicit() {
    assert!(evaluate(CorpusAction::StorePrivate, License::UserOwnedPrivate).allowed);
    assert!(!evaluate(CorpusAction::SeedRedistribute, License::UserOwnedPrivate).allowed);
}

#[test]
fn seed_manifest_is_written_for_seedable_papers() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "redistributable").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    let paper = import_test_file(&paths, source, "Redistributable", "cc-by").unwrap();

    let manifest = create_seed_manifest(&paths, &paper.metadata.id).unwrap();
    assert_eq!(manifest.paper_id, paper.metadata.id);
    assert!(
        paths
            .seeds_dir
            .join(format!("{}.json", manifest.paper_id))
            .exists()
    );
}

#[test]
fn load_is_resilient_to_trailing_characters() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "resilient beta gamma").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Resilient Systems", "cc-by").unwrap();

    let raw = std::fs::read_to_string(&paths.db_path).unwrap();
    let corrupted = format!("{raw}\n{{\"extra\":\"trailing garbage\"}}");
    std::fs::write(&paths.db_path, &corrupted).unwrap();

    let db = CorpusDb::load(&paths.db_path).unwrap();
    assert_eq!(db.papers.len(), 1);
    assert_eq!(db.papers[0].paper.metadata.id, paper.metadata.id);
}

#[test]
fn corrupt_database_is_rejected_and_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corpus.json");
    std::fs::write(&db_path, "{ definitely not valid json").unwrap();

    let error = CorpusDb::load(&db_path).unwrap_err();
    assert!(error.to_string().contains("corrupt"));
    assert!(!db_path.exists(), "corrupt database should be quarantined");

    let quarantined = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("corpus.json.bad."));
    assert!(quarantined.is_some(), "quarantine file was not created");
}

#[test]
fn lookup_rejects_empty_and_ambiguous_hash_prefixes() {
    let mut db = CorpusDb::default();
    db.upsert(indexed_paper("paper-a", "abc1111111111111"));
    db.upsert(indexed_paper("paper-b", "abc2222222222222"));

    assert!(db.get("").is_err());
    let error = db.get("abc").unwrap_err();
    let message = error.to_string();
    assert!(message.contains("ambiguous"));
    assert!(message.contains("paper-a"));
    assert!(message.contains("paper-b"));
    assert_eq!(
        db.get("paper-a").unwrap().unwrap().paper.metadata.id,
        "paper-a"
    );
}

#[test]
fn concurrent_imports_do_not_lose_corpus_entries() {
    const IMPORTS: usize = 16;
    let dir = tempfile::tempdir().unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(IMPORTS));

    let handles = (0..IMPORTS)
        .map(|index| {
            let source = dir.path().join(format!("fixture-{index}.txt"));
            std::fs::write(&source, format!("unique concurrent content {index}")).unwrap();
            let paths = paths.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                import_test_file(&paths, source, &format!("Paper {index}"), "cc-by")
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let db = CorpusDb::load(&paths.db_path).unwrap();
    assert_eq!(db.papers.len(), IMPORTS);
}

#[test]
fn save_is_atomic_and_does_not_leave_partial_files() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("corpus.json");
    let db = CorpusDb::default();
    db.save(&db_path).unwrap();
    let raw = std::fs::read_to_string(&db_path).unwrap();
    assert_ne!(raw.trim(), "");

    let tmp = db_path.with_extension("tmp");
    assert!(
        !tmp.exists(),
        "temp file should not remain after atomic save"
    );

    let reloaded = CorpusDb::load(&db_path).unwrap();
    assert_eq!(reloaded.papers.len(), 0);
}

#[test]
fn text_import_extracts_full_text() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "plain text content alpha beta").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Text Test", "cc-by").unwrap();

    let db = CorpusDb::load(&paths.db_path).unwrap();
    let raw_db = std::fs::read_to_string(&paths.db_path).unwrap();
    assert!(
        !raw_db.contains("plain text content alpha beta"),
        "full text must not remain inline in corpus.json"
    );
    assert!(
        db.papers[0]
            .full_text_path
            .as_ref()
            .is_some_and(|path| path.exists())
    );
    let ft = db
        .get(&paper.metadata.id)
        .unwrap()
        .and_then(|entry| entry.read_full_text().unwrap())
        .expect("text full text was not extracted");
    assert!(ft.contains("plain text content"), "fulltext: {ft}");
}

#[test]
fn seed_manifest_rejects_a_tampered_stored_file() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "original redistributable content").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Redistributable", "cc-by").unwrap();

    std::fs::write(&paper.file.path, "tampered content").unwrap();

    let error = create_seed_manifest(&paths, &paper.metadata.id).unwrap_err();
    assert!(error.to_string().contains("integrity"));
}

#[test]
fn corpus_list_show_status_and_remove_are_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "removable content").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Removable", "cc-by").unwrap();

    let status = status_summary(&paths).unwrap();
    assert_eq!(status.papers, 1);
    assert_eq!(status.index_docs, Some(1));
    assert!(status.index_in_sync);
    assert_eq!(list_entries(&paths).unwrap().len(), 1);

    let removed = remove_entry(&paths, &paper.metadata.id).unwrap();
    assert_eq!(removed.paper.metadata.id, paper.metadata.id);
    assert!(!paper.file.path.exists());
    assert!(list_entries(&paths).unwrap().is_empty());
    assert!(status_summary(&paths).unwrap().index_in_sync);
}

#[test]
fn status_reports_a_missing_or_stale_index() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "indexed content").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    import_test_file(&paths, source, "Indexed", "cc-by").unwrap();
    std::fs::remove_file(&paths.index_path).unwrap();

    let status = status_summary(&paths).unwrap();
    assert_eq!(status.papers, 1);
    assert_eq!(status.index_docs, None);
    assert!(!status.index_in_sync);
}

#[test]
fn no_fulltext_import_defers_extraction_until_first_read() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "deferred extraction content").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_with_yams(
        &paths,
        ImportRequest {
            path: source,
            title: Some("Deferred".to_string()),
            license: Some("cc-by".to_string()),
            yams_hash: None,
            extract_full_text: false,
        },
        &YamsConfig::disabled(),
    )
    .unwrap();

    assert!(
        !list_entries(&paths).unwrap()[0].has_full_text(),
        "--no-fulltext must avoid eager extraction"
    );
    let text = get_full_text(&paths, &paper.metadata.id, &YamsConfig::disabled()).unwrap();
    assert!(text.contains("deferred extraction content"));
}
