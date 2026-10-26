#![no_main]
use libfuzzer_sys::fuzz_target;
use paperbridge::internals::{book_chapter_slice_len, book_toc_chapter_count};

fuzz_target!(|content: &str| {
    let _ = book_toc_chapter_count(content);
    for chapter in [1, 2, 3, 1_000, u32::MAX] {
        if let Some(len) = book_chapter_slice_len(content, chapter) {
            assert!(len <= content.len());
        }
    }
});
