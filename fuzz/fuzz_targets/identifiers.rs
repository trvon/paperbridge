#![no_main]
use libfuzzer_sys::fuzz_target;
use paperbridge::internals::{normalize_arxiv, normalize_doi, normalize_isbn};

// Normalizers must never panic and must be idempotent on their own output.
fuzz_target!(|raw: &str| {
    for normalize in [normalize_doi, normalize_arxiv, normalize_isbn] {
        if let Some(canonical) = normalize(raw) {
            assert_eq!(normalize(&canonical).as_deref(), Some(canonical.as_str()), "{raw:?}");
        }
    }
});
