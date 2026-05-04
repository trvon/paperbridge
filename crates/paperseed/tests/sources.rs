use paperseed::app::{CorpusPaths, IngestRequest, export_bibtex, fetch_open_file, ingest, status};
use paperseed::sources::{fetch_plan, legal_sources, metadata_from_paperbridge_json};

#[test]
fn legal_sources_include_open_access_resolvers() {
    let ids: Vec<&str> = legal_sources()
        .into_iter()
        .map(|source| source.id)
        .collect();
    assert!(ids.contains(&"openalex"));
    assert!(ids.contains(&"unpaywall"));
    assert!(ids.contains(&"arxiv"));
    assert!(ids.contains(&"user-import"));
}

#[test]
fn fetch_plan_is_policy_first() {
    let plan = fetch_plan("10.1234/example", Some("unpaywall".to_string()));
    assert_eq!(plan.doi, "10.1234/example");
    assert_eq!(plan.source.as_deref(), Some("unpaywall"));
    assert!(plan.policy.contains("open-access"));
    assert!(plan.allowed_sources.contains(&"unpaywall"));
}

#[test]
fn parses_paperbridge_zotero_shape() {
    let raw = r#"{
        "key": "ITEMA",
        "data": {
            "title": "Graph Learning at Scale",
            "DOI": "10.5555/graph",
            "date": "2024-08-01",
            "publicationTitle": "Systems Journal",
            "creators": [{"firstName": "Grace", "lastName": "Hopper"}],
            "url": "https://example.org/graph",
            "rights": "cc-by"
        }
    }"#;

    let metadata = metadata_from_paperbridge_json(raw).unwrap();
    assert_eq!(metadata.title.as_deref(), Some("Graph Learning at Scale"));
    assert_eq!(metadata.doi.as_deref(), Some("10.5555/graph"));
    assert_eq!(metadata.year, Some(2024));
    assert_eq!(metadata.authors, vec!["Grace Hopper"]);
    assert_eq!(metadata.license.as_deref(), Some("cc-by"));
}

#[test]
fn ingest_applies_paperbridge_metadata_and_exports_bibtex() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "graph learning text").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let metadata = metadata_from_paperbridge_json(
        r#"{
            "title": "Graph Learning at Scale",
            "doi": "10.5555/graph",
            "authors": ["Grace Hopper"],
            "year": 2024,
            "venue": "Systems Journal",
            "license": "cc-by"
        }"#,
    )
    .unwrap();

    let paper = ingest(
        &paths,
        IngestRequest {
            path: source,
            metadata,
            license: None,
            yams_hash: None,
        },
    )
    .unwrap();

    assert_eq!(paper.metadata.doi.as_deref(), Some("10.5555/graph"));
    assert_eq!(paper.metadata.authors, vec!["Grace Hopper"]);

    let bibtex = export_bibtex(&status(&paths).unwrap());
    assert!(bibtex.contains("Graph Learning at Scale"));
    assert!(bibtex.contains("Grace Hopper"));
    assert!(bibtex.contains("10.5555/graph"));
}

#[test]
fn fetch_open_file_requires_known_open_license() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.txt");
    std::fs::write(&source, "open text").unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));

    let blocked = fetch_open_file(
        &paths,
        "10.1234/example".to_string(),
        source.clone(),
        None,
        None,
    );
    assert!(blocked.is_err());

    let paper = fetch_open_file(
        &paths,
        "10.1234/example".to_string(),
        source,
        Some("Open Example".to_string()),
        Some("cc-by".to_string()),
    )
    .unwrap();
    assert_eq!(paper.metadata.doi.as_deref(), Some("10.1234/example"));
}
