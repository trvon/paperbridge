use paperseed::corpus::import_local_file;
use paperseed::models::License;
use paperseed::storage::{
    content_addressed_path, copy_and_describe_file, describe_file, hash_file,
};

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
    let path = content_addressed_path("/corpus", "abcdef", Some("pdf")).unwrap();
    assert_eq!(path.to_string_lossy(), "/corpus/ab/abcdef.pdf");
}

#[test]
fn content_addressed_path_rejects_unsafe_hashes() {
    for hash in ["", "a", "../etc", "a\u{e9}b", "ab/cd", "..", "xyz123"] {
        assert!(
            content_addressed_path("/corpus", hash, None).is_err(),
            "{hash:?} must be rejected"
        );
    }
    let path = content_addressed_path("/corpus", "abcd", Some("../../x")).unwrap();
    assert_eq!(path.to_string_lossy(), "/corpus/ab/abcd");
}

#[test]
fn copy_is_idempotent_for_identical_content() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("paper.pdf");
    std::fs::write(&source, b"same bytes").unwrap();
    let files = dir.path().join("files");
    let first = copy_and_describe_file(&source, &files, "application/pdf").unwrap();
    let second = copy_and_describe_file(&source, &files, "application/pdf").unwrap();
    assert_eq!(first, second);
    let leftovers = std::fs::read_dir(&files)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(leftovers, 0);
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn hex_hashes_map_under_root(hash in "[0-9a-f]{2,64}", ext in proptest::option::of("[a-z0-9]{0,5}")) {
            let path = content_addressed_path("/corpus", &hash, ext.as_deref()).unwrap();
            prop_assert!(path.starts_with("/corpus"));
            let parent = path.parent().unwrap();
            prop_assert_eq!(parent.file_name().unwrap().to_str().unwrap(), &hash[..2]);
            prop_assert_eq!(parent.parent().unwrap(), std::path::Path::new("/corpus"));
            prop_assert!(path.file_name().unwrap().to_str().unwrap().starts_with(&hash));
        }

        #[test]
        fn arbitrary_hashes_never_panic_or_escape(hash in "\\PC{0,12}", ext in proptest::option::of("\\PC{0,6}")) {
            if let Ok(path) = content_addressed_path("/corpus", &hash, ext.as_deref()) {
                prop_assert!(path.starts_with("/corpus"));
                prop_assert_eq!(path.components().count(), 4);
            }
        }
    }
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

#[test]
fn copy_hashes_while_streaming_into_content_addressed_storage() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("paper.txt");
    std::fs::write(&source, b"single pass fixture").unwrap();
    let files = dir.path().join("files");

    let stored = copy_and_describe_file(&source, &files, "text/plain").unwrap();

    assert_eq!(stored.hash, hash_file(&source).unwrap());
    assert_eq!(std::fs::read(&stored.path).unwrap(), b"single pass fixture");
    assert!(stored.path.starts_with(files));
}
