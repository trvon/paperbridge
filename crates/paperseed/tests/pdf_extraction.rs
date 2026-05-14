//! Real-world PDF extraction regression tests.
//!
//! Fixtures live in `tests/fixtures/`. Each test imports a vendored arXiv
//! preprint through `paperseed::app::import_with_yams` (the same path the
//! ingest CLI/MCP tools use) and asserts that the indexed `full_text`
//! contains a specific phrase from the paper.
//!
//! Tests marked `#[ignore]` capture failure modes of the legacy hand-rolled
//! extractor in `paperseed::app::extract_text_from_pdf_bytes`. They flip to
//! `passing` once that extractor is replaced with `pdf-extract`.

use paperseed::app::{CorpusPaths, ImportRequest, import_with_yams};
use paperseed::db::CorpusDb;
use paperseed::yams::YamsConfig;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn import_and_get_text(fixture: &str, title: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let paper = import_with_yams(
        &paths,
        ImportRequest {
            path: fixture_path(fixture),
            title: Some(title.to_string()),
            license: Some("cc-by".to_string()),
            yams_hash: None,
        },
        &YamsConfig::disabled(),
    )
    .expect("import");
    let db = CorpusDb::load(&paths.db_path).expect("load corpus");
    db.get(&paper.metadata.id)
        .and_then(|entry| entry.full_text.clone())
        .unwrap_or_default()
}

/// The legacy extractor inserts a literal space between every glyph on
/// subset-font PDFs ("G le n c o r a B o r r a d a ile" instead of
/// "Glencora Borradaile"). pdf-extract recovers the actual string by
/// honoring the ToUnicode CMap.
#[test]
fn arxiv_1408_extracts_author_and_abstract() {
    let text = import_and_get_text(
        "arxiv_1408_5939_planar_subgraphs.pdf",
        "Planar Induced Subgraphs of Sparse Graphs",
    );
    assert!(!text.is_empty(), "no text extracted");
    assert!(
        text.contains("Glencora Borradaile"),
        "expected author name in extracted text; first 400 chars: {:?}",
        text.chars().take(400).collect::<String>()
    );
    assert!(
        text.contains("induced pseudoforest"),
        "expected key abstract phrase; first 400 chars: {:?}",
        text.chars().take(400).collect::<String>()
    );
}

/// Vietnamese author name + math fonts. The legacy extractor returns the
/// usual letter-spaced garbage ("V u T r u n g H i e u"). pdf-extract
/// returns the real name.
#[test]
fn arxiv_1808_extracts_author_and_topic() {
    let text = import_and_get_text(
        "arxiv_1808_06100_polynomial_optimization.pdf",
        "On the solution existence and stability of polynomial optimization problems",
    );
    assert!(!text.is_empty(), "no text extracted");
    assert!(
        text.contains("Vu Trung Hieu"),
        "expected author name; first 400 chars: {:?}",
        text.chars().take(400).collect::<String>()
    );
    assert!(
        text.contains("polynomial optimization"),
        "expected topic phrase; first 400 chars: {:?}",
        text.chars().take(400).collect::<String>()
    );
}
