use paperseed::app::{CorpusPaths, ImportRequest, get_entry, import_with_yams, set_full_text};
use paperseed::yams::YamsConfig;

#[test]
fn set_full_text_replaces_text_and_keeps_file_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let paths = CorpusPaths::new(dir.path().join("corpus"));
    let source = dir.path().join("paper.txt");
    std::fs::write(&source, "thin text").unwrap();
    let paper = import_with_yams(
        &paths,
        ImportRequest {
            path: source,
            title: Some("Kept Title".to_string()),
            license: None,
            yams_hash: None,
            extract_full_text: true,
        },
        &YamsConfig::disabled(),
    )
    .unwrap();
    let id = paper.metadata.id.clone();
    assert_eq!(
        get_entry(&paths, &id)
            .unwrap()
            .read_full_text()
            .unwrap()
            .as_deref(),
        Some("thin text")
    );

    set_full_text(&paths, &id, "recovered full text of the paper".to_string()).unwrap();

    let entry = get_entry(&paths, &id).unwrap();
    assert_eq!(
        entry.read_full_text().unwrap().as_deref(),
        Some("recovered full text of the paper")
    );
    assert_eq!(entry.paper, paper, "file and metadata are untouched");
    assert_eq!(
        std::fs::read_to_string(&entry.paper.file.path).unwrap(),
        "thin text"
    );
    // The search index follows the new text.
    let hits =
        paperseed::app::query_with_yams(&paths, "recovered", &YamsConfig::disabled()).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);

    assert!(set_full_text(&paths, "ffffffffffff", "x".to_string()).is_err());
}
