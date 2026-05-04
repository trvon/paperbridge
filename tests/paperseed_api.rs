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
                authors: vec!["Grace Hopper".to_string()],
                year: Some(2024),
                venue: Some("Systems Journal".to_string()),
                license: Some("cc-by".to_string()),
                source_url: Some("https://example.org/graph".to_string()),
            },
            None,
        )
        .unwrap();

    let hits = api.query_corpus("graph").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, paper.metadata.id);

    let manifest = api.create_seed_manifest(&paper.metadata.id).unwrap();
    assert_eq!(manifest.paper_id, paper.metadata.id);
}
