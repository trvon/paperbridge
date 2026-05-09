//! Bridge between `CorpusDb` and the `paperseed-index` BM25F index.
//!
//! Build path: `IndexedPaper` records become indexed documents; persisted
//! to `corpus.idx.json` next to `corpus.json`. Query path: load (or rebuild
//! on miss), search, map results back to `QueryHit`.

use crate::db::{CorpusDb, IndexedPaper, QueryHit};
use paperseed_index::{Index, IndexBuilder, paperseed_defaults};
use std::path::Path;

/// Multiplier used to project BM25F's f32 scores into the existing
/// `usize` `QueryHit::score` field. Preserves ordering and gives a
/// readable integer in CLI output.
const SCORE_SCALE: f32 = 1_000_000.0;

pub fn build_index(db: &CorpusDb) -> Index {
    let mut builder = IndexBuilder::new(paperseed_defaults());
    for entry in &db.papers {
        let authors_blob = entry.paper.metadata.authors.join(" ");
        let venue = entry.paper.metadata.venue.as_deref().unwrap_or("");
        let full_text = entry.full_text.as_deref().unwrap_or("");
        builder.add_document(
            entry.paper.metadata.id.clone(),
            &[
                ("title", entry.paper.metadata.title.as_str()),
                ("authors", authors_blob.as_str()),
                ("venue", venue),
                ("full_text", full_text),
            ],
        );
    }
    builder.build()
}

pub fn persist_index(db: &CorpusDb, path: &Path) -> std::io::Result<()> {
    let index = build_index(db);
    index
        .save(path)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Search the corpus using the persisted index, falling back to an
/// in-memory build when the index is missing or schema-mismatched.
pub fn search(db: &CorpusDb, index_path: &Path, query: &str, top_k: usize) -> Vec<QueryHit> {
    search_scored(db, index_path, query, top_k)
        .into_iter()
        .map(|(hit, _score)| hit)
        .collect()
}

/// Same as [`search`] but also returns the raw BM25F `f32` score per hit. Use
/// when downstream needs the score as an advisory signal.
pub fn search_scored(
    db: &CorpusDb,
    index_path: &Path,
    query: &str,
    top_k: usize,
) -> Vec<(QueryHit, f32)> {
    let index = Index::load(index_path).unwrap_or_else(|_| build_index(db));
    index
        .search(query, top_k)
        .into_iter()
        .filter_map(|hit| {
            let entry = db.get(&hit.doc_id)?;
            Some((query_hit_from(entry, hit.score), hit.score))
        })
        .collect()
}

fn query_hit_from(entry: &IndexedPaper, score: f32) -> QueryHit {
    QueryHit {
        id: entry.paper.metadata.id.clone(),
        title: entry.paper.metadata.title.clone(),
        score: (score * SCORE_SCALE).round().max(0.0) as usize,
        path: entry.paper.file.path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{License, LocalPaper, PaperMetadata, StoredFile};
    use std::path::PathBuf;

    fn fixture(id: &str, title: &str, full_text: Option<&str>) -> IndexedPaper {
        IndexedPaper {
            paper: LocalPaper {
                metadata: PaperMetadata {
                    id: id.to_string(),
                    title: title.to_string(),
                    authors: vec![],
                    year: None,
                    doi: None,
                    license: License::UserOwnedPrivate,
                    venue: None,
                    source_url: None,
                },
                file: StoredFile {
                    path: PathBuf::from(format!("/tmp/{id}.pdf")),
                    hash: format!("hash-{id}"),
                    size_bytes: 0,
                    mime: "application/pdf".to_string(),
                },
            },
            full_text: full_text.map(|s| s.to_string()),
            yams_hash: None,
        }
    }

    #[test]
    fn search_ranks_topical_doc_above_unrelated_doc() {
        let mut db = CorpusDb::default();
        db.upsert(fixture(
            "cdc",
            "Content-Defined Chunking for Deduplication",
            Some("Rabin fingerprinting variable-size blocks"),
        ));
        db.upsert(fixture(
            "llm",
            "Survey of Large Language Models",
            Some("Transformer architectures for text generation"),
        ));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.idx.json");
        persist_index(&db, &path).unwrap();

        let hits = search(&db, &path, "content-defined chunking deduplication", 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, "cdc");
    }

    #[test]
    fn search_falls_back_to_in_memory_when_index_missing() {
        let mut db = CorpusDb::default();
        db.upsert(fixture("a", "alpha topic paper", None));
        db.upsert(fixture("b", "unrelated material", None));

        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.idx.bin");

        let hits = search(&db, &missing, "alpha", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }
}
