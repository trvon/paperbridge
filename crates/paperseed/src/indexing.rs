//! Bridge between `CorpusDb` and the `paperseed-index` BM25F index.
//!
//! Build path: `IndexedPaper` records become indexed documents; persisted
//! to `corpus.idx.bin` next to `corpus.json`. Query path: load (or rebuild
//! on miss), search, map results back to `QueryHit`.

use crate::db::{CorpusDb, IndexedPaper, QueryHit};
use crate::error::Result;
use paperseed_index::{Index, IndexBuilder, paperseed_defaults};
use std::path::Path;

pub fn build_index(db: &CorpusDb) -> Result<Index> {
    let mut builder = IndexBuilder::new(paperseed_defaults());
    for entry in &db.papers {
        add_to_builder(&mut builder, entry)?;
    }
    Ok(builder.build())
}

pub fn persist_index(db: &CorpusDb, path: &Path) -> std::io::Result<()> {
    let index = build_index(db).map_err(|error| std::io::Error::other(error.to_string()))?;
    index
        .save(path)
        .map_err(|e| std::io::Error::other(e.to_string()))
}

pub fn persist_upsert(db: &CorpusDb, path: &Path, paper_id: &str) -> std::io::Result<()> {
    let entry = db
        .papers
        .iter()
        .find(|entry| entry.paper.metadata.id == paper_id)
        .ok_or_else(|| std::io::Error::other(format!("paper missing from corpus: {paper_id}")))?;
    let mut index = match Index::load(path) {
        Ok(index) => {
            let already_present = index.contains_document(paper_id);
            let expected_count = if already_present {
                db.papers.len()
            } else {
                db.papers.len().saturating_sub(1)
            };
            if index.doc_count() == expected_count {
                index
            } else {
                build_index(db).map_err(|error| std::io::Error::other(error.to_string()))?
            }
        }
        Err(_) => build_index(db).map_err(|error| std::io::Error::other(error.to_string()))?,
    };
    upsert_index_entry(&mut index, entry)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    index
        .save(path)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

pub fn persisted_doc_count(path: &Path) -> Option<usize> {
    Index::load(path).ok().map(|index| index.doc_count())
}

/// Search the corpus using the persisted index, falling back to an
/// in-memory build when the index is missing or schema-mismatched.
pub fn search(
    db: &CorpusDb,
    index_path: &Path,
    query: &str,
    top_k: usize,
) -> Result<Vec<QueryHit>> {
    Ok(search_scored(db, index_path, query, top_k)?
        .into_iter()
        .map(|(hit, _score)| hit)
        .collect())
}

/// Same as [`search`] but also returns the raw BM25F `f32` score per hit. Use
/// when downstream needs the score as an advisory signal.
pub fn search_scored(
    db: &CorpusDb,
    index_path: &Path,
    query: &str,
    top_k: usize,
) -> Result<Vec<(QueryHit, f32)>> {
    let index = match Index::load(index_path) {
        Ok(index) => index,
        Err(_) => {
            let index = build_index(db)?;
            index
                .save(index_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            index
        }
    };
    Ok(index
        .search(query, top_k)
        .into_iter()
        .filter_map(|hit| {
            let entry = db
                .papers
                .iter()
                .find(|entry| entry.paper.metadata.id == hit.doc_id)?;
            Some((query_hit_from(entry, hit.score), hit.score))
        })
        .collect())
}

fn add_to_builder(builder: &mut IndexBuilder, entry: &IndexedPaper) -> Result<()> {
    let authors = entry.paper.metadata.authors.join(" ");
    let venue = entry.paper.metadata.venue.as_deref().unwrap_or("");
    let abstract_note = entry.paper.metadata.abstract_note.as_deref().unwrap_or("");
    let full_text = entry.read_full_text()?.unwrap_or_default();
    builder.add_document(
        entry.paper.metadata.id.clone(),
        &[
            ("title", entry.paper.metadata.title.as_str()),
            ("authors", authors.as_str()),
            ("venue", venue),
            ("abstract", abstract_note),
            ("full_text", full_text.as_str()),
        ],
    );
    Ok(())
}

fn upsert_index_entry(index: &mut Index, entry: &IndexedPaper) -> Result<()> {
    let authors = entry.paper.metadata.authors.join(" ");
    let venue = entry.paper.metadata.venue.as_deref().unwrap_or("");
    let abstract_note = entry.paper.metadata.abstract_note.as_deref().unwrap_or("");
    let full_text = entry.read_full_text()?.unwrap_or_default();
    index.upsert_document(
        entry.paper.metadata.id.clone(),
        &[
            ("title", entry.paper.metadata.title.as_str()),
            ("authors", authors.as_str()),
            ("venue", venue),
            ("abstract", abstract_note),
            ("full_text", full_text.as_str()),
        ],
    );
    Ok(())
}

fn query_hit_from(entry: &IndexedPaper, score: f32) -> QueryHit {
    QueryHit {
        id: entry.paper.metadata.id.clone(),
        title: entry.paper.metadata.title.clone(),
        score,
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
                    arxiv_id: None,
                    license: License::UserOwnedPrivate,
                    venue: None,
                    abstract_note: None,
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
            full_text_path: None,
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
        let path = dir.path().join("corpus.idx.bin");
        persist_index(&db, &path).unwrap();

        let hits = search(&db, &path, "content-defined chunking deduplication", 10).unwrap();
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

        let hits = search(&db, &missing, "alpha", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
        assert!(missing.exists(), "fallback rebuild should be persisted");
    }
}
