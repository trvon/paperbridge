//! Bridge between `CorpusDb` and the `paperseed-index` BM25F index.
//!
//! Build path: `IndexedPaper` records become indexed documents; persisted
//! to `corpus.idx.bin` next to `corpus.json`. Query path: load (or rebuild
//! on miss), search, map results back to `QueryHit`.

use crate::db::{CorpusDb, IndexedPaper, QueryHit};
use crate::error::Result;
use paperseed_index::{Index, IndexBuilder, paperseed_defaults};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Identity of an index file on disk; a change in any field invalidates the
/// in-process cache.
#[derive(Clone, PartialEq, Eq)]
struct IndexFileStamp {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

impl IndexFileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            modified: meta.modified().ok()?,
            len: meta.len(),
        })
    }
}

type CachedIndex = (IndexFileStamp, Arc<Index>);

fn index_cache() -> &'static Mutex<Option<CachedIndex>> {
    static CACHE: OnceLock<Mutex<Option<CachedIndex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Load the persisted index, reusing the last decoded copy while the file is
/// unchanged. Long-running hosts (the MCP server) otherwise decode the whole
/// index on every query.
fn load_index_cached(path: &Path) -> paperseed_index::Result<Arc<Index>> {
    let stamp = IndexFileStamp::of(path);
    if let Some(stamp) = &stamp
        && let Ok(cache) = index_cache().lock()
        && let Some((cached_stamp, index)) = cache.as_ref()
        && cached_stamp == stamp
    {
        return Ok(Arc::clone(index));
    }
    let index = Arc::new(Index::load(path)?);
    if let Some(stamp) = stamp
        && let Ok(mut cache) = index_cache().lock()
    {
        *cache = Some((stamp, Arc::clone(&index)));
    }
    Ok(index)
}

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
        Ok(index) if index_matches_corpus(&index, db, Some(paper_id)) => index,
        _ => build_index(db).map_err(|error| std::io::Error::other(error.to_string()))?,
    };
    upsert_index_entry(&mut index, entry)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    index
        .save(path)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// True when the persisted index holds exactly the corpus ids, ignoring
/// `pending_id`, which the caller is about to upsert. Comparing counts alone
/// accepts an index where one paper was swapped for another.
fn index_matches_corpus(index: &Index, db: &CorpusDb, pending_id: Option<&str>) -> bool {
    let keep = |id: &&str| Some(*id) != pending_id;
    let corpus: HashSet<&str> = db
        .papers
        .iter()
        .map(|entry| entry.paper.metadata.id.as_str())
        .filter(keep)
        .collect();
    let indexed: HashSet<&str> = index.doc_ids().filter(keep).collect();
    corpus == indexed
}

/// Document count of the persisted index and whether it holds exactly the
/// corpus ids. `None` when the index is missing or unreadable.
pub fn persisted_index_status(db: &CorpusDb, path: &Path) -> Option<(usize, bool)> {
    let index = Index::load(path).ok()?;
    Some((index.doc_count(), index_matches_corpus(&index, db, None)))
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
    let index = match load_index_cached(index_path) {
        Ok(index) => index,
        Err(_) => {
            let index = build_index(db)?;
            index
                .save(index_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            Arc::new(index)
        }
    };
    // First entry wins for a repeated id, matching `CorpusDb` lookup order.
    let mut by_id: HashMap<&str, &IndexedPaper> = HashMap::with_capacity(db.papers.len());
    for entry in &db.papers {
        by_id
            .entry(entry.paper.metadata.id.as_str())
            .or_insert(entry);
    }
    Ok(index
        .search(query, top_k)
        .into_iter()
        .filter_map(|hit| {
            let entry = by_id.get(hit.doc_id.as_str())?;
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
                    isbn: None,
                    work_type: None,
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

    fn indexed_ids(path: &Path) -> std::collections::BTreeSet<String> {
        Index::load(path)
            .unwrap()
            .doc_ids()
            .map(str::to_string)
            .collect()
    }

    fn corpus_ids(db: &CorpusDb) -> std::collections::BTreeSet<String> {
        db.papers
            .iter()
            .map(|entry| entry.paper.metadata.id.clone())
            .collect()
    }

    #[test]
    fn upsert_rebuilds_when_a_paper_was_swapped_without_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.idx.bin");
        let mut db = CorpusDb::default();
        db.upsert(fixture("a", "alpha", None));
        db.upsert(fixture("c", "gamma", None));
        persist_index(&db, &path).unwrap();

        // Same count, different membership: the old count check accepted this.
        db.papers.retain(|entry| entry.paper.metadata.id != "a");
        db.upsert(fixture("b", "beta", None));
        persist_upsert(&db, &path, "c").unwrap();

        assert_eq!(indexed_ids(&path), corpus_ids(&db));
        assert!(search(&db, &path, "alpha", 5).unwrap().is_empty());
        assert_eq!(search(&db, &path, "beta", 5).unwrap()[0].id, "b");
    }

    #[test]
    fn index_status_detects_membership_drift() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.idx.bin");
        let mut db = CorpusDb::default();
        db.upsert(fixture("a", "alpha", None));
        persist_index(&db, &path).unwrap();
        assert_eq!(persisted_index_status(&db, &path), Some((1, true)));

        db.papers.clear();
        db.upsert(fixture("b", "beta", None));
        assert_eq!(persisted_index_status(&db, &path), Some((1, false)));
    }

    #[derive(Debug, Clone)]
    enum Op {
        /// Upsert paper `n`, persisting via `persist_upsert` when `persist`.
        Upsert {
            n: u8,
            version: u8,
            persist: bool,
        },
        /// Remove paper at position `n % len`, rebuilding when `persist`.
        Remove {
            n: u8,
            persist: bool,
        },
        Reindex,
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            (0_u8..6, 0_u8..3, any::<bool>()).prop_map(|(n, version, persist)| Op::Upsert {
                n,
                version,
                persist
            }),
            (any::<u8>(), any::<bool>()).prop_map(|(n, persist)| Op::Remove { n, persist }),
            Just(Op::Reindex),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// Model test: after any persisting operation the index holds exactly
        /// the corpus ids, even if earlier operations skipped index updates.
        #[test]
        fn persisted_index_tracks_corpus_membership(ops in proptest::collection::vec(op(), 1..16)) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("corpus.idx.bin");
            let mut db = CorpusDb::default();
            for op in ops {
                let persisted = match op {
                    Op::Upsert { n, version, persist } => {
                        let id = format!("p{n}");
                        db.upsert(fixture(&id, &format!("topic{n} rev{version}"), None));
                        if persist {
                            persist_upsert(&db, &path, &id).unwrap();
                        }
                        persist
                    }
                    Op::Remove { n, persist } => {
                        if !db.papers.is_empty() {
                            let index = usize::from(n) % db.papers.len();
                            db.papers.remove(index);
                        }
                        if persist {
                            persist_index(&db, &path).unwrap();
                        }
                        persist
                    }
                    Op::Reindex => {
                        persist_index(&db, &path).unwrap();
                        true
                    }
                };
                if persisted {
                    proptest::prop_assert_eq!(indexed_ids(&path), corpus_ids(&db));
                    proptest::prop_assert_eq!(persisted_index_status(&db, &path).map(|s| s.1), Some(true));
                }
                if path.exists() {
                    for hit in search(&db, &path, "topic0 topic1 topic2 topic3 topic4 topic5", 10).unwrap() {
                        proptest::prop_assert!(corpus_ids(&db).contains(&hit.id));
                    }
                }
            }
        }
    }
}
