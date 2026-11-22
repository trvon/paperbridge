//! Citation store for paperbridge (`citegraph.sqlite`).
//!
//! Design: `docs/design/citation-graph.md` ("Data model", "Identity",
//! "Freshness", "Concurrency"). Concurrency is modelled by
//! `formal/tla/MergeFetchDecide.tla`.
//!
//! The API is synchronous and never exposes the store's internal work ids:
//! callers speak in normalized external ids ([`ExtId`]). Every write is one
//! `BEGIN IMMEDIATE` transaction that resolves external ids itself, so no
//! caller can carry a stale resolution across an HTTP await.

pub mod ids;
mod migrate;
mod store;
mod time;
mod uf;

pub use ids::{ExtId, Scheme, StrongIds};
pub use store::{
    CommitOutcome, Direction, EdgeSource, FetchKey, FetchState, MergeInfo, Neighbor, NeighborPage,
    NeighborQuery, Page, PageRow, RateGrant, RateLimit, Store, WorkMeta, WorkRecord,
};

/// Schema version this build writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: u32 = migrate::LATEST;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("citation store error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("citation store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "citation store schema version {found} is newer than this build supports ({supported}); upgrade paperbridge or move the store aside"
    )]
    NewerSchema { found: u32, supported: u32 },
    #[error("citation store is inconsistent: {0}")]
    Corrupt(String),
    #[error("no fetch generation {generation} for {subject} {direction}/{origin}")]
    NoFetch {
        subject: String,
        direction: &'static str,
        origin: String,
        generation: i64,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
