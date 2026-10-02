use paperbridge::paperseed_api::PaperseedApi;
use paperseed::sources::PaperbridgeMetadata;

#[test]
fn paperseed_api_import_query_and_seed_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("paper.txt");
    std::fs::write(&source, "graph graph learning").unwrap();

    let api = PaperseedApi::with_yams(
        dir.path().join("corpus"),
        None,
        paperseed::yams::YamsConfig::disabled(),
    );
    let paper = api
        .ingest_with_metadata(
            &source,
            PaperbridgeMetadata {
                title: Some("Graph Learning".to_string()),
                doi: Some("10.5555/graph".to_string()),
                arxiv_id: Some("2401.01234".to_string()),
                isbn: None,
                work_type: None,
                authors: vec!["Grace Hopper".to_string()],
                year: Some(2024),
                venue: Some("Systems Journal".to_string()),
                abstract_note: Some("A real scholarly abstract.".to_string()),
                license: Some("cc-by".to_string()),
                source_url: Some("https://example.org/graph".to_string()),
            },
            None,
        )
        .unwrap();

    let hits = api.query_corpus("graph").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, paper.metadata.id);

    let cached = api.search_cached_papers("scholarly", 1).unwrap();
    assert_eq!(cached[0].arxiv_id.as_deref(), Some("2401.01234"));
    assert_eq!(
        cached[0].abstract_note.as_deref(),
        Some("A real scholarly abstract.")
    );

    let manifest = api.create_seed_manifest(&paper.metadata.id).unwrap();
    assert_eq!(manifest.paper_id, paper.metadata.id);
}

#[test]
fn paperseed_api_isbn_identity_and_work_type() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("book.txt");
    std::fs::write(&source, "Chapter 1: Systems Principles").unwrap();

    let api = PaperseedApi::with_yams(
        dir.path().join("corpus"),
        None,
        paperseed::yams::YamsConfig::disabled(),
    );
    let book = api
        .ingest_with_metadata(
            &source,
            PaperbridgeMetadata {
                title: Some("Designing Data-Intensive Applications".to_string()),
                doi: None,
                arxiv_id: None,
                isbn: Some("978-1-4493-7332-0".to_string()),
                work_type: Some("book".to_string()),
                authors: vec!["Martin Kleppmann".to_string()],
                year: Some(2017),
                venue: Some("O'Reilly".to_string()),
                abstract_note: None,
                license: Some("cc-by".to_string()),
                source_url: None,
            },
            None,
        )
        .unwrap();

    // 1. Identity lookup with unhyphenated target ISBN
    let found = api.find_cached_identity(None, None, None, Some("9781449373320"));
    assert!(found.is_some());
    assert_eq!(found.unwrap().paper.metadata.id, book.metadata.id);

    // 2. Identity lookup with hyphenated target ISBN
    let found_hyphen = api.find_cached_identity(None, None, None, Some("978-1-4493-7332-0"));
    assert!(found_hyphen.is_some());
    assert_eq!(found_hyphen.unwrap().paper.metadata.id, book.metadata.id);

    // 3. Cached search parses WorkType correctly
    let cached = api.search_cached_papers("Systems", 1).unwrap();
    assert_eq!(cached.len(), 1);
    assert_eq!(
        cached[0].work_type,
        Some(paperbridge::models::WorkType::Book)
    );
    assert_eq!(cached[0].isbn.as_deref(), Some("978-1-4493-7332-0"));
}
