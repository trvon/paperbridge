//! BM25F index over the paperseed local paper corpus.
//!
//! Build once, query many. Persisted alongside `corpus.json`. Replaces the
//! prior naive substring-count scorer in `paperseed::db`.

pub mod error;
pub mod index;
pub mod params;
pub mod tokenizer;

pub use error::{IndexError, Result};
pub use index::{Index, IndexBuilder, ScoredHit};
pub use params::{BuildOptions, FieldParams, paperseed_defaults};
pub use tokenizer::tokenize;
