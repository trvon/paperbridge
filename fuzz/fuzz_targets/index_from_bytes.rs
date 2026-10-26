#![no_main]
use libfuzzer_sys::fuzz_target;
use paperseed_index::Index;

// Arbitrary index files must be rejected or decode into an index that
// validates and can be searched without panicking.
fuzz_target!(|bytes: &[u8]| {
    if let Ok(index) = Index::from_bytes(bytes) {
        assert!(index.validate().is_ok());
        let _ = index.search("graph neural storage", 10);
    }
});
