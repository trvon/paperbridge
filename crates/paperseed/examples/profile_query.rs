//! Time each phase of a corpus query against a real corpus.
//!
//! cargo run --profile profiling -p paperseed --example profile_query -- \
//!     ~/.local/share/paperbridge/paperseed "attention is all you need" [runs]
//!
//! YAMS is not consulted; this measures the local BM25F path only.

use paperseed::app::{CorpusPaths, DEFAULT_QUERY_TOP_K};
use paperseed::db::CorpusDb;
use paperseed::indexing;
use paperseed_index::Index;
use std::time::{Duration, Instant};

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let out = f();
    eprintln!("{label:<28} {:>10.3} ms", ms(started.elapsed()));
    out
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1_000.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .ok_or("usage: profile_query <corpus-root> <query> [runs]")?;
    let query = args.next().ok_or("missing query")?;
    let runs: usize = args.next().map_or(Ok(3), |r| r.parse())?;
    let paths = CorpusPaths::new(root);

    let db = timed("corpus.json load", || CorpusDb::load(&paths.db_path))?;
    eprintln!("  {} papers", db.papers.len());
    let index = timed("index load (decode+validate)", || {
        Index::load(&paths.index_path)
    })?;
    eprintln!("  {} docs", index.doc_count());
    let hits = timed("index.search", || index.search(&query, DEFAULT_QUERY_TOP_K));
    eprintln!("  {} hits", hits.len());
    timed("map hits via db.get", || {
        hits.iter()
            .filter_map(|hit| db.get(&hit.doc_id).ok().flatten().cloned())
            .count()
    });
    for run in 1..=runs {
        timed(&format!("search_scored run {run}"), || {
            indexing::search_scored(&db, &paths.index_path, &query, DEFAULT_QUERY_TOP_K)
        })?;
    }
    timed("build_index (full rebuild)", || indexing::build_index(&db))?;
    Ok(())
}
