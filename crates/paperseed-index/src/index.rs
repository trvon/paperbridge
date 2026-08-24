use crate::error::{IndexError, Result};
use crate::params::BuildOptions;
use crate::tokenizer::tokenize;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const SCHEMA_VERSION: u32 = 2;

/// Per-document field tf for a single posting.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldTerm {
    field: u8,
    tf: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Posting {
    doc: u32,
    fields: Vec<FieldTerm>,
}

/// Persistent BM25F index. Built once, queried many times.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    schema_version: u32,
    options: BuildOptions,
    /// External document ids in insertion order; internal `u32` ids index
    /// into this vec.
    doc_ids: Vec<String>,
    /// `field_lens[doc_idx][field_id] = token count`.
    field_lens: Vec<Vec<u32>>,
    /// Average length per field across the corpus, computed at build time.
    avg_field_len: Vec<f32>,
    /// `term -> postings`. Postings are sorted by `doc` for determinism.
    postings: HashMap<String, Vec<Posting>>,
}

impl Index {
    pub fn doc_count(&self) -> usize {
        self.doc_ids.len()
    }

    pub fn options(&self) -> &BuildOptions {
        &self.options
    }

    pub fn contains_document(&self, doc_id: &str) -> bool {
        self.doc_ids.iter().any(|candidate| candidate == doc_id)
    }

    pub fn upsert_document(&mut self, doc_id: impl Into<String>, fields: &[(&str, &str)]) {
        let doc_id = doc_id.into();
        let doc_idx = if let Some(index) = self.doc_ids.iter().position(|id| id == &doc_id) {
            let doc = index as u32;
            self.postings.retain(|_, postings| {
                postings.retain(|posting| posting.doc != doc);
                !postings.is_empty()
            });
            index
        } else {
            let index = self.doc_ids.len();
            self.doc_ids.push(doc_id);
            self.field_lens.push(vec![0; self.options.field_count()]);
            index
        };

        let mut lens = vec![0_u32; self.options.field_count()];
        let mut terms: HashMap<String, HashMap<u8, u32>> = HashMap::new();
        for (name, text) in fields {
            let Some(field) = self.options.field_id(name) else {
                continue;
            };
            let tokens = tokenize(text);
            lens[field as usize] = tokens.len() as u32;
            for token in tokens {
                *terms.entry(token).or_default().entry(field).or_insert(0) += 1;
            }
        }
        self.field_lens[doc_idx] = lens;
        for (term, by_field) in terms {
            let mut fields = by_field
                .into_iter()
                .map(|(field, tf)| FieldTerm { field, tf })
                .collect::<Vec<_>>();
            fields.sort_by_key(|field| field.field);
            let postings = self.postings.entry(term).or_default();
            postings.push(Posting {
                doc: doc_idx as u32,
                fields,
            });
            postings.sort_by_key(|posting| posting.doc);
        }
        self.recalculate_average_field_lengths();
    }

    fn recalculate_average_field_lengths(&mut self) {
        let mut totals = vec![0_u64; self.options.field_count()];
        for lens in &self.field_lens {
            for (field, len) in lens.iter().enumerate() {
                totals[field] += u64::from(*len);
            }
        }
        self.avg_field_len = totals
            .into_iter()
            .map(|total| {
                if self.doc_ids.is_empty() {
                    0.0
                } else {
                    (total as f64 / self.doc_ids.len() as f64) as f32
                }
            })
            .collect();
    }

    /// Score the corpus against `query` and return the top-K hits in
    /// descending score order. Hits with a score of 0 are dropped.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<ScoredHit> {
        if top_k == 0 || self.doc_ids.is_empty() {
            return Vec::new();
        }

        let terms = tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }

        let n = self.doc_ids.len() as f32;
        let mut accum: HashMap<u32, f32> = HashMap::new();

        let mut seen_terms: HashMap<&str, ()> = HashMap::new();
        for term in &terms {
            // De-duplicate query terms — IDF is per-term, not per-occurrence.
            if seen_terms.insert(term.as_str(), ()).is_some() {
                continue;
            }
            let Some(plist) = self.postings.get(term) else {
                continue;
            };
            let df = plist.len() as f32;
            // BM25 IDF (Lucene-style, always non-negative).
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();

            for posting in plist {
                let tf_d = self.weighted_tf(posting);
                if tf_d <= 0.0 {
                    continue;
                }
                let contribution = idf * (tf_d / (self.options.k1 + tf_d));
                *accum.entry(posting.doc).or_insert(0.0) += contribution;
            }
        }

        let mut hits: Vec<ScoredHit> = accum
            .into_iter()
            .filter(|(_, score)| *score > 0.0)
            .map(|(doc, score)| ScoredHit {
                doc_id: self.doc_ids[doc as usize].clone(),
                score,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        hits.truncate(top_k);
        hits
    }

    fn weighted_tf(&self, posting: &Posting) -> f32 {
        let doc_idx = posting.doc as usize;
        let mut tf_d = 0.0;
        for ft in &posting.fields {
            let fid = ft.field as usize;
            let Some(field_params) = self.options.fields.get(fid) else {
                continue;
            };
            let avg_dl = self.avg_field_len.get(fid).copied().unwrap_or(0.0);
            let dl = self
                .field_lens
                .get(doc_idx)
                .and_then(|v| v.get(fid))
                .copied()
                .unwrap_or(0) as f32;
            let denom = if avg_dl > 0.0 {
                1.0 - field_params.b + field_params.b * (dl / avg_dl)
            } else {
                1.0
            };
            if denom <= 0.0 {
                continue;
            }
            tf_d += field_params.weight * (ft.tf as f32) / denom;
        }
        tf_d
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self)?;
        let tmp = path.with_extension("idx.tmp");
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        let index: Index = bincode::deserialize(&bytes)?;
        if index.schema_version != SCHEMA_VERSION {
            return Err(IndexError::SchemaMismatch {
                expected: SCHEMA_VERSION,
                actual: index.schema_version,
            });
        }
        Ok(index)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredHit {
    pub doc_id: String,
    pub score: f32,
}

/// Builder that accumulates documents and finalizes into a queryable `Index`.
pub struct IndexBuilder {
    options: BuildOptions,
    doc_ids: Vec<String>,
    field_lens: Vec<Vec<u32>>,
    /// `term -> doc_idx -> field_id -> tf`. Flattened at build time.
    term_docs: HashMap<String, HashMap<u32, HashMap<u8, u32>>>,
}

impl IndexBuilder {
    pub fn new(options: BuildOptions) -> Self {
        Self {
            options,
            doc_ids: Vec::new(),
            field_lens: Vec::new(),
            term_docs: HashMap::new(),
        }
    }

    /// Add a document. `fields` is a slice of `(field_name, text)` pairs;
    /// missing fields are treated as empty. Field names must match the names
    /// declared in `BuildOptions::fields` — unknown names are dropped.
    pub fn add_document(&mut self, doc_id: impl Into<String>, fields: &[(&str, &str)]) {
        let doc_idx_usize = self.doc_ids.len();
        let doc_idx = u32::try_from(doc_idx_usize).expect("doc count exceeds u32");
        self.doc_ids.push(doc_id.into());
        let mut lens = vec![0u32; self.options.field_count()];

        for (name, text) in fields {
            let Some(fid) = self.options.field_id(name) else {
                continue;
            };
            let tokens = tokenize(text);
            lens[fid as usize] = tokens.len() as u32;
            for tok in tokens {
                let by_doc = self.term_docs.entry(tok).or_default();
                let by_field = by_doc.entry(doc_idx).or_default();
                *by_field.entry(fid).or_insert(0) += 1;
            }
        }
        self.field_lens.push(lens);
    }

    pub fn build(self) -> Index {
        let field_count = self.options.field_count();
        let n_docs = self.doc_ids.len();
        let mut total_field_len = vec![0u64; field_count];
        for lens in &self.field_lens {
            for (fid, len) in lens.iter().enumerate().take(field_count) {
                total_field_len[fid] += *len as u64;
            }
        }
        let avg_field_len: Vec<f32> = total_field_len
            .into_iter()
            .map(|total| {
                if n_docs == 0 {
                    0.0
                } else {
                    (total as f64 / n_docs as f64) as f32
                }
            })
            .collect();

        let mut postings: HashMap<String, Vec<Posting>> = HashMap::new();
        for (term, by_doc) in self.term_docs {
            let mut docs: Vec<Posting> = by_doc
                .into_iter()
                .map(|(doc, by_field)| {
                    let mut fields: Vec<FieldTerm> = by_field
                        .into_iter()
                        .map(|(field, tf)| FieldTerm { field, tf })
                        .collect();
                    fields.sort_by_key(|f| f.field);
                    Posting { doc, fields }
                })
                .collect();
            docs.sort_by_key(|p| p.doc);
            postings.insert(term, docs);
        }

        Index {
            schema_version: SCHEMA_VERSION,
            options: self.options,
            doc_ids: self.doc_ids,
            field_lens: self.field_lens,
            avg_field_len,
            postings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::paperseed_defaults;

    fn build_small_index() -> Index {
        let mut b = IndexBuilder::new(paperseed_defaults());
        b.add_document(
            "doc-a",
            &[
                ("title", "Content-Defined Chunking for Deduplication"),
                ("authors", "Alice"),
                ("abstract", "Rabin fingerprinting for variable-size blocks"),
            ],
        );
        b.add_document(
            "doc-b",
            &[
                ("title", "Survey of Large Language Model Architectures"),
                ("authors", "Bob"),
                (
                    "abstract",
                    "Transformer-based language models for natural language tasks",
                ),
            ],
        );
        b.add_document(
            "doc-c",
            &[
                ("title", "Async Extraction Pipelines for Storage Systems"),
                ("authors", "Carol"),
                (
                    "abstract",
                    "Pipeline design for content addressable storage and deduplication",
                ),
            ],
        );
        b.build()
    }

    #[test]
    fn empty_query_returns_no_hits() {
        let idx = build_small_index();
        assert!(idx.search("", 5).is_empty());
        assert!(idx.search("   !!!  ", 5).is_empty());
    }

    #[test]
    fn term_only_in_one_doc_ranks_that_doc_first() {
        let idx = build_small_index();
        let hits = idx.search("rabin fingerprinting", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].doc_id, "doc-a");
    }

    #[test]
    fn unrelated_query_does_not_pull_in_unrelated_doc() {
        let idx = build_small_index();
        let hits = idx.search("transformer", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "doc-b");
    }

    #[test]
    fn cdc_query_does_not_overweight_llm_doc() {
        // Regression target: the user's specific complaint. A CDC/dedupe query
        // should not surface the LLM survey doc (doc-b) above the topical CDC
        // doc (doc-a) just because LLM content happens to be in the corpus.
        let idx = build_small_index();
        let hits = idx.search("content-defined chunking deduplication", 5);
        assert!(!hits.is_empty());
        assert!(
            hits.iter().any(|h| h.doc_id == "doc-a"),
            "topical doc should match"
        );
        if let Some(top) = hits.first() {
            assert_ne!(
                top.doc_id, "doc-b",
                "LLM survey should not outrank the CDC doc on a CDC query"
            );
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corpus.idx.bin");

        let idx = build_small_index();
        idx.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_ne!(
            bytes.first(),
            Some(&b'{'),
            "index should use binary encoding"
        );
        let loaded = Index::load(&path).unwrap();
        assert_eq!(loaded.doc_count(), idx.doc_count());

        let a = idx.search("deduplication", 5);
        let b = loaded.search("deduplication", 5);
        assert_eq!(a, b);
    }

    #[test]
    fn top_k_truncates_results() {
        let idx = build_small_index();
        let hits = idx.search("storage", 1);
        assert!(hits.len() <= 1);
    }

    #[test]
    fn deterministic_score_ordering_for_ties() {
        let mut b = IndexBuilder::new(paperseed_defaults());
        // Two identical documents (different IDs) get identical scores. Ordering
        // must be stable by doc_id for determinism.
        b.add_document("z-doc", &[("title", "alpha beta gamma")]);
        b.add_document("a-doc", &[("title", "alpha beta gamma")]);
        let idx = b.build();
        let hits = idx.search("alpha", 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].score, hits[1].score);
        assert_eq!(hits[0].doc_id, "a-doc"); // tie-break by doc_id ascending
    }

    #[test]
    fn incremental_upsert_replaces_and_adds_documents() {
        let mut index = build_small_index();
        index.upsert_document("doc-a", &[("title", "Completely Replaced Quantum Topic")]);
        assert!(
            index
                .search("deduplication", 5)
                .iter()
                .all(|hit| hit.doc_id != "doc-a")
        );
        assert_eq!(index.search("quantum", 5)[0].doc_id, "doc-a");

        index.upsert_document("doc-d", &[("title", "New Incremental Paper")]);
        assert_eq!(index.doc_count(), 4);
        assert_eq!(index.search("incremental", 5)[0].doc_id, "doc-d");
    }
}
