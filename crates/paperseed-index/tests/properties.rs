//! Property tests for the BM25F index: ranking invariants, normalization
//! invariance, builder/upsert equivalence, and codec robustness.

use paperseed_index::{Index, IndexBuilder, ScoredHit, paperseed_defaults, tokenize};
use proptest::prelude::*;
use std::collections::{BTreeMap, HashSet};
use unicode_normalization::UnicodeNormalization;

const WORDS: &[&str] = &[
    "storage",
    "Chunking",
    "dedup",
    "graph",
    "caf\u{e9}",
    "e\u{fb03}cient",
    "\u{fb01}le",
    "Rust",
    "index",
    "retrieval",
    "na\u{ef}ve",
    "Stra\u{df}e",
    "x",
    "42",
];

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(WORDS), 0..8).prop_map(|words| words.join(" "))
}

#[derive(Debug, Clone)]
struct Doc {
    id: String,
    title: String,
    abstract_note: String,
}

impl Doc {
    fn fields(&self) -> [(&str, &str); 2] {
        [
            ("title", self.title.as_str()),
            ("abstract", self.abstract_note.as_str()),
        ]
    }
}

fn doc() -> impl Strategy<Value = Doc> {
    ("d[0-9]", text(), text()).prop_map(|(id, title, abstract_note)| Doc {
        id,
        title,
        abstract_note,
    })
}

fn build(docs: &[Doc]) -> Index {
    let mut builder = IndexBuilder::new(paperseed_defaults());
    for doc in docs {
        builder.add_document(doc.id.clone(), &doc.fields());
    }
    builder.build()
}

/// Final version of each id, in first-insertion order (matches the index).
fn latest(docs: &[Doc]) -> Vec<Doc> {
    let mut order: Vec<Doc> = Vec::new();
    for doc in docs {
        match order.iter_mut().find(|existing| existing.id == doc.id) {
            Some(existing) => *existing = doc.clone(),
            None => order.push(doc.clone()),
        }
    }
    order
}

fn scores(hits: &[ScoredHit]) -> BTreeMap<String, f32> {
    hits.iter()
        .map(|hit| (hit.doc_id.clone(), hit.score))
        .collect()
}

fn assert_close(a: &[ScoredHit], b: &[ScoredHit]) -> Result<(), TestCaseError> {
    let (a, b) = (scores(a), scores(b));
    prop_assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
    for (id, score) in &a {
        let other = b[id];
        prop_assert!(
            (score - other).abs() <= 1e-4 * score.abs().max(1.0),
            "{id}: {score} vs {other}"
        );
    }
    Ok(())
}

proptest! {
    #[test]
    fn search_results_are_bounded_finite_and_sorted(
        docs in prop::collection::vec(doc(), 0..8),
        query in text(),
        top_k in 0_usize..6,
    ) {
        let index = build(&docs);
        let hits = index.search(&query, top_k);
        prop_assert!(hits.len() <= top_k);
        let query_tokens: HashSet<String> = tokenize(&query).into_iter().collect();
        if query_tokens.is_empty() {
            prop_assert!(hits.is_empty());
        }
        let latest = latest(&docs);
        for hit in &hits {
            prop_assert!(hit.score.is_finite() && hit.score > 0.0);
            let doc = latest.iter().find(|doc| doc.id == hit.doc_id).unwrap();
            let doc_tokens: HashSet<String> = tokenize(&doc.title)
                .into_iter()
                .chain(tokenize(&doc.abstract_note))
                .collect();
            prop_assert!(!doc_tokens.is_disjoint(&query_tokens));
        }
        for pair in hits.windows(2) {
            prop_assert!(
                pair[0].score > pair[1].score
                    || (pair[0].score == pair[1].score && pair[0].doc_id < pair[1].doc_id)
            );
        }
    }

    #[test]
    fn query_normalization_does_not_change_results(
        docs in prop::collection::vec(doc(), 1..8),
        query in text(),
    ) {
        let index = build(&docs);
        let base = index.search(&query, 50);
        prop_assert_eq!(&index.search(&format!("{query} {query}"), 50), &base);
        prop_assert_eq!(&index.search(&query.to_ascii_uppercase(), 50), &base);
        prop_assert_eq!(&index.search(&query.nfd().collect::<String>(), 50), &base);
        prop_assert_eq!(&index.search(&query.nfc().collect::<String>(), 50), &base);
    }

    #[test]
    fn builder_matches_repeated_upsert(docs in prop::collection::vec(doc(), 1..8), query in text()) {
        let built = build(&docs);
        let mut upserted = IndexBuilder::new(paperseed_defaults()).build();
        for doc in &docs {
            upserted.upsert_document(doc.id.clone(), &doc.fields());
        }
        prop_assert_eq!(built.doc_ids().collect::<Vec<_>>(), upserted.doc_ids().collect::<Vec<_>>());
        assert_close(&built.search(&query, 100), &upserted.search(&query, 100))?;
        prop_assert!(upserted.validate().is_ok());
    }

    #[test]
    fn upsert_replacement_matches_fresh_build(
        docs in prop::collection::vec(doc(), 1..8),
        pick in any::<prop::sample::Index>(),
        title in text(),
        query in text(),
    ) {
        let mut index = build(&docs);
        let mut replaced = latest(&docs);
        let target = pick.index(replaced.len());
        replaced[target].title = title;
        index.upsert_document(replaced[target].id.clone(), &replaced[target].fields());
        assert_close(&index.search(&query, 100), &build(&replaced).search(&query, 100))?;
    }

    #[test]
    fn doc_count_counts_distinct_ids_and_index_stays_valid(docs in prop::collection::vec(doc(), 0..12)) {
        let index = build(&docs);
        let distinct: HashSet<&str> = docs.iter().map(|doc| doc.id.as_str()).collect();
        prop_assert_eq!(index.doc_count(), distinct.len());
        prop_assert!(index.validate().is_ok());
    }

    #[test]
    fn bytes_round_trip_preserves_results(docs in prop::collection::vec(doc(), 0..8), query in text()) {
        let index = build(&docs);
        let loaded = Index::from_bytes(&index.to_bytes().unwrap()).unwrap();
        prop_assert_eq!(loaded.search(&query, 100), index.search(&query, 100));
    }

    #[test]
    fn from_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        if let Ok(index) = Index::from_bytes(&bytes) {
            let _ = index.search("storage graph", 5);
        }
    }

    #[test]
    fn from_bytes_with_valid_header_never_panics(
        docs in prop::collection::vec(doc(), 0..4),
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
    ) {
        let mut bytes = build(&docs).to_bytes().unwrap();
        for (position, value) in flips {
            let i = 8 + position.index(bytes.len().saturating_sub(8).max(1));
            if i < bytes.len() {
                bytes[i] = value;
            }
        }
        if let Ok(index) = Index::from_bytes(&bytes) {
            prop_assert!(index.validate().is_ok());
            let _ = index.search("storage graph retrieval", 5);
        }
    }
}
