//! Unstable entry points for benchmarks and fuzz targets.
//!
//! Not part of the public API: names and signatures may change in any
//! release. Wrappers return plain types so crate-private models stay private.

use crate::models::PaperHit;

pub fn split_technical_content(
    input: &str,
    max_chars: usize,
    breadcrumb: Option<&str>,
) -> Vec<String> {
    crate::chunking::split_technical_content(input, max_chars, breadcrumb)
}

/// Number of chapters detected by the book TOC extractor.
pub fn book_toc_chapter_count(content: &str) -> usize {
    crate::book::extract_toc_from_text("fuzz", None, content)
        .chapters
        .len()
}

/// Byte length of the extracted chapter slice, if the chapter is found.
pub fn book_chapter_slice_len(content: &str, chapter: u32) -> Option<usize> {
    crate::book::extract_chapter_slice(content, chapter).map(|(slice, _)| slice.len())
}

pub fn normalize_doi(raw: &str) -> Option<String> {
    crate::hit_enrich::normalize_doi_str(raw)
}

pub fn normalize_arxiv(raw: &str) -> Option<String> {
    crate::hit_enrich::normalize_arxiv_str(raw)
}

pub fn normalize_isbn(raw: &str) -> Option<String> {
    crate::book::isbn::normalize_isbn(raw)
}

pub fn dedupe_hits(hits: Vec<PaperHit>) -> Vec<PaperHit> {
    crate::external::dedupe(hits)
}

/// Number of sections the heading-based fallback builds from plain text.
pub fn fallback_section_count(abstract_note: Option<&str>, content: &str) -> usize {
    crate::paper::fallback::build_sections(abstract_note, content).len()
}
