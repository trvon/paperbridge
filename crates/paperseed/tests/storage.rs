use paperseed::corpus::import_local_file;
use paperseed::models::License;
use paperseed::storage::{content_addressed_path, describe_file, hash_file};

#[test]
fn hash_and_describe_file_are_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.txt");
    std::fs::write(&path, b"paperseed fixture").unwrap();

    let hash = hash_file(&path).unwrap();
    let described = describe_file(&path, "text/plain").unwrap();

    assert_eq!(hash, described.hash);
    assert_eq!(described.size_bytes, 17);
    assert_eq!(described.mime, "text/plain");
}

#[test]
fn content_addressed_paths_use_hash_prefix() {
    let path = content_addressed_path("/corpus", "abcdef", Some("pdf"));
    assert_eq!(path.to_string_lossy(), "/corpus/ab/abcdef.pdf");
}

#[test]
fn import_local_file_builds_local_paper_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.txt");
    std::fs::write(&path, b"owned paper").unwrap();

    let paper = import_local_file(
        &path,
        "Owned Paper",
        License::UserOwnedPrivate,
        "text/plain",
    )
    .expect("private user-owned import should be allowed");

    assert_eq!(paper.metadata.title, "Owned Paper");
    assert_eq!(paper.metadata.license, License::UserOwnedPrivate);
    assert_eq!(paper.file.size_bytes, 11);
}
