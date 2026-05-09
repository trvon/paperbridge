use paperseed::app::{
    CorpusPaths, ImportRequest, create_seed_manifest, import_with_yams, query_with_yams, seed_check,
};
use paperseed::db::CorpusDb;
use paperseed::models::{CorpusAction, License};
use paperseed::policy::evaluate;
use paperseed::yams::YamsConfig;

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
        },
        &YamsConfig::disabled(),
    )
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
    // BM25F scores are scaled f32→usize; the exact value is implementation-defined,
    // but a successful match must produce a positive score.
    assert!(hits[0].score > 0);
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
fn pdf_import_extracts_full_text_uncompressed() {
    let dir = tempfile::tempdir().unwrap();
    let pdf_path = dir.path().join("test.pdf");
    // Minimal uncompressed PDF with one text stream
    std::fs::write(
        &pdf_path,
        b"%PDF-1.0\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Contents 4 0 R/Parent 2 0 R>>endobj\n\
4 0 obj<</Length 44>>stream\n\
BT /F1 12 Tf 100 700 Td (Hello World Test) Tj ET\n\
endstream\n\
endobj\n\
xref\n\
0 5\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
0000000190 00000 n \n\
trailer<</Size 5/Root 1 0 R>>\n\
startxref\n\
279\n\
%%EOF\n",
    )
    .unwrap();

    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, pdf_path, "PDF Test", "cc-by").unwrap();

    let db = CorpusDb::load(&paths.db_path).unwrap();
    let ft = db
        .get(&paper.metadata.id)
        .and_then(|e| e.full_text.as_deref())
        .expect("pdf full text was not extracted");
    assert!(ft.contains("Hello World Test"), "fulltext: {ft}");
}

#[test]
fn text_import_extracts_full_text() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "plain text content alpha beta").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_test_file(&paths, source, "Text Test", "cc-by").unwrap();

    let db = CorpusDb::load(&paths.db_path).unwrap();
    let ft = db
        .get(&paper.metadata.id)
        .and_then(|e| e.full_text.as_deref())
        .expect("text full text was not extracted");
    assert!(ft.contains("plain text content"), "fulltext: {ft}");
}
